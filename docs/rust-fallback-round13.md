# Round 13: appearance lookup and shared ColorShift construction

This validated checkpoint is **1.087× faster across all 37 effects** by geometric mean than PR revision `6dce496`, the baseline for the ongoing optimization goal. The median effect gains 4.0%, and 32 effects are faster. This **8.7% overall gain remains modest**; the goal is still active and PR #41 remains draft. The latest changes separately measure **1.016× over the preceding checkpoint `57deb79`**, with a 1.004× median effect. The older 1.765× improvement over upstream is not counted again.

Assembly remains 3.539× faster geometrically on this workload. All complete Rust/assembly outputs match before timing. Timings use ordinary portable release builds, CPU 2, seed 1, 190×46 ASCII input on a 200×50 canvas, virtual clock, no pacing, and output to `/dev/null`. Builds, correctness tests, profiling, memory measurement, and timed runs are kept separate.

## Retained changes

- Appearance palette hits borrow colors instead of copying the full value key repeatedly. Overflow constructs its row color pair once. Ordinary appearance equality checks also compare borrowed values; allocation, exact colors, and weak-reference behavior are preserved.
- ColorShift shares immutable frame sequences for matching symbols, gradient rotations, and input styling. Playback counters remain independent. The effect-local construction cache holds at most 4,096 programs and is dropped after construction. Public color-code fields and channel values are part of the key. `TTFX_COLORSHIFT_PROGRAM_CACHE=0` bypasses sharing for comparisons.
- `RgbString` no longer validates UTF-8 on every color read or hash. Its private constructors either copy an entire valid `&str` or create six ASCII hexadecimal digits. The bounded slice remains checked. This adds one small unsafe conversion with its invariant documented locally; it adds no manual allocator or CPU requirement. The [Rust safety contract](https://doc.rust-lang.org/std/str/fn.from_utf8_unchecked.html) requires the valid UTF-8 invariant.

## Whole-program results

| Effect | PR `6dce496` Rust ms | Candidate Rust ms | Speedup |
| --- | ---: | ---: | ---: |
| beams | 88.88 | 85.43 | 1.040× |
| binarypath | 571.57 | 564.12 | 1.013× |
| blackhole | 268.98 | 230.87 | 1.165× |
| bouncyballs | 121.45 | 112.50 | 1.080× |
| bubbles | 238.75 | 228.93 | 1.043× |
| burn | 191.19 | 192.58 | 0.993× |
| colorshift | 137.56 | 112.70 | 1.221× |
| crumble | 202.97 | 185.51 | 1.094× |
| decrypt | 136.61 | 136.82 | 0.998× |
| errorcorrect | 104.74 | 102.80 | 1.019× |
| expand | 99.58 | 81.00 | 1.229× |
| fireworks | 258.73 | 210.81 | 1.227× |
| highlight | 26.74 | 26.21 | 1.020× |
| laseretch | 255.19 | 243.39 | 1.049× |
| matrix | 104.73 | 88.64 | 1.182× |
| middleout | 59.56 | 58.90 | 1.011× |
| orbittingvolley | 69.33 | 67.13 | 1.033× |
| overflow | 118.66 | 65.69 | 1.806× |
| pour | 82.39 | 73.68 | 1.118× |
| print | 38.67 | 37.72 | 1.025× |
| rain | 100.32 | 92.42 | 1.085× |
| randomsequence | 25.39 | 24.23 | 1.048× |
| rings | 421.54 | 425.12 | 0.992× |
| scattered | 101.06 | 84.90 | 1.190× |
| slice | 44.49 | 42.80 | 1.040× |
| slide | 54.12 | 53.80 | 1.006× |
| smoke | 75.05 | 73.21 | 1.025× |
| spotlights | 152.89 | 101.85 | 1.501× |
| spray | 94.26 | 91.91 | 1.026× |
| swarm | 419.00 | 369.62 | 1.134× |
| sweep | 34.51 | 33.43 | 1.032× |
| synthgrid | 46.82 | 47.65 | 0.983× |
| thunderstorm | 72.29 | 69.36 | 1.042× |
| unstable | 148.14 | 146.08 | 1.014× |
| vhstape | 129.10 | 133.12 | 0.970× |
| waves | 79.44 | 76.50 | 1.038× |
| wipe | 28.18 | 26.79 | 1.052× |

VHStape takes 3.1% longer in this sweep, Synthgrid 1.8% longer, and Rings 0.8% longer. These are measured tradeoffs, not a claim that every effect improves. Raw samples and complete-output hashes are in [final.json](benchmarks/rust-fallback/round13/final.json). The separate [37-effect checkpoint comparison](benchmarks/rust-fallback/round13/checkpoint.json) measures this round alone.

## Longer rechecks

Twenty-one paired repetitions confirm the main targeted gains. Colorshift now improves over the original PR baseline; VHStape still takes 1.6% longer. Rings and Synthgrid are effectively unchanged in this recheck.

| Effect | PR `6dce496` Rust ms | Candidate Rust ms | Speedup |
| --- | ---: | ---: | ---: |
| colorshift | 139.13 | 113.85 | 1.222× |
| overflow | 119.09 | 67.58 | 1.762× |
| spotlights | 150.81 | 102.89 | 1.466× |
| vhstape | 133.13 | 135.27 | 0.984× |
| rings | 435.79 | 436.73 | 0.998× |
| synthgrid | 44.66 | 44.63 | 1.001× |

[All recheck samples](benchmarks/rust-fallback/round13/recheck.json).

## Peak memory

Three separate complete runs per effect/mode, measured with Linux `wait4`. Values are medians of process peak RSS, not live allocation totals. ColorShift drops about 4.1 MiB relative to the preceding checkpoint. Other measured changes are within about 1 MiB of that checkpoint; they are recorded below rather than assumed to be zero.

| Effect | Goal-start Rust MiB | Prior checkpoint MiB | Candidate MiB |
| --- | ---: | ---: | ---: | ---: |
| binarypath | 215.86 | 217.94 | 217.79 |
| rings | 180.78 | 181.38 | 181.79 |
| colorshift | 58.81 | 59.85 | 55.71 |
| waves | 36.62 | 36.62 | 37.38 |
| spotlights | 19.84 | 19.84 | 19.84 |
| matrix | 28.24 | 25.89 | 26.71 |
| overflow | 43.05 | 36.82 | 37.77 |
| fireworks | 51.89 | 52.39 | 53.19 |

[Raw memory samples](benchmarks/rust-fallback/round13/memory.json).

## Validation

- 106 release tests passed with NASM supplied explicitly, including 21 assembly differential tests.
- 85 release tests passed with `--no-default-features`.
- 19 CLI cases, 354 Python-reference parity cases, 41 complete terminal streams, and signal, terminal-close, and resize checks passed.
- 844 additional complete-output comparisons cover 277 scenarios: varied input/color modes, palette and runtime switches, and 120 ColorShift option combinations in three candidate modes.
- New tests check complete UTF-8 construction at every supported byte length, borrowed string hashing, all xterm colors, and independent playback/copy-on-write edits of shared ColorShift programs. Existing RGB tests cover 65,536 generated color combinations.
- The restored release binary has the same SHA-256 as the fully tested checkpoint; source hashes are in [environment.json](benchmarks/rust-fallback/round13/environment.json).

## Experiments not retained

| Experiment | Isolated result | Decision |
| --- | --- | --- |
| Prepared arena-only tick loop | 1.014× geometric mean over eight effects | Too little improvement for another update path |
| Cached per-cell formatted glyph bytes | 0.976× geometric mean over ten effects | Remove; extra copying slowed several effects |
| Shared exact motion-easing factors | 1.007× geometric mean, 0.997× median over nine effects | Remove; Binarypath and Rings regressed |
| Reserve once per row and copy into spare capacity | 1.010× geometric mean, 1.013× median over eleven effects | Remove; small mixed gains did not justify additional unsafe rendering code |

The retained UTF-8, borrowed-key, and ColorShift experiments were evaluated incrementally; their ratios must not be multiplied into the whole-program result. The ColorShift-only construction comparison measured 1.286× for that effect, while the final combined result versus `6dce496` is 1.221×. These use different baselines. The glyph cache, motion factors, and reserved-row writer are absent from retained production code.

The next substantial investigation is grouping equivalent stationary scene progress, to avoid advancing and scheduling identical counters once per character. That refactor is not implemented or included in these measurements.
