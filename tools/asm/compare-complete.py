#!/usr/bin/env python3
"""Compare complete Rust/assembly animations; a declined assembly run fails.

Usage: compare-complete.py [binary]
TTFX_ASM_TIER selects the assembly tier (bin/test-asm defaults to baseline 1).
The existing oracle.sh offers a wider effect-option corpus for deeper testing.
"""

import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[2]
INPUTS = [
    b"Hello, World!\nRust and assembly\n",
    "\x1b[1;38;2;200;32;90mRust λ 界\x1b[0m\n\x1b[38;5;42mColors\x1b[0m and tabs\t!\n".encode(),
]
SEEDS = (42, 1337)


def run(binary, engine, effect, seed, data):
    args = [str(binary), "--seed", str(seed), "--frame-rate", "0", "--virtual-clock",
            "--canvas-width", "40", "--canvas-height", "12", "--ignore-terminal-dimensions", effect]
    env = {**os.environ, "TTFX_ASM": engine, "COLUMNS": "40", "LINES": "12"}
    with tempfile.TemporaryFile() as output:
        result = subprocess.run(args, input=data, stdout=output, stderr=subprocess.PIPE,
                                env=env, timeout=120)
        if result.returncode != 0:
            raise RuntimeError(f"{effect}, seed {seed}, TTFX_ASM={engine}: exit {result.returncode}: "
                               f"{result.stderr.decode(errors='replace')}")
        size = output.tell()
        if size == 0:
            raise RuntimeError(f"{effect}, seed {seed}, TTFX_ASM={engine}: no animation output")
        output.seek(0)
        digest = hashlib.file_digest(output, "sha256").hexdigest()
    return size, digest, result.stderr


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, nargs="?", default=ROOT / "target/release/ttfx")
    args = parser.parse_args()
    binary = args.binary.resolve()
    effects = sorted(p.stem for p in (ROOT / "asm/effects").glob("*.asm") if p.stem != "registry")
    if not effects:
        parser.error("no assembly effects found; refusing an empty comparison suite")
    cases = 0
    for effect in effects:
        for seed in SEEDS:
            for index, data in enumerate(INPUTS):
                reference = run(binary, "0", effect, seed, data)
                assembly = run(binary, "force", effect, seed, data)
                if assembly != reference:
                    raise RuntimeError(f"{effect}, seed {seed}, input {index}: output differs: "
                                       f"Rust={reference}, assembly={assembly}")
                cases += 1
        print(f"ok {effect}: {len(SEEDS) * len(INPUTS)} complete-output comparisons", flush=True)
    print(f"Assembly parity: {cases} comparisons passed across {len(effects)} effects")


if __name__ == "__main__":
    main()
