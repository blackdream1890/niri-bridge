#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Build deterministic binary and corresponding-source archives from a clean commit."""
import argparse
import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def archive(destination, prefix, files, epoch):
    if destination.exists():
        raise ValueError('A release asset with this name already exists; use a fresh output directory.')
    with destination.open('xb') as handle:
        with gzip.GzipFile(filename='', mode='wb', fileobj=handle, mtime=epoch) as zipped:
            with tarfile.open(fileobj=zipped, mode='w', format=tarfile.PAX_FORMAT) as tar:
                for name, (data, mode) in sorted(files.items()):
                    info = tarfile.TarInfo(prefix + '/' + name)
                    info.size, info.mode, info.mtime = len(data), mode, epoch
                    info.uid = info.gid = 0
                    info.uname = info.gname = 'root'
                    tar.addfile(info, io.BytesIO(data))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/release/niri-bridge')
    parser.add_argument('--output', type=Path, default=ROOT / 'dist')
    args = parser.parse_args()
    if command('git', 'status', '--porcelain'):
        parser.exit(1, 'Commit and review the source tree before creating release assets.\n')
    subprocess.run(['python3', '-B', str(ROOT / 'scripts/check-public.py'), '--tree', str(ROOT)], check=True)
    revision = command('git', 'rev-parse', 'HEAD')
    epoch = int(command('git', 'show', '-s', '--format=%ct', 'HEAD'))
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
    if command(str(args.binary.resolve()), '--version') != 'niri-bridge ' + version:
        parser.exit(1, 'The binary version does not match the source version.\n')
    raw = subprocess.check_output(['git', 'archive', '--format=tar', revision], cwd=ROOT)
    source = {}
    with tarfile.open(fileobj=io.BytesIO(raw)) as tar:
        for member in tar:
            if member.isdir():
                continue
            if not member.isfile():
                parser.exit(1, 'Source archives must not contain symbolic links or special files.\n')
            source[member.name] = (tar.extractfile(member).read(), 0o755 if member.mode & 0o111 else 0o644)
    required = ['LICENSE', 'COPYRIGHT', 'THIRD_PARTY_NOTICES.md', 'licenses/rust/index.json', 'scripts/uninstall.py']
    if not all(name in source for name in required):
        parser.exit(1, 'Required licensing or installation files are missing from the commit.\n')
    version_info = command('readelf', '--version-info', str(args.binary.resolve()))
    glibc = set(re.findall(r'GLIBC_([0-9.]+)', version_info))
    metadata = {'name': 'NiriBridge', 'version': version, 'source_commit': revision,
                'target': 'x86_64-unknown-linux-gnu', 'supported_os': 'Ubuntu 26.04',
                'compiler': command('rustc', '--version'), 'license': 'GPL-3.0-or-later',
                'minimum_glibc_symbol_version': max(glibc, key=lambda x: tuple(map(int, x.split('.')))),
                'cargo_lock_sha256': hashlib.sha256(source['Cargo.lock'][0]).hexdigest()}
    binary = {'bin/niri-bridge': (args.binary.read_bytes(), 0o755)}
    included_roots = {'ui', 'docs', 'licenses', 'service'}
    excluded_ui = {'test_ui.py', 'test_i18n.py', 'render_demo.py'}
    top_docs = {'LICENSE', 'COPYRIGHT', 'THIRD_PARTY_NOTICES.md', 'README.md', 'README.zh-CN.md',
                'CONTRIBUTING.md', 'CONTRIBUTING.zh-CN.md', 'SECURITY.md', 'SECURITY.zh-CN.md', 'CHANGELOG.md'}
    scripts = {'scripts/install.py', 'scripts/uninstall.py', 'scripts/device-access.py'}
    for name, value in source.items():
        path = Path(name)
        if name in top_docs or name in scripts or (path.parts[0] in included_roots and path.name not in excluded_ui):
            binary[name] = value
    binary['release.json'] = ((json.dumps(metadata, indent=2, sort_keys=True) + '\n').encode(), 0o644)
    # The source download includes exact vendored dependencies, so the source
    # needed to rebuild a published GPL binary is available with that binary.
    with tempfile.TemporaryDirectory(prefix='niri-bridge-source-release-') as directory:
        vendor = Path(directory) / 'vendor'
        result = subprocess.run(['cargo', 'vendor', '--locked', '--versioned-dirs', str(vendor)],
                                cwd=ROOT, text=True, capture_output=True, check=True)
        config = tomllib.loads(result.stdout)
        config['source']['vendored-sources']['directory'] = 'vendor'
        sections = []
        for key, values in config['source'].items():
            sections.append('[source.' + json.dumps(key) + ']')
            sections.extend(name + ' = ' + json.dumps(value) for name, value in values.items())
            sections.append('')
        source['.cargo/config.toml'] = (('\n'.join(sections)).encode(), 0o644)
        for path in vendor.rglob('*'):
            if path.is_symlink():
                parser.exit(1, 'A vendored dependency contains a symbolic link that requires review.\n')
            if path.is_file():
                source['vendor/' + path.relative_to(vendor).as_posix()] = (path.read_bytes(), 0o755 if path.stat().st_mode & 0o111 else 0o644)
    source['release.json'] = binary['release.json']
    args.output.mkdir(parents=True, exist_ok=True)
    base = 'niri-bridge-' + version
    binary_path = args.output / (base + '-ubuntu26.04-x86_64.tar.gz')
    source_path = args.output / (base + '-source.tar.gz')
    archive(binary_path, base, binary, epoch)
    archive(source_path, base, source, epoch)
    checksums = ''.join(hashlib.sha256(path.read_bytes()).hexdigest() + '  ' + path.name + '\n' for path in (binary_path, source_path))
    (args.output / 'SHA256SUMS').write_text(checksums)
    for path in (binary_path, source_path):
        subprocess.run(['python3', '-B', str(ROOT / 'scripts/check-public.py'), '--archive', str(path)], check=True)
    print('Release archives and SHA256SUMS created from commit ' + revision + '.')


if __name__ == '__main__':
    main()
