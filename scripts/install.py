#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Install NiriBridge for the current user, preserving identities and user settings."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
UI_FILES = ['app.py', 'model.py', 'canvas.py', 'tray.py', 'i18n.py', 'style.css',
            'locales/zh_CN.json', 'assets/niri-bridge.svg']
MANIFEST = Path('share/niri-bridge/install-manifest.json')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def atomic_copy(source, destination, mode=0o644):
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=destination.parent, prefix='.niri-bridge-install-', delete=False) as handle:
        temporary = Path(handle.name)
        try:
            os.fchmod(handle.fileno(), mode)
            with source.open('rb') as content:
                shutil.copyfileobj(content, handle)
            handle.flush()
            os.fsync(handle.fileno())
            temporary.replace(destination)
        finally:
            temporary.unlink(missing_ok=True)


def write_file(destination, content, mode=0o644):
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode='w', dir=destination.parent, prefix='.niri-bridge-install-', delete=False) as handle:
        temporary = Path(handle.name)
        try:
            os.fchmod(handle.fileno(), mode)
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
            temporary.replace(destination)
        finally:
            temporary.unlink(missing_ok=True)


def desktop_exec(path):
    escaped = str(path).replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$').replace('%', '%%')
    return '"' + escaped + '"'


def systemd_exec(path):
    return '"' + str(path).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%') + '"'


def installation_sources():
    binary = ROOT / 'bin/niri-bridge'
    if not binary.is_file():
        binary = ROOT / 'target/release/niri-bridge'
    sources = [(binary, 'bin/niri-bridge', 0o755),
               (ROOT / 'scripts/device-access.py', 'bin/niri-bridge-setup-input', 0o755),
               (ROOT / 'scripts/uninstall.py', 'share/niri-bridge/installer/uninstall.py', 0o644),
               (ROOT / 'ui/assets/niri-bridge.svg', 'share/icons/hicolor/scalable/apps/niri-bridge.svg', 0o644)]
    sources.extend((ROOT / 'ui' / name, 'share/niri-bridge/ui/' + name, 0o644) for name in UI_FILES)
    documents = [ROOT / name for name in ('README.md', 'README.zh-CN.md', 'CONTRIBUTING.md',
                 'CONTRIBUTING.zh-CN.md', 'SECURITY.md', 'SECURITY.zh-CN.md', 'LICENSE', 'COPYRIGHT', 'THIRD_PARTY_NOTICES.md')]
    documents.extend((ROOT / 'docs').glob('*.md'))
    documents.extend((ROOT / 'licenses').rglob('*') if (ROOT / 'licenses').is_dir() else [])
    for source in documents:
        if source.is_file():
            sources.append((source, 'share/niri-bridge/docs/' + str(source.relative_to(ROOT)), 0o644))
    return sources


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    parser.add_argument('--no-restart', action='store_true', help='Install files without restarting an active backend.')
    parser.add_argument('--skip-systemd', action='store_true', help='Stage application files without accessing user services.')
    args = parser.parse_args()
    if os.geteuid() == 0 and not args.skip_systemd:
        parser.exit(1, 'Run this installer as your desktop user. Administrator access is requested separately for input permissions.\n')
    prefix = args.prefix.expanduser().resolve()
    if any(character in str(prefix) for character in ('\n', '\r', '\0')):
        parser.error('The installation path contains an unsupported character.')
    sources = installation_sources()
    if not all(source.is_file() for source, _, _ in sources) or not all((ROOT / name).is_file() for name in ('LICENSE', 'COPYRIGHT', 'THIRD_PARTY_NOTICES.md')):
        parser.exit(1, 'The installation files are incomplete. Download the complete release archive or build from source: cargo build --locked --release\n')
    if not (ROOT / 'service/niri-bridge.service').is_file():
        parser.exit(1, 'The user service template is missing. Download the complete release archive.\n')
    check = subprocess.run(['/usr/bin/python3', '-c', "import gi; gi.require_version('Gtk','3.0'); gi.require_foreign('cairo'); from gi.repository import Gtk,Gio; import cairo"], capture_output=True)
    if check.returncode:
        parser.exit(1, 'GTK 3, PyGObject and Cairo are required. On Ubuntu install python3-gi python3-gi-cairo gir1.2-gtk-3.0.\n')
    manifest_path = prefix / MANIFEST
    try:
        previous = json.loads(manifest_path.read_text()) if manifest_path.exists() else {}
    except (OSError, ValueError):
        parser.exit(1, 'The existing installation record could not be read. No application files were changed.\n')
    if not isinstance(previous, dict) or (previous and (previous.get('format') != 1 or previous.get('application') != 'niri-bridge')):
        parser.exit(1, 'The existing installation record is not recognized. It was preserved.\n')
    if previous.get('unit') is not None and (not isinstance(previous['unit'], dict) or not isinstance(previous['unit'].get('sha256'), str)):
        parser.exit(1, 'The user-service installation record is invalid. No application files were changed.\n')
    for _, relative, _ in sources:
        if not (prefix / relative).parent.resolve().is_relative_to(prefix):
            parser.exit(1, 'An installation directory points outside its prefix. No application files were changed.\n')
    destination = prefix / 'bin/niri-bridge'
    changed = not destination.exists() or digest(destination) != digest(sources[0][0])
    installed = {}
    for source, relative, mode in sources:
        path = prefix / relative
        atomic_copy(source, path, mode)
        installed[relative] = digest(path)
    ui = prefix / 'share/niri-bridge/ui'
    launcher = prefix / 'bin/niri-bridge-ui'
    command = ['env', 'NIRI_BRIDGE_BIN=' + str(destination),
               'NIRI_BRIDGE_INPUT_HELPER=' + str(prefix / 'bin/niri-bridge-setup-input'),
               '/usr/bin/python3', '-B', str(ui / 'app.py')]
    write_file(launcher, '#!/bin/sh\nexec ' + shlex.join(command) + ' "$@"\n', 0o755)
    installed[str(launcher.relative_to(prefix))] = digest(launcher)
    uninstaller = prefix / 'bin/niri-bridge-uninstall'
    command = ['/usr/bin/python3', '-B', str(prefix / 'share/niri-bridge/installer/uninstall.py'), '--prefix', str(prefix)]
    write_file(uninstaller, '#!/bin/sh\nexec ' + shlex.join(command) + ' "$@"\n', 0o755)
    installed[str(uninstaller.relative_to(prefix))] = digest(uninstaller)
    desktop = prefix / 'share/applications/org.niribridge.NiriBridge.desktop'
    write_file(desktop, f'''[Desktop Entry]
Type=Application
Name=NiriBridge
GenericName=Keyboard and pointer sharing
GenericName[zh_CN]=键鼠共享
Comment=Share keyboard, mouse and touchpad gestures across Niri desktops
Comment[zh_CN]=跨 Niri 桌面共享键鼠与触摸板手势
Exec={desktop_exec(launcher)}
Icon=niri-bridge
Terminal=false
StartupNotify=true
StartupWMClass=org.niribridge.NiriBridge
Categories=Utility;
Keywords=keyboard;mouse;touchpad;Wayland;Niri;sharing;
Keywords[zh_CN]=键盘;鼠标;触摸板;共享;Niri;
''')
    if shutil.which('desktop-file-validate'):
        subprocess.run(['desktop-file-validate', str(desktop)], check=True)
    installed[str(desktop.relative_to(prefix))] = digest(desktop)
    unit_record = previous.get('unit')
    if not args.skip_systemd:
        unit = Path.home() / '.config/systemd/user/niri-bridge.service'
        owned = not os.path.lexists(unit) or (not unit.is_symlink() and unit_record and unit.is_file() and unit_record.get('sha256') == digest(unit))
        if owned:
            content = (ROOT / 'service/niri-bridge.service').read_text().replace('%h/.local/bin/niri-bridge', systemd_exec(destination))
            write_file(unit, content)
            unit_record = {'sha256': digest(unit)}
        elif not unit_record:
            print('Existing user service definition preserved.')
        subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
    write_file(manifest_path, json.dumps({'format': 1, 'application': 'niri-bridge', 'files': installed,
               'unit': unit_record}, indent=2, sort_keys=True) + '\n', 0o600)
    if not args.skip_systemd and changed and not args.no_restart:
        subprocess.run(['systemctl', '--user', 'try-restart', 'niri-bridge.service'], check=True)
    if shutil.which('update-desktop-database'):
        subprocess.run(['update-desktop-database', str(desktop.parent)], check=False, capture_output=True)
    print('NiriBridge and its desktop interface are installed.')
    print('Open NiriBridge from your application launcher, or run niri-bridge-ui.')
    print('Existing identities, pairing files, input permissions and settings were preserved.')
    print('To preview application removal: niri-bridge-uninstall --dry-run')


if __name__ == '__main__':
    main()
