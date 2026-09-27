"""Profile unchanged Rust fallback sources; this is not a timing benchmark.

Build first, separately from recording:
NASM=/path/to/nasm CARGO_TARGET_DIR=target/research/profile \
  RUSTFLAGS='-C force-frame-pointers=yes -C debuginfo=1 -C strip=none' \
  cargo build --release

PERF and PERF_LIBRARY_PATH optionally select a locally extracted perf tool.
"""
from pathlib import Path
import hashlib
import json
import os
import subprocess
import sys
import tempfile

ROOT = Path.cwd()
os.chdir(ROOT)
PROFILE = ROOT / "target/round15/profile/release/ttfx"
RELEASE = ROOT / "target/round15/ttfx-before"
INPUT = ROOT / "docs/benchmarks/rust-fallback/pr/input.txt"
ARTIFACTS = Path(__file__).resolve().parent
RAW = ARTIFACTS
EFFECTS = ("binarypath", "rings", "spotlights", "waves", "overflow", "swarm", "colorshift", "fireworks", "bouncyballs", "burn", "laseretch", "vhstape")
REPEATS = 8


def command(binary, effect):
    return [str(binary), "--seed", "1", "--frame-rate", "0", "--virtual-clock",
            effect]


def digest(path):
    with open(path, "rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


env = {**os.environ, "TTFX_ASM": "0", "COLUMNS": "200", "LINES": "50"}
# Make the intended runtime configuration independent of the launching shell.
for key in ("TTFX_SCENE_RUNTIME", "TTFX_SIMD"):
    env.pop(key, None)

if len(sys.argv) == 3 and sys.argv[1] == "--worker":
    effect = sys.argv[2]
    assert effect in EFFECTS
    for _ in range(REPEATS):
        with INPUT.open("rb") as source:
            result = subprocess.run(command(PROFILE, effect), stdin=source,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                    env=env, timeout=120)
        assert result.returncode == 0 and not result.stderr, result.stderr
    sys.exit(0)

os.sched_setaffinity(0, {2})
perf = os.environ.get("PERF", "perf")
perf_env = dict(env)
if os.environ.get("PERF_LIBRARY_PATH"):
    perf_env["LD_LIBRARY_PATH"] = os.environ["PERF_LIBRARY_PATH"]

metadata = {
    "purpose": "Hotspot attribution, not release performance or speedup claims",
    "cpu": 2, "effects": EFFECTS, "repetitions": REPEATS,
    "sample_event": "cycles:u", "sample_frequency": 999, "call_graph": "fp",
    "report_filter": "comm=ttfx (includes ttfx shared-library samples)",
    "profile_rustflags": "-C force-frame-pointers=yes -C debuginfo=1 -C strip=none",
    "input_sha256": digest(INPUT), "binary_sha256": {
        "release": digest(RELEASE), "profile": digest(PROFILE)},
    "source_sha256": {"src/" + str(p.relative_to("target/round15/source-before")): digest(p) for p in sorted(Path("target/round15/source-before").rglob("*.rs"))},
    "source_origin": "Frozen accepted source-before at6e8c962; profile binary was built before all row-cache edits",
    "toolchain": subprocess.check_output(["rustc", "-vV"], text=True),
    "git_head": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
    "output_checks": {}, "record_commands": {}, "report_commands": {},
}
RAW.mkdir(parents=True, exist_ok=True)
for effect in EFFECTS:
    outputs = []
    for binary in (RELEASE, PROFILE):
        with tempfile.TemporaryFile() as output, INPUT.open("rb") as source:
            result = subprocess.run(command(binary, effect), stdin=source, stdout=output,
                                    stderr=subprocess.PIPE, env=env, timeout=120)
            output.seek(0)
            outputs.append({"stdout_sha256": hashlib.file_digest(output, "sha256").hexdigest(),
                            "stdout_bytes": os.fstat(output.fileno()).st_size, "exit_code": result.returncode,
                            "stderr_hex": result.stderr.hex()})
    assert outputs[0] == outputs[1] and outputs[0]["exit_code"] == 0, (effect, outputs)
    metadata["output_checks"][effect] = outputs[0]
    raw_file = RAW / f"{effect}.data"
    record = [perf, "record", "-F", "999", "-e", "cycles:u", "--call-graph", "fp",
              "-o", str(raw_file), "--", sys.executable, str(Path(__file__).resolve()),
              "--worker", effect]
    metadata["record_commands"][effect] = record
    with (ARTIFACTS / f"{effect}-record.log").open("w") as log:
        subprocess.run(record, stdout=log, stderr=log, env=perf_env, check=True, timeout=180)
    for name, flag in (("self", "--no-children"), ("inclusive", "--children")):
        report = [perf, "report", "--stdio", "-i", str(raw_file), "--comms", "ttfx",
                  flag, "--sort", "dso,symbol", "--percent-limit", "0.5",
                  "--no-inline", "-g", "none"]
        metadata["report_commands"][f"{effect}-{name}"] = report
        with (ARTIFACTS / f"{effect}-{name}.txt").open("w") as output:
            subprocess.run(report, stdout=output, stderr=subprocess.STDOUT,
                           env=perf_env, check=True, timeout=60)
    (ARTIFACTS / "profile-metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(f"{effect}: complete output matched, profile collected", flush=True)
