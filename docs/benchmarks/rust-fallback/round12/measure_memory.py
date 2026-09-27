"""Per-process peak RSS, three runs for each mode, on the timing workload."""
import hashlib
import json
import os
from pathlib import Path
import statistics

os.sched_setaffinity(0, {2})
modes = [("previous_rust", "target/round12/ttfx-pr41"), ("candidate_rust", "target/round12/ttfx-final"),
         ("assembly", "target/round12/ttfx-pr41")]
report = {"cpu": 2, "repeats": 3, "binary_sha256": {name: hashlib.sha256(Path(p).read_bytes()).hexdigest() for name, p in modes}, "results": {}}
for effect in ["binarypath", "rings", "colorshift", "waves", "spotlights", "matrix", "overflow", "fireworks"]:
    report["results"][effect] = {}
    for name, binary in modes:
        samples = []
        for _ in range(3):
            source = os.open("target/pr35-input.txt", os.O_RDONLY)
            sink = os.open("/dev/null", os.O_WRONLY)
            args = [str(Path(binary).resolve()), "--seed", "1", "--frame-rate", "0", "--virtual-clock", "--canvas-width", "200", "--canvas-height", "50", "--ignore-terminal-dimensions", effect]
            pid = os.posix_spawn(args[0], args, {**os.environ, "TTFX_ASM": "force" if name == "assembly" else "0"},
                                 file_actions=[(os.POSIX_SPAWN_DUP2, source, 0), (os.POSIX_SPAWN_DUP2, sink, 1)])
            _, status, usage = os.wait4(pid, 0)
            os.close(source)
            os.close(sink)
            assert os.waitstatus_to_exitcode(status) == 0, (name, effect, status)
            samples.append(usage.ru_maxrss)
        report["results"][effect][name] = {"max_rss_kib_samples": samples, "median_max_rss_kib": statistics.median(samples)}
print(json.dumps(report, indent=2))
