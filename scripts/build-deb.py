#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Package the reviewed portable release with a desktop updater and apt dependencies."""
import os
from pathlib import Path
import subprocess
import tempfile


def build(destination, files, metadata, epoch):
    if destination.exists():
        raise ValueError('A package with this name already exists; use a fresh output directory.')
    with tempfile.TemporaryDirectory(prefix='niri-bridge-deb-') as temporary:
        root = Path(temporary)
        def write(name, content, mode=0o644):
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content.encode() if isinstance(content, str) else content)
            path.chmod(mode)
        for name, (data, mode) in files.items():
            write('usr/lib/niri-bridge/' + name, data, mode)
        write('usr/bin/niri-bridge', '#!/bin/sh\nexec /usr/lib/niri-bridge/bin/niri-bridge "$@"\n', 0o755)
        write('usr/bin/niri-bridge-ui', '#!/bin/sh\nexec /usr/bin/python3 -I /usr/lib/niri-bridge/scripts/package.py "$@"\n', 0o755)
        write('usr/share/applications/org.niribridge.NiriBridge.desktop', '''[Desktop Entry]
Type=Application
Name=NiriBridge
GenericName=Keyboard and pointer sharing
GenericName[zh_CN]=键鼠共享
Comment=Share keyboard, mouse and touchpad gestures across Niri desktops
Comment[zh_CN]=跨 Niri 桌面共享键鼠与触摸板手势
Exec=/usr/bin/niri-bridge-ui
Icon=niri-bridge
Terminal=false
StartupNotify=true
StartupWMClass=org.niribridge.NiriBridge
Categories=Utility;
''')
        write('usr/share/icons/hicolor/scalable/apps/niri-bridge.svg', files['ui/assets/niri-bridge.svg'][0])
        write('usr/share/doc/niri-bridge/copyright', files['COPYRIGHT'][0] + b'\n' + files['LICENSE'][0])
        write('DEBIAN/control', f'''Package: niri-bridge
Version: {metadata['version'].replace('-', '~', 1)}
Section: utils
Priority: optional
Architecture: amd64
Maintainer: NiriBridge contributors <174260507+blackdream1890@users.noreply.github.com>
Homepage: https://github.com/blackdream1890/niri-bridge
Depends: libc6 (>= {metadata['minimum_glibc_symbol_version']}), libgcc-s1, python3 (>= 3.11), python3-gi, python3-gi-cairo, gir1.2-gtk-3.0, openssl, acl, pkexec, systemd
Description: Share keyboard, pointer and touchpad gestures between Niri desktops
 NiriBridge connects two Ubuntu 26.04 computers running Niri 26.04.
 Open the application to set up pairing and choose Start sharing.
 Quitting from the tray stops sharing. Existing settings are retained.
''')
        write('DEBIAN/postinst', '''#!/bin/sh
set -eu
if [ "$1" = configure ]; then
    /usr/bin/python3 -I /usr/lib/niri-bridge/scripts/package-users.py register
fi
''', 0o755)
        write('DEBIAN/prerm', '''#!/bin/sh
set -eu
if [ "$1" = remove ]; then
    /usr/bin/python3 -I /usr/lib/niri-bridge/scripts/package-users.py unregister
fi
''', 0o755)
        for path in sorted(root.rglob('*'), reverse=True):
            os.utime(path, (epoch, epoch))
        os.utime(root, (epoch, epoch))
        subprocess.run(['dpkg-deb', '--build', '--root-owner-group', '--uniform-compression', '-Zxz',
                        str(root), str(destination.resolve())], check=True,
                       env=dict(os.environ, SOURCE_DATE_EPOCH=str(epoch)))
    return destination
