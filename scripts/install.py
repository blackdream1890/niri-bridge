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
import lifecycle
from uninstall import safe_file, FIXED_FILES

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
               (ROOT / 'scripts/lifecycle.py', 'share/niri-bridge/installer/lifecycle.py', 0o644),
               (ROOT / 'ui/assets/niri-bridge.svg', 'share/icons/hicolor/scalable/apps/niri-bridge.svg', 0o644)]
    sources.extend((ROOT / 'ui' / name, 'share/niri-bridge/ui/' + name, 0o644) for name in UI_FILES)
    documents = [ROOT / name for name in ('README.md', 'README.zh-CN.md', 'CONTRIBUTING.md',
                 'CONTRIBUTING.zh-CN.md', 'SECURITY.md', 'SECURITY.zh-CN.md', 'LICENSE', 'COPYRIGHT', 'THIRD_PARTY_NOTICES.md', 'examples/bridge.toml')]
    documents.extend((ROOT / 'docs').rglob('*'))
    documents.extend((ROOT / 'licenses').rglob('*') if (ROOT / 'licenses').is_dir() else [])
    for source in documents:
        if source.is_file():
            sources.append((source, 'share/niri-bridge/docs/' + str(source.relative_to(ROOT)), 0o644))
    return sources


def backup_installation(prefix, previous, sources):
    if not previous and not any((prefix / name).exists() for name in FIXED_FILES):
        return None
    base = Path.home() / '.local/state/niri-bridge/backups'
    base.mkdir(parents=True, exist_ok=True, mode=0o700)
    backup = Path(tempfile.mkdtemp(prefix='upgrade-', dir=base))
    names = set(previous.get('files', {})) | {relative for _, relative, _ in sources} | {str(MANIFEST)} | FIXED_FILES
    for name in sorted(names):
        source = safe_file(prefix, name)
        if source.is_file() and not source.is_symlink():
            atomic_copy(source, backup / 'application' / name, source.stat().st_mode & 0o777)
    for name in ('niri-bridge.service', 'niri-bridge-ui.service'):
        source = Path.home() / '.config/systemd/user' / name
        if source.is_file() and not source.is_symlink():
            atomic_copy(source, backup / 'services' / name)
    config = Path.home() / '.config/niri-bridge/config.toml'
    if config.is_file() and not config.is_symlink():
        atomic_copy(config, backup / 'config.toml', 0o600)
    return backup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    parser.add_argument('--no-restart', action='store_true', help='Deprecated compatibility option; installation always leaves sharing stopped.')
    parser.add_argument('--skip-systemd', action='store_true', help='Stage application files without accessing user services.')
    parser.add_argument('--package', action='store_true', help='Use the system package launcher for desktop and login startup.')
    args = parser.parse_args()
    if os.geteuid() == 0 and not args.skip_systemd:
        parser.exit(1, 'Run this installer as your desktop user. Administrator access is requested separately for input permissions.\n')
    prefix = args.prefix.expanduser().resolve()
    if any(character in str(prefix) for character in ('\n', '\r', '\0')):
        parser.error('The installation path contains an unsupported character.')
    sources = installation_sources()
    if not all(source.is_file() for source, _, _ in sources) or not all((ROOT / name).is_file() for name in ('LICENSE', 'COPYRIGHT', 'THIRD_PARTY_NOTICES.md')):
        parser.exit(1, 'The installation files are incomplete. Download the complete release archive or build from source: cargo build --locked --release\n')
    if not all((ROOT / 'service' / name).is_file() for name in ('niri-bridge.service', 'niri-bridge-ui.service')):
        parser.exit(1, 'The user service template is missing. Download the complete release archive.\n')
    check = subprocess.run(['/usr/bin/python3', '-c', "import gi; gi.require_version('Gtk','3.0'); gi.require_foreign('cairo'); from gi.repository import Gtk,Gio; import cairo"], capture_output=True)
    if check.returncode:
        parser.exit(1, 'GTK 3, PyGObject and Cairo are required. On Ubuntu install python3-gi python3-gi-cairo gir1.2-gtk-3.0.\n')
    if not (prefix / MANIFEST).parent.resolve().is_relative_to(prefix):
        parser.exit(1, 'The installation directory points outside its prefix. No files were changed.\n')
    with lifecycle.installation_lock(prefix):
        manifest_path = prefix / MANIFEST
        try:
            previous = json.loads(manifest_path.read_text()) if manifest_path.exists() else {}
        except (OSError, ValueError):
            parser.exit(1, 'The existing installation record could not be read. No application files were changed.\n')
        if not isinstance(previous, dict) or (previous and (previous.get('format') != 1 or previous.get('application') != 'niri-bridge' or not isinstance(previous.get('files'), dict))):
            parser.exit(1, 'The existing installation record is not recognized. It was preserved.\n')
        if any(previous.get(key) is not None and (not isinstance(previous[key], dict) or not isinstance(previous[key].get('sha256'), str)) for key in ('unit', 'ui_unit')):
            parser.exit(1, 'The user-service installation record is invalid. No application files were changed.\n')
        for relative in set(previous.get('files', {})) | {relative for _, relative, _ in sources} | FIXED_FILES:
            try:
                safe_file(prefix, relative)
            except ValueError:
                parser.exit(1, 'An installation path is not safe. No application files were changed.\n')
            if not (prefix / relative).parent.resolve().is_relative_to(prefix):
                parser.exit(1, 'An installation directory points outside its prefix. No application files were changed.\n')
        package_record = {}
        if args.package:
            from package import fingerprint
            package_record['package_fingerprint'] = fingerprint()
        destination = prefix / 'bin/niri-bridge'
        startup_enabled = False
        if not args.skip_systemd:
            try:
                lifecycle.close_interface()
                model = lifecycle.desktop_model(sources[0][0])
                startup_enabled = model.service().get('UnitFileState') in ('enabled', 'enabled-runtime')
                lifecycle.stop_sharing(sources[0][0], restore_legacy=bool(destination.exists() and not previous.get('safe_touchpad_release')))
            except Exception as error:
                parser.exit(1, 'Installation paused before replacing files: ' + str(error) + '\n')
        backup = backup_installation(prefix, previous, sources)
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
Exec={desktop_exec(Path('/usr/bin/niri-bridge-ui') if args.package else launcher)}
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
        records = {key: previous.get(key) for key in ('unit', 'ui_unit')}
        if not args.skip_systemd:
            for name, key, executable in (
                    ('niri-bridge.service', 'unit', destination),
                    ('niri-bridge-ui.service', 'ui_unit', Path('/usr/bin/niri-bridge-ui') if args.package else launcher)):
                unit = Path.home() / '.config/systemd/user' / name
                record = records[key]
                owned = not os.path.lexists(unit) or (not unit.is_symlink() and record and unit.is_file() and record.get('sha256') == digest(unit))
                if owned:
                    original = '%h/.local/bin/' + ('niri-bridge' if key == 'unit' else 'niri-bridge-ui')
                    content = (ROOT / 'service' / name).read_text().replace(original, systemd_exec(executable))
                    write_file(unit, content)
                    records[key] = {'sha256': digest(unit)}
                else:
                    print('Existing custom user service definition preserved: ' + name)
            subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
            subprocess.run(['systemctl', '--user', 'disable', 'niri-bridge.service'], check=True)
            if startup_enabled:
                subprocess.run(['systemctl', '--user', 'enable', 'niri-bridge-ui.service'], check=True)
        if args.package and package_record['package_fingerprint'] != fingerprint():
            parser.exit(1, 'The system package changed during setup. Reopen NiriBridge to finish installing the current version.\n')
        write_file(manifest_path, json.dumps({'format': 1, 'application': 'niri-bridge', 'files': installed,
                   **records, **package_record, 'safe_touchpad_release': True, 'package': args.package,
                   'source_sha256': digest(sources[0][0])}, indent=2, sort_keys=True) + '\n', 0o600)
        if args.package:
            (prefix / 'share/niri-bridge/package-registration.json').unlink(missing_ok=True)
        if shutil.which('update-desktop-database'):
            subprocess.run(['update-desktop-database', str(desktop.parent)], check=False, capture_output=True)
        print('NiriBridge application files staged; user services were not accessed.' if args.skip_systemd else 'NiriBridge and its desktop interface are installed. Sharing is stopped.')
        if backup:
            print('Previous application files and settings were backed up to: ' + str(backup))
        print('Open NiriBridge from your application launcher, or run niri-bridge-ui.')
        print('Existing identities, pairing files, input permissions and settings were preserved.')
        print('To preview application removal: niri-bridge-uninstall --dry-run')


if __name__ == '__main__':
    main()
