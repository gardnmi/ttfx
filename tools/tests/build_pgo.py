"""Build an optional, portable Rust-fallback PGO binary and matched control.

Run from any directory: python3 tools/tests/build_pgo.py [--llvm-profdata PATH]
Set NASM in the environment as for a normal build, or use --no-default-features.
The normal target/release binary and Cargo profiles are not changed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


def run(command, **kwargs):
    print(' '.join(map(str, command)), flush=True)
    subprocess.run(list(map(str, command)), check=True, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--llvm-profdata', default='llvm-profdata')
    parser.add_argument('--output-dir', type=Path, default=Path('target/rust-pgo'))
    parser.add_argument('--no-default-features', action='store_true')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    os.chdir(root)
    if os.environ.get('RUSTFLAGS') or os.environ.get('CARGO_ENCODED_RUSTFLAGS'):
        parser.error('unset RUSTFLAGS and CARGO_ENCODED_RUSTFLAGS for matched portable builds')
    compiler = subprocess.check_output(['rustc', '-vV'], text=True)
    host = re.search(r'^host: (.+)$', compiler, re.M).group(1)
    llvm = re.search(r'^LLVM version: (.+)$', compiler, re.M).group(1)
    version = subprocess.check_output([args.llvm_profdata, '--version'], text=True)
    actual = re.search(r'LLVM version (\S+)', version).group(1)
    if llvm != actual:
        parser.error(f'llvm-profdata must match rustc LLVM {llvm}; found {actual}')
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    result = Path(tempfile.mkdtemp(prefix='run-', dir=output))
    profiles = result / 'profiles'
    profiles.mkdir()
    profile = result / 'merged.profdata'
    features = ['--no-default-features'] if args.no_default_features else []
    base = ['cargo', 'build', '--release', '--target', host, *features]
    control_dir = output / 'control'
    tuned_dir = output / 'pgo'
    run([*base, '--target-dir', control_dir])
    instrument_env = {**os.environ, 'CARGO_ENCODED_RUSTFLAGS': f'-Cprofile-generate={profiles}'}
    run([*base, '--target-dir', tuned_dir], env=instrument_env)
    binary = tuned_dir / host / 'release/ttfx'
    run([sys.executable, root / 'tools/tests/train_pgo.py', '--binary', binary,
         '--profiles', profiles, '--json', result / 'training.json'])
    run([args.llvm_profdata, 'merge', '-o', profile, profiles])
    tuned_env = {**os.environ, 'CARGO_ENCODED_RUSTFLAGS': f'-Cprofile-use={profile}\x1f-Cllvm-args=-pgo-warn-missing-function'}
    run([*base, '--target-dir', tuned_dir], env=tuned_env)
    control = control_dir / host / 'release/ttfx'
    report = {'compiler': compiler, 'llvm_profdata': version, 'profile': str(profile),
              'control': str(control), 'binary': str(binary),
              'control_sha256': hashlib.sha256(control.read_bytes()).hexdigest(),
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}
    (result / 'build.json').write_text(json.dumps(report, indent=2) + '\n')
    print(f'Build report: {result / "build.json"}')
    print(f'Run with TTFX_ASM=0: {binary}')
    print(f'Compare held-out workloads against: {control}')


if __name__ == '__main__':
    main()
