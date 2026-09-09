#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run touchpad handoff checks in a disposable VM with no host input or display access."""
import argparse
import gzip
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def initramfs(files):
    directories = {'dev', 'proc', 'sys', 'tmp', 'bin'}
    for name in list(files):
        directories.update(str(path) for path in Path(name).parents if str(path) != '.')
    entries = dict(files)
    entries.update({name: (b'', stat.S_IFDIR | 0o755) for name in directories})
    archive = bytearray()
    def entry(name, data, mode, inode):
        values = (inode, mode, 0, 0, 1, 0, len(data), 0, 0, 0, 0, len(name.encode()) + 1, 0)
        archive.extend(b'070701' + b''.join(f'{value:08x}'.encode() for value in values) + name.encode() + b'\0')
        archive.extend(b'\0' * (-len(archive) % 4))
        archive.extend(data)
        archive.extend(b'\0' * (-len(archive) % 4))
    for inode, (name, (data, mode)) in enumerate(sorted(entries.items()), 1):
        entry(name, data, mode, inode)
    entry('TRAILER!!!', b'', stat.S_IFREG, len(entries) + 1)
    return gzip.compress(archive, mtime=0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--kernel', type=Path, required=True, help='Readable Ubuntu kernel image with CONFIG_INPUT_UINPUT=y.')
    parser.add_argument('--busybox', type=Path, default=Path(shutil.which('busybox') or '/usr/bin/busybox'))
    parser.add_argument('--observer', type=Path, help='Existing observer compiled from tests/common/libinput_observer.c.')
    parser.add_argument('--tcg', action='store_true', help='Use software emulation even when KVM is available.')
    args = parser.parse_args()
    if not args.kernel.is_file() or not os.access(args.kernel, os.R_OK) or not args.busybox.is_file():
        parser.error('A readable kernel image and busybox-static are required; this runner never changes host boot files.')
    with tempfile.TemporaryDirectory(prefix='niri-bridge-kernel-test-') as temporary:
        directory = Path(temporary)
        build = subprocess.check_output(['cargo', 'test', '--locked', '--lib', '--no-run', '--message-format=json'], cwd=ROOT, text=True)
        artifacts = [json.loads(line) for line in build.splitlines()]
        binary = Path(next(item['executable'] for item in artifacts if item.get('executable') and item.get('target', {}).get('name') == 'niri_bridge'))
        observer = args.observer or directory / 'libinput-observer'
        if not args.observer:
            flags = subprocess.check_output(['pkg-config', '--cflags', '--libs', 'libinput'], text=True).split()
            subprocess.run(['cc', '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror',
                            str(ROOT / 'tests/common/libinput_observer.c'), '-o', str(observer), *flags], check=True)
        files = {}
        def add(source, target):
            source = Path(source)
            files[target] = (source.read_bytes(), stat.S_IFREG | (source.stat().st_mode & 0o777))
        for source, target in [(args.busybox, 'bin/busybox'), (binary, 'probe'), (observer, 'libinput-observer')]:
            add(source, target)
            libraries = subprocess.run(['ldd', str(source)], capture_output=True, text=True)
            for name in re.findall(r'(/[^\s()]+)', libraries.stdout):
                if Path(name).is_file():
                    add(name, name.lstrip('/'))
        for path in Path('/usr/share/libinput').rglob('*'):
            if path.is_file():
                add(path, str(path).lstrip('/'))
        files['init'] = (b'''#!/bin/busybox sh
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/probe --exact touchpad::tests::kernel_handoff_keeps_the_original_reader_synchronized --ignored --nocapture
status=$?
echo KERNEL_PROBE_EXIT=$status
/bin/busybox poweroff -f
''', stat.S_IFREG | 0o755)
        archive = directory / 'initramfs.cpio.gz'
        archive.write_bytes(initramfs(files))
        # No host filesystem sharing, networking, monitor, input passthrough or display.
        command = ['qemu-system-x86_64', '-no-user-config', '-nodefaults', '-display', 'none',
                   '-serial', 'stdio', '-monitor', 'none', '-nic', 'none', '-no-reboot',
                   '-m', '384', '-smp', '2', '-kernel', str(args.kernel), '-initrd', str(archive),
                   '-append', 'console=ttyS0 quiet panic=1 rdinit=/init niri-bridge-kernel-test=1']
        if not args.tcg and os.access('/dev/kvm', os.R_OK | os.W_OK):
            command.extend(['-accel', 'kvm', '-cpu', 'host'])
        else:
            command.extend(['-accel', 'tcg'])
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=90)
        print(result.stdout[-6000:])
        if result.returncode or 'KERNEL_PROBE_EXIT=0' not in result.stdout:
            raise SystemExit('Isolated kernel/libinput handoff verification failed.')


if __name__ == '__main__':
    main()
