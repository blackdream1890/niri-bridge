#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
if [[ "${NIRI_BRIDGE_CI:-}" != 1 ]]; then
    echo 'This entry point is for the isolated build container.' >&2
    exit 1
fi
python3 - <<'PY'
from pathlib import Path
import shutil, subprocess
source, workspace = Path('/input'), Path('/workspace')
if not (source / '.git').is_dir() or any(workspace.iterdir()):
    raise SystemExit('Use a fresh Git checkout and an empty container workspace.')
excluded = {'.local', 'target', 'dist', 'logs', 'recordings', 'tmp', '.venv'}
for path in source.iterdir():
    if path.name in excluded or (path.name.startswith('.env') and path.name != '.env.example'):
        continue
    target = workspace / path.name
    if path.is_symlink():
        raise SystemExit('Symbolic links in the build input require review.')
    if path.is_dir():
        shutil.copytree(path, target, symlinks=True)
    else:
        shutil.copy2(path, target)
# These are new container-owned files. No Git ownership exception is configured.
# Remove ignored state from this disposable copy before any dependency executes.
ignored = subprocess.check_output(['git', '-C', str(workspace), 'ls-files', '--others', '--ignored', '--exclude-standard', '-z'])
for value in ignored.split(b'\0'):
    if value:
        path = workspace / value.decode()
        if not path.parent.resolve().is_relative_to(workspace):
            raise SystemExit('An ignored path points outside the disposable workspace.')
        path.unlink(missing_ok=True)
if subprocess.check_output(['git', '-C', str(workspace), 'status', '--porcelain']):
    raise SystemExit('The source checkout must be clean before running the build.')
PY
exec bash /workspace/scripts/ci.sh
