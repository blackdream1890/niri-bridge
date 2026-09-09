#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check publication candidates without printing any matched private values."""
import argparse
import json
import io
from pathlib import Path
import re
import struct
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parent.parent
TOKEN = re.compile(rb'(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|AKIA[A-Z0-9]{16})')
KEY = re.compile(rb'-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----\s+[A-Za-z0-9+/=]{32}')
LOCAL_PATH = re.compile(rb'(?:/home/[A-Za-z0-9._-]+/|[A-Za-z]:\\Users\\[A-Za-z0-9._-]+\\)')
EMAIL = re.compile(rb'[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}')
EXCLUDED = {'.git', '.local', 'target', 'dist', '__pycache__', '.venv'}
LOCAL_INSTRUCTION_FILES = {'agents.md', 'agents.override.md', 'claude.md', 'gemini.md',
                           'skill.md', 'copilot-instructions.md', '.cursorrules', '.windsurfrules'}
LOCAL_INSTRUCTION_DIRS = {'.codex', '.claude', '.cursor', '.agent', '.agents', '.windsurf'}
ATTRIBUTIONS = ROOT / 'licenses/binary-attributions.json'
PUBLIC_ATTRIBUTION_EMAILS = {item['email'].encode() for item in json.loads(ATTRIBUTIONS.read_text())['attributions']} if ATTRIBUTIONS.is_file() else set()


def local_instruction_path(name):
    parts = tuple(part.lower() for part in Path(name).parts)
    if 'vendor' in parts:
        return False
    return (Path(name).name.lower() in LOCAL_INSTRUCTION_FILES
            or any(part in LOCAL_INSTRUCTION_DIRS for part in parts)
            or any(parent == '.github' and child in {'instructions', 'agents', 'prompts'}
                   for parent, child in zip(parts, parts[1:])))


def findings(name, content, denied=(), *, check_emails=True):
    result = []
    parts = Path(name).parts
    upstream = 'vendor' in parts
    if not upstream and (any(part in EXCLUDED for part in parts) or any(part.startswith('.env') and part != '.env.example' for part in parts)):
        result.append('private or generated path')
    if not upstream and Path(name).suffix.lower() in ('.pem', '.key', '.p12', '.pfx', '.pcap', '.pcapng', '.log'):
        result.append('credential or diagnostic file')
    for label, pattern in [('private key', KEY), ('token-shaped value', TOKEN), ('personal build path', LOCAL_PATH)]:
        if not upstream and pattern.search(content):
            result.append(label)
    if any(value and value in content for value in denied):
        result.append('private value from local audit rules')
    # Upstream source can contain public test keys and author notices. It is
    # checksum-verified separately; exact maintainer-private strings still fail.
    legal = upstream or 'licenses' in parts or Path(name).name in ('LICENSE', 'COPYRIGHT')
    if not legal and check_emails:
        for address in EMAIL.findall(content):
            if not (address in PUBLIC_ATTRIBUTION_EMAILS or address.endswith(b'@users.noreply.github.com') or address.endswith(b'@example.com') or address.endswith(b'@example.invalid')):
                result.append('non-private email address')
                break
    if not upstream and content.startswith(b'\x89PNG\r\n\x1a\n'):
        offset = 8
        while offset + 12 <= len(content):
            size = struct.unpack('>I', content[offset:offset + 4])[0]
            kind = content[offset + 4:offset + 8]
            if kind in (b'tEXt', b'zTXt', b'iTXt', b'eXIf'):
                result.append('image metadata needs review')
                break
            offset += 12 + size
    return result


def tree_files(root):
    if (root / '.git').exists():
        output = subprocess.check_output(['git', '-C', str(root), 'ls-files', '-co', '--exclude-standard', '-z'])
        return [Path(value.decode()) for value in sorted(set(output.split(b'\0'))) if value]
    return [p.relative_to(root) for p in root.rglob('*') if p.is_file() and not any(part in EXCLUDED for part in p.relative_to(root).parts)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tree', type=Path, default=ROOT)
    parser.add_argument('--archive', type=Path)
    parser.add_argument('--history', action='store_true', help='Scan commit metadata and historical content; report earlier local instruction files.')
    parser.add_argument('--deny-file', type=Path, help='Local ignored JSON containing exact private strings; matched values are never printed.')
    args = parser.parse_args()
    denied = [value.encode() for value in json.loads(args.deny_file.read_text())] if args.deny_file else []
    issues, checked, vendor_files, vendor_checksums = [], 0, {}, {}
    historical_instructions = []
    def inspect(name, content):
        if local_instruction_path(name):
            issues.append({'file': name, 'reason': 'local automation instructions or configuration'})
        parts = Path(name).parts
        if 'vendor' in parts:
            position = parts.index('vendor')
            if len(parts) < position + 3:
                issues.append({'file': name, 'reason': 'unexpected vendored source path'})
            else:
                package, relative = parts[position + 1], '/'.join(parts[position + 2:])
                if relative == '.cargo-checksum.json':
                    vendor_checksums[package] = json.loads(content)['files']
                else:
                    import hashlib
                    vendor_files.setdefault(package, {})[relative] = hashlib.sha256(content).hexdigest()
        for reason in findings(name, content, denied):
            issues.append({'file': name, 'reason': reason})
    if args.archive:
        streams = [None]
        if args.archive.suffix == '.deb':
            streams = [io.BytesIO(subprocess.check_output(['dpkg-deb', option, str(args.archive)]))
                       for option in ('--fsys-tarfile', '--ctrl-tarfile')]
        for stream in streams:
            with tarfile.open(args.archive if stream is None else None, mode='r:*', fileobj=stream) as archive:
                for member in archive:
                    if member.isdir():
                        continue
                    checked += 1
                    path = Path(member.name)
                    if not member.isfile() or path.is_absolute() or '..' in path.parts or member.size > 128 * 1024 * 1024:
                        issues.append({'file': member.name, 'reason': 'unexpected archive member'})
                        continue
                    inspect(member.name, archive.extractfile(member).read())
    else:
        root = args.tree.resolve()
        for name in tree_files(root):
            path = root / name
            checked += 1
            if path.is_symlink() or not path.resolve().is_relative_to(root):
                issues.append({'file': str(name), 'reason': 'symbolic link needs review'})
                continue
            inspect(str(name), path.read_bytes())
        if args.history:
            seen = set()
            commits = subprocess.check_output(['git', '-C', str(root), 'rev-list', '--all'], text=True).splitlines()
            for commit in commits:
                metadata = subprocess.check_output(['git', '-C', str(root), 'show', '-s', '--format=%an%n%ae%n%cn%n%ce', commit])
                for reason in findings('commit-metadata', metadata, denied, check_emails=False):
                    issues.append({'commit': commit[:12], 'reason': reason})
                message = subprocess.check_output(['git', '-C', str(root), 'show', '-s', '--format=%B', commit])
                for reason in findings('commit-message', message, denied):
                    issues.append({'commit': commit[:12], 'reason': reason})
                entries = subprocess.check_output(['git', '-C', str(root), 'ls-tree', '-r', '-z', commit]).split(b'\0')
                for entry in entries:
                    if not entry:
                        continue
                    metadata, name = entry.split(b'\t', 1)
                    _, kind, oid = metadata.split()
                    if kind != b'blob' or oid in seen:
                        continue
                    seen.add(oid)
                    content = subprocess.check_output(['git', '-C', str(root), 'cat-file', 'blob', oid.decode()])
                    if local_instruction_path(name.decode()):
                        historical_instructions.append({'file': name.decode(), 'commit': commit[:12]})
                    for reason in findings(name.decode(), content, denied):
                        issues.append({'file': name.decode(), 'commit': commit[:12], 'reason': reason})
    for package in vendor_files.keys() | vendor_checksums.keys():
        if vendor_files.get(package) != vendor_checksums.get(package):
            issues.append({'package': package, 'reason': 'vendored source checksum mismatch'})
    print(json.dumps({'files_checked': checked, 'vendored_packages_verified': len(vendor_files),
                      'historical_local_instruction_files': historical_instructions, 'issues': issues}, indent=2))
    raise SystemExit(bool(issues))


if __name__ == '__main__':
    main()
