# Rust fallback optimization: review and measurements

The tables below record the initial PR revision `6dce496`. The subsequent
[round 13 report](rust-fallback-round13.md) measures further changes directly
against that PR revision, with fresh validation. Use that report for the latest
incremental results; the independently measured speedups should not be multiplied.

This branch reduces repeated work in the Rust fallback while preserving complete
terminal output and callback ordering. It is based on current upstream master
`702b630512e76dc8edcbba47cf948e857bed8d77` (v0.4.0). The primary comparison below
uses that commit's Rust fallback, including its post-assembly-port improvements.
It does not use the older Rust-only revision or multiply gains from experiments.

The assembly engine, bridge, dispatcher, default feature, and build script remain
unchanged. `TTFX_ASM=0` selects the optimized Rust path; normal engine selection
still applies. See [fallback examples](rust-fallback-dispatch.md).

## Initial PR gain over upstream Rust (6dce496)

Normal portable release builds, **without PGO or `target-cpu=native`**: the final
Rust fallback is **1.765× faster** by geometric mean across all 37 effects;
the median effect speedup is **1.658×**. All 37 complete streams match upstream
Rust and forced assembly. Of these effects, 36 improve and Overflow regresses.

The benchmark uses a Ryzen 5 7600X, Rust 1.98.1 / LLVM 22.1.8, CPU affinity 2,
the supplied 190×46 ASCII input on a 200×50 canvas, seed 1, virtual time, pacing
off, and stdout to `/dev/null`. Seven measured repetitions follow one warmup,
with shuffled engine order. Timings include startup, setup, and the full animation.
Complete stdout hashes/lengths, stderr, and exit status are checked before timing.
Builds, tests, profiling, and memory measurements run separately from timing.
These numbers measure engine throughput rather than physical-terminal frame rate.

| Effect | Upstream Rust ms | PR Rust ms | Speedup |
| --- | ---: | ---: | ---: |
| beams | 204.22 | 103.77 | 1.968× |
| binarypath | 780.32 | 604.04 | 1.292× |
| blackhole | 409.43 | 288.39 | 1.420× |
| bouncyballs | 325.94 | 140.53 | 2.319× |
| bubbles | 487.27 | 266.42 | 1.829× |
| burn | 328.58 | 186.63 | 1.761× |
| colorshift | 337.66 | 149.65 | 2.256× |
| crumble | 295.21 | 208.41 | 1.417× |
| decrypt | 406.48 | 131.34 | 3.095× |
| errorcorrect | 259.99 | 111.52 | 2.331× |
| expand | 115.03 | 103.96 | 1.106× |
| fireworks | 332.58 | 284.13 | 1.170× |
| highlight | 49.64 | 29.48 | 1.684× |
| laseretch | 502.89 | 279.79 | 1.797× |
| matrix | 187.25 | 133.34 | 1.404× |
| middleout | 90.23 | 66.04 | 1.366× |
| orbittingvolley | 95.37 | 76.74 | 1.243× |
| overflow | 122.11 | 133.56 | 0.914× |
| pour | 228.58 | 95.89 | 2.384× |
| print | 288.67 | 41.70 | 6.922× |
| rain | 222.89 | 117.83 | 1.892× |
| randomsequence | 53.38 | 28.93 | 1.845× |
| rings | 670.99 | 453.11 | 1.481× |
| scattered | 137.34 | 116.80 | 1.176× |
| slice | 68.63 | 49.96 | 1.374× |
| slide | 93.65 | 61.13 | 1.532× |
| smoke | 168.33 | 89.66 | 1.877× |
| spotlights | 287.89 | 172.16 | 1.672× |
| spray | 167.44 | 107.52 | 1.557× |
| swarm | 589.79 | 490.29 | 1.203× |
| sweep | 59.16 | 40.07 | 1.476× |
| synthgrid | 107.20 | 53.84 | 1.991× |
| thunderstorm | 339.24 | 77.50 | 4.377× |
| unstable | 179.51 | 150.03 | 1.196× |
| vhstape | 209.49 | 136.63 | 1.533× |
| waves | 444.38 | 85.13 | 5.220× |
| wipe | 50.90 | 30.69 | 1.658× |

At `6dce496`, Overflow took 122.11 → 133.56 ms in this sweep: **9.4% longer**.
This was a known per-effect regression in the initial draft.
Assembly still leads the candidate Rust implementation by **3.815×** in geometric-mean
elapsed time. Matching the assembly engine across the suite has not been achieved.

Full evidence: [raw samples and output checksums](benchmarks/rust-fallback/pr/upstream-vs-final.json),
[readable run log](benchmarks/rust-fallback/pr/upstream-vs-final.log),
[summary](benchmarks/rust-fallback/pr/summary.json), and
[exact benchmark input](benchmarks/rust-fallback/pr/input.txt).

The independent 21-pair recheck confirms Overflow at 123.35 → 135.25 ms
(**9.6% longer**, 0.912×). See the
[recheck samples](benchmarks/rust-fallback/pr/overflow-recheck.json).

Median peak resident memory from three separate complete runs per mode:

| Effect | Upstream Rust MiB | PR Rust MiB | Assembly MiB |
| --- | ---: | ---: | ---: |
| binarypath | 276.94 | 217.59 | 222.57 |
| rings | 354.16 | 181.70 | 56.73 |
| colorshift | 103.48 | 58.07 | 27.52 |
| waves | 115.13 | 37.43 | 33.86 |
| spotlights | 20.00 | 20.00 | 41.66 |

These are process high-water marks measured with Linux `wait4`, not live
allocation counts. Cumulative sharing reduces memory in several large workloads,
even though the latest persistent-motion round adds storage relative to its
immediate predecessor. See [raw RSS samples](benchmarks/rust-fallback/pr/memory.json).

## What worked and is retained

- **Avoiding repeated rendering work.** Track changed characters and rows, switch
  between sparse and dense rendering according to workload, compare rows with a
  runtime-guarded AVX2 kernel, and reuse serialized bytes for unchanged frames.
  Every required frame is still emitted. Reuse equal visuals and shared immutable
  frame programs instead of rebuilding strings and frames.
- **Separating definitions from playback.** Persistent compact scene cursors and
  callback-aware held-frame scheduling avoid traversing scene objects just to
  advance counters. Exact easing tables have bounded admission and storage;
  unusual shapes retain ordinary stepping. Waves constructs shared programs once.
- **Preparing reusable motion.** Event-free stepping avoids repeated lookups;
  persistent motion cursors and prepared line/quadratic geometry remove more
  repeated traversal. A guarded SSE4.1 kernel computes both coordinates with
  unchanged operation order and rounding. Public reads materialize counters;
  writes invalidate prepared state. Completion, observed events, and reentrant
  callbacks retain their established order. Integer-prefix eligibility and
  overshoot handling preserve floating-point results.
- **Sharing construction work.** Binarypath/Rings reuse waypoint definitions;
  Rings shares immutable event chains with copy-on-write mutation. Each character
  still owns its live progress and event state. Numeric scene handles validate
  cached map slots before use.
- **Reducing Spotlights work.** Exact integer-offset distance caching, dense color
  indexing, and skipping illumination when all relevant inputs are unchanged
  reduce repeated distance, lookup, and appearance work. There is no approximate
  brightness or geometry. Input-map and arena mutation invalidate cached lighting.
- **Keeping SIMD small and optional.** Runtime feature detection selects AVX2 row
  comparison and SSE4.1 point evaluation. Scalar alternatives remain available;
  normal Rust containers own all allocations. No inline assembly or manual
  allocator was added. Optional PGO tooling is provided separately from normal
  release builds and checks the compiler/LLVM tool versions.

## What was tried and rejected

These trials compare each prototype with its immediate predecessor. Their ratios
are independent experiments, not components to multiply into a total speedup.
All rejected production changes were removed before final validation.

| Experiment | Observed result | Decision |
| --- | --- | --- |
| Cumulative integer-distance motion index | Binarypath 763.72 → 915.81 ms, about 20% longer | Reject the index traversal |
| Four-lane AVX2 with per-tick gathering | Six-effect median 0.951×; Binarypath 0.905× | Gathering from scattered objects erased arithmetic savings |
| Gated version of the motion gather | Median 0.996×; Rings 0.965× | Reject; use persistent prepared storage instead |
| Unchecked dense-render indexing | About 1.007× on the longer recheck; 1.004× geometric mean against an equivalent checked refactor | Benefit too small to justify the extra unsafe indexing |
| Brightness memoization plus appearance prechecks/forced inlining | Isolated Spotlights gains, but suite-wide regressions; outlined cache 0.997× overall | Remove these caches/prechecks; retain the separately tested unchanged-illumination skip |
| Compact mirrored render records | Six-effect median 1.000× in an isolated 11-pair comparison | Extra storage/invalidation had no reliable benefit |

Evidence: [motion index](benchmarks/rust-fallback/pr/rejected-motion-index.json),
[SIMD gather](benchmarks/rust-fallback/pr/rejected-motion-gather.json),
[gated gather](benchmarks/rust-fallback/pr/rejected-motion-gated.json),
[unchecked renderer recheck](benchmarks/rust-fallback/pr/rejected-render-unchecked.json),
[checked renderer control](benchmarks/rust-fallback/pr/rejected-render-control.json),
[brightness cache](benchmarks/rust-fallback/pr/rejected-spotlights-cache.json),
[forced inlining](benchmarks/rust-fallback/pr/rejected-appearance-inline.json), and
[render records](benchmarks/rust-fallback/pr/rejected-render-records.json).

## Latest round versus the preceding optimized Rust build

The final motion/mutation/lighting round adds **1.035×** geometric-mean speedup
in ordinary release and **1.042×** in a PGO-versus-PGO comparison across 37
effects. These are incremental results against an already optimized intermediate
build, not the cumulative gain over current upstream Rust.

Twenty-one-pair PGO rechecks show Binarypath **1.212×**, Rings **1.092×**, and
Spotlights **1.100×**. Tradeoffs remain: in ordinary release this round makes
Pour 8.4% slower, Bouncyballs 3.9% slower, and Unstable about 3.7% slower than the
preceding optimized build. PGO reverses the Pour/Bouncyballs regression but leaves
Unstable about 2.9% slower. A regression relative to an intermediate build does
not necessarily mean a regression relative to upstream; use the primary table
for the latter.

Binarypath's peak RSS grows about 9.7 MiB (207.1 → 216.8 MiB) in this last round;
Rings grows about 1.4 MiB. Separate hardware counters show fewer instructions in
all five measured effects, yet Bouncyballs and Pour use more cycles without PGO.
Instruction count alone therefore does not guarantee lower elapsed time.

Evidence: [ordinary release](benchmarks/rust-fallback/pr/round11-normal.json),
[PGO](benchmarks/rust-fallback/pr/round11-pgo.json),
[ordinary recheck](benchmarks/rust-fallback/pr/round11-recheck-normal.json),
[PGO recheck](benchmarks/rust-fallback/pr/round11-recheck-pgo.json),
[memory](benchmarks/rust-fallback/pr/round11-memory.json), and
[hardware counters](benchmarks/rust-fallback/pr/round11-counters.json).

## Compatibility and validation

This is a substantial Rust engine refactor. CLI flags, deterministic output, RNG
ordering, frame emission, and callback/event behavior are the compatibility
targets. Direct Rust library consumers need adaptation: `FrameOutput`,
`CharacterArena`, `FrameStorage`/scene cursor accessors, `MapHandle`, `Waypoints`,
path-progress accessors, and `InputCoordinateMap` replace some prior return types,
public fields, and containers. Assigning an owned waypoint vector or input map
requires `.into()`; counter mutation uses mutable setters to invalidate caches.

Validation at revision `6dce496`:

- `TTFX_ASM=0 ./bin/test`: 98 release tests, 19 CLI cases, 354 Python-reference
  parity cases, 41 complete terminal-stream comparisons, and signal/close/resize
  checks passed.
- No-assembly release build: 77 tests passed (21 assembly differential tests are
  omitted). Separate scalar-motion testing: 47 tests passed.
- 436 additional candidate comparisons across 133 complete scenarios exercise
  Unicode, color handling, Spotlights variants, and disabled caches/runtimes.
- New differential tests cover mutations, map replacement, arena cloning/growth,
  callback observations before/after the update cursor, nested updates, completion,
  all named easing functions, overshoot, large coordinates, and scalar/SIMD limits.
- Fresh PGO binary: 19 CLI cases, 354 parity cases, and 41 terminal streams passed.
  PGO trains on 111 cases spanning 37 effects with inputs/seeds distinct from the
  main timing workload. It remains an optional build step.

Logs and manifests: [full suite](benchmarks/rust-fallback/pr/validation-full.log),
[no assembly](benchmarks/rust-fallback/pr/validation-no-asm.log),
[scalar motion](benchmarks/rust-fallback/pr/validation-scalar.log),
[variant comparisons](benchmarks/rust-fallback/pr/validation-variants.json),
[source/binary identities](benchmarks/rust-fallback/pr/environment.json),
[PGO build](benchmarks/rust-fallback/pr/pgo-build.json), and
[training](benchmarks/rust-fallback/pr/pgo-training.json).

The source and tests in the PR match the recorded validated hashes. Linux results
are local evidence; macOS and musl validation are left to the existing CI jobs.

## Reproduction

Build upstream `702b630` and the PR revision in separate worktrees with the same
toolchain and ordinary release flags; save the upstream binary before switching
builds. Both may use the default assembly feature. Set `NASM` if the assembler is
not on `PATH`. Use an available CPU number instead of 2 if necessary.

```sh
TTFX_ASM=0 ./bin/test
cargo test --release --no-default-features --target-dir target/no-asm
python3 tools/tests/bench_compare.py /path/to/upstream/ttfx target/release/ttfx \
  --assembly /path/to/upstream/ttfx \
  --input docs/benchmarks/rust-fallback/pr/input.txt \
  --cpu 2 --repeats 7 \
  --terminal-options='--canvas-width 200 --canvas-height 50 --ignore-terminal-dimensions' \
  --json /tmp/ttfx-upstream-comparison.json

# Optional, separate PGO build; does not modify the normal release profile.
python3 tools/tests/build_pgo.py --output-dir target/rust-pgo
```

Diagnostic controls include `TTFX_SCHEDULER=0`, `TTFX_SCENE_RUNTIME=0`,
`TTFX_MOTION_RUNTIME=0`, `TTFX_MOTION_SIMD=0`, `TTFX_LIGHTING_CACHE=0`, and
`TTFX_SIMD=0`. They support differential comparisons inside Rust; select Rust
with `TTFX_ASM=0` first. The benchmark harness enforces engine selection and
supports repeatable `--before-env` / `--after-env` overrides.
