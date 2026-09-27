# Round 15: patch cached output rows in place

The final portable Rust release build is **1.182× faster across all 37 effects** by geometric mean than the original PR revision `6dce496`: an **18.2% cumulative speedup**. The median effect improves by 12.7%, summed median runtime improves by 1.177×, and 34/37 effects are faster. This is a direct measurement against that original PR, not a product of earlier experiment ratios.

The row-cache change independently measures **1.088× over the preceding checkpoint `6e8c962`** (8.8% faster geometrically, 1.029× median effect, 1.083× summed medians, 35/37 effects faster). Assembly still leads by 3.251× overall. All complete outputs matched before timing.

## What changed

Changed rows previously regenerated every visible cell. The new cache stores byte boundaries every eight cells, finds changed blocks, and rewrites only their runs. It processes disjoint runs from right to left, preserves the unchanged prefix, and shifts the suffix with checked `Vec` operations when byte lengths change. One shared scratch buffer serves all rows. Rows with many changes, changed dimensions, or more than 512 cells use complete serialization.

An AVX2 or scalar comparison builds a bounded 64-bit block mask. Runtime CPU detection and `TTFX_SIMD=0` retain the scalar path. The cache owns bytes and offsets, preserving character/visual ownership and callback timing. It adds no public API migration, assembly requirement, allocator, or unchecked row writer.

The SIMD loads cover only complete blocks of equal-length slices and remain behind the existing AVX2 guard. XOR/OR accumulates differences and the [documented zero-test intrinsic](https://doc.rust-lang.org/core/arch/x86_64/fn._mm256_testz_si256.html) determines whether a block differs. The row edits use safe `resize`, `copy_within`, `truncate`, and slice copying; all boundaries come from complete UTF-8 symbols.

Fresh profiles of `6e8c962` on the exact timing workload attributed 47.44% of Burn, 49.94% of Laseretch, and 29.46% of Bouncyballs samples to frame serialization. Binarypath remained dominated by grid updates (25.37%). [Profile commands, source identities, output hashes, and compiler flags](benchmarks/rust-fallback/round15/profiles/profile-metadata.json) make this attribution reproducible. The profiling build includes frame pointers and debug symbols; speedups below use ordinary release binaries.

The exploratory `inspect_rows.rs` workload used an explicit 200×50 canvas, while timing uses the normal CLI canvas defaults with a 200×50 terminal. Its counts are workload observations, not performance measurements. Initial profiles using that exploratory geometry were replaced by the exact-workload profiles linked above; all 12 new profiling output hashes match their benchmark baseline.

## Whole-program comparison

| Effect | Original PR Rust ms | New Rust ms | Speedup |
| --- | ---: | ---: | ---: |
| beams | 89.72 | 82.55 | 1.087× |
| binarypath | 571.19 | 563.42 | 1.014× |
| blackhole | 264.65 | 222.88 | 1.187× |
| bouncyballs | 120.90 | 105.32 | 1.148× |
| bubbles | 239.47 | 214.95 | 1.114× |
| burn | 183.30 | 109.31 | 1.677× |
| colorshift | 141.20 | 116.53 | 1.212× |
| crumble | 192.01 | 166.16 | 1.156× |
| decrypt | 124.60 | 119.66 | 1.041× |
| errorcorrect | 114.96 | 62.44 | 1.841× |
| expand | 112.99 | 90.59 | 1.247× |
| fireworks | 264.20 | 214.93 | 1.229× |
| highlight | 27.12 | 25.10 | 1.081× |
| laseretch | 251.99 | 155.90 | 1.616× |
| matrix | 104.89 | 70.40 | 1.490× |
| middleout | 59.68 | 60.51 | 0.986× |
| orbittingvolley | 70.37 | 58.21 | 1.209× |
| overflow | 119.17 | 65.82 | 1.811× |
| pour | 83.02 | 67.30 | 1.234× |
| print | 39.35 | 37.16 | 1.059× |
| rain | 99.97 | 90.82 | 1.101× |
| randomsequence | 24.32 | 23.65 | 1.028× |
| rings | 432.40 | 420.85 | 1.027× |
| scattered | 110.33 | 88.94 | 1.240× |
| slice | 44.80 | 43.06 | 1.040× |
| slide | 55.04 | 55.11 | 0.999× |
| smoke | 74.50 | 60.81 | 1.225× |
| spotlights | 158.71 | 95.40 | 1.664× |
| spray | 94.92 | 92.70 | 1.024× |
| swarm | 417.91 | 343.86 | 1.215× |
| sweep | 31.47 | 27.94 | 1.127× |
| synthgrid | 42.85 | 40.28 | 1.064× |
| thunderstorm | 64.91 | 56.19 | 1.155× |
| unstable | 140.10 | 139.18 | 1.007× |
| vhstape | 128.67 | 131.15 | 0.981× |
| waves | 80.32 | 76.00 | 1.057× |
| wipe | 27.20 | 26.00 | 1.046× |

Middleout takes 1.4% longer, Slide 0.1% longer, and VHStape 1.9% longer in this sweep. These measured tradeoffs are retained alongside the gains. [All original-PR samples](benchmarks/rust-fallback/round15/final.json) and the independent [current-checkpoint samples](benchmarks/rust-fallback/round15/checkpoint.json) include binary hashes and full-output checksums.

## Longer rechecks

Twenty-one paired repetitions against `6dce496` confirm the large targeted gains. VHStape remains 1.4% slower.

| Effect | Original PR Rust ms | New Rust ms | Speedup |
| --- | ---: | ---: | ---: |
| burn | 186.35 | 111.02 | 1.679× |
| laseretch | 252.61 | 157.47 | 1.604× |
| errorcorrect | 105.89 | 57.68 | 1.836× |
| matrix | 105.19 | 70.76 | 1.487× |
| overflow | 121.18 | 66.67 | 1.818× |
| spotlights | 150.21 | 93.02 | 1.615× |
| binarypath | 570.71 | 556.71 | 1.025× |
| vhstape | 128.42 | 130.22 | 0.986× |

[All recheck samples](benchmarks/rust-fallback/round15/recheck.json).

## Scalar path and memory

With explicit SIMD disabled in both binaries, a seven-effect subset improves by 1.237× geometrically over `6e8c962`. Burn gains 1.625×, Laseretch 1.431×, ErrorCorrect 1.661×, and Matrix 1.219×. Bouncyballs takes 3.7% longer, Binarypath 1.7% longer, and Waves 0.7% longer. These are scalar-path measurements on the same x86-64 host. [Raw scalar samples](benchmarks/rust-fallback/round15/scalar.json).

Peak RSS comes from three separate complete, untimed runs per effect/build using Linux `wait4`. The new cache adds offset storage and a shared scratch buffer. Observed changes versus the preceding checkpoint range from about −0.79 to +0.71 MiB; these small process-peak differences include allocator/run variation.

| Effect | Original PR MiB | Prior checkpoint MiB | New MiB |
| --- | ---: | ---: | ---: |
| binarypath | 215.57 | 214.65 | 214.71 |
| rings | 181.20 | 181.22 | 180.43 |
| colorshift | 58.55 | 54.62 | 54.72 |
| burn | 42.01 | 41.90 | 42.61 |
| laseretch | 30.51 | 30.67 | 30.70 |
| errorcorrect | 33.53 | 34.20 | 34.26 |
| bouncyballs | 39.43 | 39.47 | 39.64 |
| matrix | 28.46 | 24.88 | 25.07 |
| spotlights | 19.26 | 19.26 | 19.26 |
| overflow | 41.91 | 35.00 | 35.13 |

[All memory samples](benchmarks/rust-fallback/round15/memory.json).

## Validation

- 107 release tests passed with NASM explicitly supplied, including 21 assembly differential tests.
- 86 release tests passed with `--no-default-features` in a separate target directory.
- 19 CLI cases, 354 Python-reference parity cases, 41 complete terminal streams, and signal/terminal-close/resize checks passed.
- 562 additional complete-output comparisons cover 281 scenarios: all 37 effects with plain, Unicode, input-color, xterm, and no-color input; scalar execution; and boundary-width cases through 2,048 columns.
- New row-cache tests compare against fresh serialization through growth, shrinkage, empty/Unicode/ANSI/heap symbols, and geometry changes; they assert that the partial-patch branch executes. Block masks are checked at every alignment/tail/mismatch, including the 512-cell mask limit and oversized-row fallback.
- All 37 complete original-PR Rust/new Rust/forced-assembly outputs matched before timing.

[Full validation log](benchmarks/rust-fallback/round15/final-tests.log), [no-assembly log](benchmarks/rust-fallback/round15/no-asm-tests.log), [additional comparison results](benchmarks/rust-fallback/round15/render-parity.json), and [source/binary identities](benchmarks/rust-fallback/round15/environment.json). Hosted CI status is recorded on PR #41 for the pushed commit.

## Experiments and tuning

| Experiment | Measurement | Decision |
| --- | --- | --- |
| Shared stationary progress groups (round 14) | 1.010× geometric mean, 1.000× median across 37 effects; several moving effects regressed | Removed the additional scheduler |
| Per-cell row offsets with one changed span | Burn 1.553×, Laseretch 1.332×, Bouncyballs 0.759× | Removed fine-grained offset bookkeeping |
| Sixteen-cell offsets with one changed span | Burn 1.528×, Laseretch 1.330×, Bouncyballs 0.949× | Replaced the single span with disjoint runs |
| Sixteen-cell disjoint patches | 1.078× geometric mean over 37 effects | Retained the algorithm; tuned block size |
| Eight-cell blocks | 1.011× geometric mean over the sixteen-cell version on 13 effects | Retained the constant-size tuning; final results above measure it directly |
| Hidden-update filtering and a different overlap winner scan | 0.999× geometric mean over 11 effects | Removed both supplementary changes |

The rejected shared-counter, per-cell, single-span, and overlap variants are preserved as generated patches and raw timing artifacts in this report’s evidence directory. They are absent from retained production code. This change differs from the rejected round-12 block-copy prototype: it edits the cached row in place instead of assembling a new copy of every changed row.

## Measurement method

Ryzen 5 7600X, Linux, Rust 1.98.1 / LLVM 22.1.8; CPU affinity 2; seed 1; the existing 190×46 ASCII input; terminal environment 200×50; normal CLI canvas defaults; virtual clock; frame pacing disabled; output to `/dev/null`. Timings include startup, setup, the whole animation, and output generation. Full comparisons use seven measured repetitions after two warmups; targeted rechecks use twenty-one. Execution order alternates, or is shuffled for the three-binary comparison. Builds, correctness tests, profiling, memory measurements, and timing runs are separate.

Both primary builds use `TTFX_ASM=0`; the assembly comparator uses `TTFX_ASM=force`. No PGO, native-CPU build flags, or binary rewriting contribute to these results. Full stdout length/hash, stderr, and exit status are compared before timing. Absolute timings remain specific to this machine and workload.
