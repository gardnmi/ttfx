"""Train every effect on inputs distinct from the timing/evaluation workloads."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--profiles', type=Path, required=True)
parser.add_argument('--json', type=Path, required=True)
args = parser.parse_args()
binary = args.binary.resolve()
profiles = args.profiles.resolve()
profiles.mkdir(parents=True, exist_ok=True)
help_text = subprocess.check_output([str(binary), '--help'], text=True)
commands = help_text.split('Commands:', 1)[1].split('Options:', 1)[0]
effects = [line.split()[0] for line in commands.splitlines() if line.strip() and line.split()[0] != 'help']
workloads = [
    (120, 32, 41, ('train alpha beta gamma delta 0123456789 ' * 3 + '\n') * 26, []),
    (60, 20, 73, ('\x1b[1;38;2;32;128;192mRust λ 雨 Ω\x1b[0m words 123\n') * 13, ['--existing-color-handling', 'always']),
    (40, 12, 97, ('narrow ü 🦀\ttrain\n') * 7, ['--xterm-colors']),
]
report = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'engine': 'rust', 'cases': []}
for index, (width, height, seed, text, options) in enumerate(workloads):
    data = text.encode()
    for effect in effects:
        command = [str(binary), '--seed', str(seed), '--frame-rate', '0', '--virtual-clock', '--canvas-width', str(width),
                   '--canvas-height', str(height), '--ignore-terminal-dimensions', *options, effect]
        result = subprocess.run(command, input=data, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                env={**os.environ, 'TTFX_ASM': '0', 'LLVM_PROFILE_FILE': str(profiles / f'{index}-{effect}-%m.profraw')}, timeout=180)
        assert result.returncode == 0 and not result.stderr, (command, result.returncode, result.stderr)
        report['cases'].append({'effect': effect, 'width': width, 'height': height, 'seed': seed,
                                'input_sha256': hashlib.sha256(data).hexdigest(), 'input_bytes': len(data), 'terminal_options': options})
        print(f'{index + 1}/{len(workloads)} {effect}', flush=True)
args.json.write_text(json.dumps(report, indent=2) + '\n')
