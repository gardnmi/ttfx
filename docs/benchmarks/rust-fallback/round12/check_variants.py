"""Complete output comparisons for runtime switches and held-out effect options."""
from pathlib import Path
import hashlib
import itertools
import json
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[4]
os.chdir(ROOT)
BASE = ROOT / "target/round12/ttfx-pr41"
CANDIDATE = ROOT / "target/round12/ttfx-final"
OUTPUT = Path(__file__).with_name("variant-parity.json")


def execute(binary, data, args, settings):
    with tempfile.TemporaryFile() as output:
        result = subprocess.run([str(binary), *args], input=data, stdout=output,
                                stderr=subprocess.PIPE, timeout=120,
                                env={**os.environ, **settings, "TTFX_ASM": "0"})
        size = output.tell()
        output.seek(0)
        return (result.returncode, result.stderr.hex(), size, hashlib.file_digest(output, "sha256").hexdigest())


report = {"binary_sha256": {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in [BASE, CANDIDATE]},
          "cases": []}
inputs = [b"ABCD\nEFGH\nIJKL", "λ界 x\nAB —\n\x1b[31mred\x1b[0m plain\n\x1b[48;5;42mbg\x1b[0m".encode()]
modes = [[], ["--no-color"], ["--xterm-colors"], ["--existing-color-handling", "always"],
         ["--existing-color-handling", "dynamic"], ["--existing-color-handling", "dynamic", "--xterm-colors"]]
common = ["--seed", "23", "--frame-rate", "0", "--virtual-clock", "--canvas-width", "20",
          "--canvas-height", "8", "--ignore-terminal-dimensions"]
for index, mode, falloff, count in itertools.product(range(len(inputs)), modes, ["0", "0.3", "1", "2"], ["1", "5"]):
    args = [*common, *mode, "spotlights", "--search-duration", "12", "--beam-falloff", falloff,
            "--spotlight-count", count]
    expected = execute(BASE, inputs[index], args, {})
    assert expected[0] == 0
    for settings in ({}, {"TTFX_LIGHTING_CACHE": "0"}, {"TTFX_SIMD": "0", "TTFX_MOTION_RUNTIME": "0"}):
        actual = execute(CANDIDATE, inputs[index], args, settings)
        assert actual == expected, (args, settings, expected, actual)
    report["cases"].append({"args": args, "input_index": index, "output": expected,
                            "candidate_modes": ["default", "lighting off", "SIMD and motion runtime off"]})
print("96 complete Spotlights variants matched in three candidate modes", flush=True)

help_text = subprocess.check_output([str(BASE), "--help"], text=True)
commands = help_text.split("Commands:", 1)[1].split("Options:", 1)[0]
effects = [line.split()[0] for line in commands.splitlines() if line.strip() and line.split()[0] != "help"]
data = "\x1b[1;38;2;200;32;90mλRust 界\x1b[0m 123\nSmall output\n".encode()
for effect in effects:
    args = [*common, "--xterm-colors", effect]
    expected = execute(BASE, data, args, {})
    assert expected[0] == 0
    for settings in ({}, {"TTFX_MOTION_SIMD": "0"}, {"TTFX_MOTION_RUNTIME": "0"},
                     {"TTFX_MOTION_RUNTIME": "0", "TTFX_SCENE_RUNTIME": "0", "TTFX_SCHEDULER": "0"}):
        actual = execute(CANDIDATE, data, args, settings)
        assert actual == expected, (args, settings, expected, actual)
    report["cases"].append({"args": args, "input": data.decode(), "output": expected,
                            "candidate_modes": ["default", "motion SIMD off", "motion runtime off", "all runtimes off"]})
OUTPUT.write_text(json.dumps(report, indent=2) + "\n")
print("37 complete effects matched in four candidate modes", flush=True)

for effect in ["matrix", "overflow"]:
    for index, mode in itertools.product(range(len(inputs)), modes):
        args = [*common, *mode, effect]
        expected = execute(BASE, inputs[index], args, {})
        assert expected[0] == 0 and expected[2] > 0
        for settings in ({}, {"TTFX_APPEARANCE_CACHE": "0"}):
            actual = execute(CANDIDATE, inputs[index], args, settings)
            assert actual == expected, (args, settings, expected, actual)
        report["cases"].append({"args": args, "input_index": index, "output": expected,
                                "candidate_modes": ["default", "appearance palette off"]})
OUTPUT.write_text(json.dumps(report, indent=2) + "\n")
print("24 Matrix/Overflow variants matched with and without palettes", flush=True)
