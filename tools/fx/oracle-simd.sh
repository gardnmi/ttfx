#!/usr/bin/env bash
# oracle-simd.sh - run tools/fx/oracle.sh for every effect with each of fx's
# SIMD kernel choices: the widest the CPU runs, TTFX_NO_AVX512=1 and
# TTFX_NO_AVX2=1 (the SSE2 and scalar paths), at most JOBS (default 4)
# oracles at a time. Pass THREADS=1 to run fx single-threaded.
#
# Usage: tools/fx/oracle-simd.sh [quick|full]
set -uo pipefail

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
MODE="${1:-quick}"
JOBS="${JOBS:-4}"
TMPROOT="${ORACLE_TMP:-$ROOT/target/oracle-tmp}"
mkdir -p "$TMPROOT" || exit 2
LOGS="$(mktemp -d "$TMPROOT/simd.XXXXXX")" || exit 2
trap 'rm -rf "$LOGS"' EXIT
effects=()
for source in "$ROOT"/src/fx/effects/*.rs; do
    [ -f "$source" ] || continue
    effect="${source##*/}"
    [ "$effect" = mod.rs ] || effects+=("${effect%.rs}")
done
[ "${#effects[@]}" -gt 0 ] || { echo "No effects found" >&2; exit 2; }

# An inherited override must not turn this into a reference/reference test or
# prevent the widest pass from exercising the runner's available kernels.
unset TTFX_FX TTFX_NO_AVX512 TTFX_NO_AVX2

status=0
for kernel in widest no-avx512 no-avx2; do
    case "$kernel" in
    widest) env=() ;;
    no-avx512) env=(TTFX_NO_AVX512=1) ;;
    no-avx2) env=(TTFX_NO_AVX512=1 TTFX_NO_AVX2=1) ;;
    esac
    [ "${THREADS:-}" = 1 ] && env+=(TTFX_THREADS=1)
    if ! printf '%s\n' "${effects[@]}" |
        xargs -P "$JOBS" -I{} env "${env[@]}" "$ROOT/tools/fx/oracle.sh" {} "$MODE" > "$LOGS/$kernel.log" 2>&1; then
        cat "$LOGS/$kernel.log"
        status=1
    fi
    total=$(grep -c '^oracle ' "$LOGS/$kernel.log")
    bad=$(grep '^oracle ' "$LOGS/$kernel.log" | grep -vc ' 0 failed')
    grep '^FAIL' "$LOGS/$kernel.log" | head -20
    echo "$kernel: $((total - bad))/${#effects[@]} effects pass ($MODE; $total completed)"
    [ "$bad" -eq 0 ] && [ "$total" -eq "${#effects[@]}" ] || status=1
done
exit $status
