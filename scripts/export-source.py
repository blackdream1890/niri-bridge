#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Export only reviewed project files to a fresh build directory, without Git history."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists() and any(args.output.iterdir()):
        parser.exit(1, 'The export destination must be empty. Existing files were preserved.\n')
    subprocess.run(['python3', '-B', str(ROOT / 'scripts/check-public.py')], check=True)
    names = sorted(set(value.decode() for value in subprocess.check_output(
        ['git', 'ls-files', '-co', '--exclude-standard', '-z'], cwd=ROOT).split(b'\0') if value))
    args.output.mkdir(parents=True, exist_ok=True)
    for name in names:
        source, destination = ROOT / name, args.output / name
        if not source.is_file() or source.is_symlink():
            parser.exit(1, 'An unexpected project file was preserved for review.\n')
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        destination.chmod(0o755 if source.stat().st_mode & 0o111 else 0o644)
    print(f'Exported {len(names)} project files without repository history or local state.')


if __name__ == '__main__':
    main()
