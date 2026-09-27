# Further Rust fallback optimization after PR #41

This round measures changes against the existing PR head `6dce496`, rather than
claiming its earlier gains over upstream a second time. Upstream master remains
`702b630`. Assembly sources, dispatch, build requirements, and the default feature
are unchanged. All retained implementation changes use safe Rust.

The retained ordinary release build is **1.069× faster by geometric mean**
across all 37 effects than the existing PR revision. The median effect speedup
is 1.030×, and the ratio of summed median times is 1.075×. Thirty effects
improve in this sweep. The largest gains are Overflow (1.51×), Spotlights
(1.29×), Fireworks (1.24×), and Expand/Scattered (1.20×).

The **6.9% overall gain remains modest** compared with the targeted gains.
Further optimization and the per-effect regressions remain under investigation;
the PR stays in draft. Assembly still leads by 3.588× geometrically.
No PGO, BOLT, native-CPU flags, or assembly execution contributed to these Rust
timings. The prior 1.765× gain over upstream belongs to the initial PR and is
not counted as a new result here.

| Effect | PR 6dce496 Rust ms | New Rust ms | Speedup |
| --- | ---: | ---: | ---: |
| beams | 101.13 | 99.56 | 1.016× |
| binarypath | 583.11 | 566.10 | 1.030× |
| blackhole | 288.85 | 250.14 | 1.155× |
| bouncyballs | 135.62 | 122.10 | 1.111× |
| bubbles | 259.68 | 246.11 | 1.055× |
| burn | 184.57 | 184.64 | 1.000× |
| colorshift | 145.69 | 145.04 | 1.005× |
| crumble | 196.23 | 174.58 | 1.124× |
| decrypt | 125.98 | 129.19 | 0.975× |
| errorcorrect | 107.74 | 105.47 | 1.022× |
| expand | 100.59 | 83.74 | 1.201× |
| fireworks | 269.30 | 217.70 | 1.237× |
| highlight | 27.93 | 26.80 | 1.042× |
| laseretch | 273.89 | 262.92 | 1.042× |
| matrix | 127.58 | 109.37 | 1.167× |
| middleout | 61.32 | 61.92 | 0.990× |
| orbittingvolley | 74.91 | 71.60 | 1.046× |
| overflow | 128.65 | 84.98 | 1.514× |
| pour | 92.62 | 81.96 | 1.130× |
| print | 39.87 | 38.94 | 1.024× |
| rain | 110.48 | 101.95 | 1.084× |
| randomsequence | 27.55 | 26.46 | 1.041× |
| rings | 430.59 | 433.81 | 0.993× |
| scattered | 110.81 | 92.25 | 1.201× |
| slice | 57.31 | 57.18 | 1.002× |
| slide | 67.35 | 68.40 | 0.985× |
| smoke | 84.87 | 83.81 | 1.013× |
| spotlights | 173.06 | 133.93 | 1.292× |
| spray | 102.90 | 101.25 | 1.016× |
| swarm | 469.14 | 414.68 | 1.131× |
| sweep | 38.52 | 37.57 | 1.025× |
| synthgrid | 53.12 | 52.88 | 1.005× |
| thunderstorm | 74.11 | 74.56 | 0.994× |
| unstable | 144.45 | 143.37 | 1.008× |
| vhstape | 131.79 | 135.45 | 0.973× |
| waves | 81.23 | 76.45 | 1.062× |
| wipe | 27.96 | 27.81 | 1.005× |

VHStape and Decrypt take about 2.8% and 2.6% longer in this sweep; some other
effects are close to unchanged. Individual gains do not imply that every
workload improves. Raw [samples and complete-output checksums](benchmarks/rust-fallback/round12/final.json)
and the [computed summary](benchmarks/rust-fallback/round12/summary.json) identify
the exact baseline and candidate.

An independent 21-pair recheck confirms the targeted gains and exposes a
Colorshift regression that the full sweep did not show. It takes **4.4% longer**
in this recheck; VHStape takes **1.6% longer**. These tradeoffs remain open.

| Recheck | PR Rust ms | New Rust ms | Speedup |
| --- | ---: | ---: | ---: |
| overflow | 130.22 | 85.42 | 1.524× |
| spotlights | 168.09 | 127.85 | 1.315× |
| matrix | 128.23 | 110.76 | 1.158× |
| fireworks | 272.73 | 223.94 | 1.218× |
| swarm | 468.78 | 408.76 | 1.147× |
| colorshift | 139.98 | 146.18 | 0.958× |
| vhstape | 130.99 | 133.07 | 0.984× |

See [all recheck samples](benchmarks/rust-fallback/round12/recheck.json).

Peak resident memory was measured separately with Linux `wait4`, three
complete runs per effect/mode. These are process high-water marks, rather than
live allocation counts. The largest measured increase is about 0.9 MiB for
Binarypath; Matrix and Overflow use less memory with effect-local visual reuse.

| Effect | PR Rust MiB | New Rust MiB |
| --- | ---: | ---: |
| binarypath | 216.72 | 217.65 |
| rings | 181.22 | 181.80 |
| colorshift | 58.21 | 58.73 |
| waves | 37.40 | 36.53 |
| spotlights | 19.85 | 19.85 |
| matrix | 29.34 | 25.90 |
| overflow | 42.91 | 36.78 |
| fireworks | 53.16 | 52.34 |

See [raw memory samples](benchmarks/rust-fallback/round12/memory.json).


## Retained changes

- Matrix and Overflow own bounded, 4,096-entry appearance palettes. Repeated
  symbols/colors reuse immutable visuals and ANSI strings. Collisions replace
  entries after an exact key comparison; no colors are approximated. Keys check
  both color constructor identity and the publicly mutable RGB/xterm fields.
  The palettes live with their effect, while ordinary `Animation::set_appearance`
  preserves its existing allocation reuse and weak-observer behavior.
- Compact input-coordinate maps build a bounded, lazy dense index. Spotlights
  clips its exact column-major ellipse traversal to populated input bounds,
  avoiding off-canvas lookups. Sparse or extreme coordinate domains retain the
  hash map. Mutable map access invalidates the index.
- Appearance-only changes preserve motion preparation and cell layout. Invisible
  movement still updates public coordinates, without dirtying the renderer.
  Stationary animation updates reuse cell membership and painter order.
- Renderer membership fields are stored together. Moving a hidden member of an
  overlapping cell or changing its color does not rescan all its occupants.
  Losing the winning character or changing painter rank still recomputes the
  winner. Dense rebuilds remain available when many characters change.
- Prepared scene playback handles exact motion-synced frame selection and
  unobserved looping scenes with active motion. Loop wraps preserve public
  counters. Completion subscribers, tracing, path completion, mutations, and
  unsupported cases retain the ordinary event path. Loop-only characters retain
  the existing inactive behavior. Active runtime bitmaps also avoid repeated
  activity lookups during pruning.

`TTFX_APPEARANCE_CACHE=0` disables the new effect palettes for comparison. Existing
scene/motion/runtime/scalar controls continue to work. No new public API migration
is required beyond the changes already described in the initial PR report.

## Correctness and measurement

The exact retained sources passed:

- 104 release tests with NASM present, including 21 assembly differential tests.
- 83 release tests with `--no-default-features`, where assembly is absent.
- 19 CLI cases, 354 Python-reference parity cases, 41 complete terminal-stream
  comparisons, and signal, terminal-close, and resize checks.
- 484 additional complete-output comparisons across 157 scenarios, including
  ASCII/Unicode/ANSI input, color modes, Spotlight geometry, Matrix/Overflow
  palettes disabled, scalar motion, and disabled scene/motion/scheduler paths.

New tests cover coordinate-index invalidation and sparse fallback, clipped
ellipses, palette eviction and independent mutations, stable render layout with
later geometry/painter changes, synced playback after public edits, looping
cursor wraps, event subscriptions, and enabling tracing. Cached-loop tests
explicitly assert that the optimized path was entered.

Timings use the same Ryzen 5 7600X and portable release configuration as the
initial PR: CPU 2, seed 1, 190×46 ASCII input on an explicit 200×50 canvas, virtual
clock, no pacing, and output to `/dev/null`. Seven measured repetitions follow
warmup, with shuffled engine order in three-way comparisons. The measurements
include process startup, construction, full animation, and byte generation.
Each case verifies complete stdout length/hash, stderr, and exit status before
timing; assembly is forced to prevent silent Rust fallback. Builds, tests,
profiling, and memory checks run separately from timed comparisons.

Evidence: [environment and source identities](benchmarks/rust-fallback/round12/environment.json),
[full suite](benchmarks/rust-fallback/round12/final-tests.log),
[no-assembly tests](benchmarks/rust-fallback/round12/no-asm-tests.log), and
[held-out comparisons](benchmarks/rust-fallback/round12/variant-parity.json).

## Experiments that did not justify retaining their changes

The following are isolated comparisons against the preceding candidate. Their
speedups must not be multiplied into the final result. Raw timing samples and
binary identities are retained alongside the report.

| Experiment | Evidence | Decision |
| --- | --- | --- |
| Boxed hot/cold character layouts | Median 0.986× in the first trial and 0.989× in the larger refactor | Remove the extra indirection/allocations |
| Chunk allocator for cold state | Median 0.975× | Remove the custom allocator and its unsafe code |
| Global strong appearance cache | Faster Overflow/Matrix, but slower Spotlights/VHStape; also conflicts with ordinary weak-observer semantics | Use effect-local ownership instead |
| Four-cell partial row serialization | Median 0.985×; Bouncyballs/Pour regressed | Keep existing row serialization |
| Longer minimum scheduler sleep | Median 1.008×, mixed per-effect outcomes | Keep existing scheduling threshold |
| Motion-only shortcut without scenes | Median 1.001× | Remove this earlier shortcut |
| Sharing color-program construction before per-frame building | Median 1.003× over 11 effects; Colorshift 1.042× but Binarypath 0.959× | Remove the construction cache |
| LLVM BOLT binary layout optimization | 1.011× geometric mean, 1.004× median over 13 effects | No new BOLT build requirement or tooling in the production build |

BOLT was tested using an unstripped, relocation-enabled Rust-only control build
and a binary optimized from that exact control. Instrumentation trained on 111
cases spanning all 37 effects and three inputs/seeds distinct from evaluation.
The experiment followed the [official LLVM BOLT guide](https://github.com/llvm/llvm-project/blob/main/bolt/README.md).
Its small gains are separate from the ordinary source-change measurements above.
See [BOLT samples](benchmarks/rust-fallback/round12/bolt.json) and
[construction-cache samples](benchmarks/rust-fallback/round12/color-program.json).

Other retained changes were evaluated incrementally, including
[cell membership](benchmarks/rust-fallback/round12/member.json),
[winner maintenance](benchmarks/rust-fallback/round12/winner.json),
[synced playback](benchmarks/rust-fallback/round12/sync.json), and
[unobserved loops](benchmarks/rust-fallback/round12/loop.json).
The final whole-program comparison is authoritative for their combined effect.
