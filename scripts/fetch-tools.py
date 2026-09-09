#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Fetch pinned upstream release tools and verify their published SHA-256 digests."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--destination', type=Path, required=True)
    args = parser.parse_args()
    entries = json.loads((ROOT / 'packaging/tools.json').read_text())
    args.destination.mkdir(parents=True, exist_ok=True)
    for name, entry in entries.items():
        with urllib.request.urlopen(entry['url'], timeout=60) as response:
            data = response.read(64 * 1024 * 1024 + 1)
        if len(data) > 64 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != entry['sha256']:
            raise SystemExit(f'{name}: the downloaded archive failed SHA-256 verification')
        with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
            members = [m for m in archive.getmembers() if m.isfile() and Path(m.name).name == name]
            if len(members) != 1 or members[0].size > 64 * 1024 * 1024:
                raise SystemExit(f'{name}: unexpected archive contents')
            destination = args.destination / name
            destination.write_bytes(archive.extractfile(members[0]).read())
            destination.chmod(0o755)
        print(f'{name} {entry["version"]}: verified and installed')


if __name__ == '__main__':
    main()
