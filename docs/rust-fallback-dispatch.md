# How the Rust fallback works

Rust always parses the command line, reads input, validates configuration, seeds
the random-number generator, and installs signal handlers. Before an animation
starts, `src/main.rs` calls `asm::try_run` to choose the engine for that run.

```text
Rust front end
    |
    +-- TTFX_ASM=0 or off ------------------------------> Rust engine
    |
    +-- offer to assembly
            |
            +-- unavailable / declines ----------------> Rust engine
            |       (TTFX_ASM=force instead exits 3)
            |
            +-- accepts ------------------------------> Assembly engine
```

A decline must happen before output is written or the caller's random-number
generator changes. The Rust engine can therefore start the entire animation from
the same input and seed. This is an alternative implementation, not a mechanism
for resuming after an assembly crash. Once assembly accepts the run, completion,
interrupts, output closure, and errors are handled as that run's outcome. A
terminal resize can cause the front end to start another run.

## Concrete cases

| Situation | Result |
| --- | --- |
| Default x86-64 Linux build with a supported linked assembly tier | Best available CPU-compatible assembly tier runs |
| `TTFX_ASM=0` or `TTFX_ASM=off` | Rust runs directly |
| `cargo build --release --no-default-features` | Binary contains no assembly engine; Rust runs |
| Non-x86-64 or non-Linux target | Build omits the assembly engine; Rust runs |
| NASM unavailable during the build | Build warns and builds without the assembly engine |
| Requested `TTFX_ASM_TIER` is unsupported by the CPU or absent from the build | Rust runs, unless assembly was forced |
| An effect cannot be marshalled or its assembly implementation declines | Rust runs, unless assembly was forced |
| Assembly reports a runtime error after accepting the run | Error is returned; the animation is not retried in Rust |
| Assembly segfaults or executes an illegal instruction | Process fails; Rust does not recover the animation |

These commands work from a built checkout. The animation is sent to `/dev/null`
so the engine-selection message on stderr is easy to see:

```sh
# Normal selection: reports the selected assembly tier, when available.
printf 'Hello\n' | TTFX_ASM_SHOW_TIER=1 target/release/ttfx --frame-rate 0 waves > /dev/null

# Explicit Rust selection: reports "Rust engine (TTFX_ASM=0)".
printf 'Hello\n' | TTFX_ASM=0 TTFX_ASM_SHOW_TIER=1 target/release/ttfx --frame-rate 0 waves > /dev/null

# Deterministically exercise a decline using an invalid tier value.
# Reports the invalid tier and runs Rust.
printf 'Hello\n' | TTFX_ASM_TIER=invalid TTFX_ASM_SHOW_TIER=1 target/release/ttfx --frame-rate 0 waves > /dev/null

# The same decline with assembly forced exits 3 instead of running Rust.
printf 'Hello\n' | TTFX_ASM=force TTFX_ASM_TIER=invalid target/release/ttfx --frame-rate 0 waves > /dev/null
```

`TTFX_ASM_SHOW_TIER` can announce a selected tier before a later effect-level
decline, so a later decline message takes precedence. A machine without AVX2
does not necessarily need the Rust engine: the assembly engine also has lower
tiers, including an SSE2 baseline.

## Optimized kernels inside Rust

The Rust fallback can contain small, CPU-specific kernels behind safe interfaces.
Our row comparator in `src/engine/render.rs` already selects AVX2 once per
terminal when it is available, and otherwise uses ordinary slice comparison.
Short rows also use the ordinary comparison.

Prepared motion in `src/engine/motion_runtime.rs` selects an SSE4.1 point kernel
on supported x86-64 CPUs. It computes both coordinates together, preserving the
scalar operation order and nearest-even rounding. Older CPUs, other
architectures, and exceptional conversions use the scalar implementation.
`TTFX_MOTION_SIMD=0` disables this point kernel. `TTFX_SIMD=0` disables both custom
SIMD kernels for differential checks; the compiler can still emit baseline SIMD.

This is a second level of selection within the Rust engine. It does not call the
full assembly engine and does not depend on NASM. Architecture-specific code is
conditionally compiled and feature-specific instructions require a matching
runtime check. Future inline-assembly kernels would need the same structure.

Unsafe code can also be kept inside small functions while `Vec` or `Box` owns
the allocation. Bounds, lifetime, alignment, initialization, and aliasing
requirements still apply. Tests can find violations but cannot prove the
contracts hold for every input. Removing a check or writing `asm!` is only an
optimization if measurements demonstrate a benefit.

References: [Rust architecture intrinsics](https://doc.rust-lang.org/core/arch/index.html),
[inline assembly contracts](https://doc.rust-lang.org/reference/inline-assembly.html#rules-for-inline-assembly),
[unsafe Rust obligations](https://doc.rust-lang.org/nomicon/what-unsafe-does.html),
and [Steve Klabnik on Rust and C performance](https://steveklabnik.com/writing/is-rust-faster-than-c/).
