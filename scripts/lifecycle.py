# SPDX-License-Identifier: GPL-3.0-or-later
"""Coordinate desktop shutdown before replacing or removing a user installation."""
import importlib.util
import fcntl
import os
import socket
from pathlib import Path
import sys
import time

APP_ID = 'org.niribridge.NiriBridge'
APP_PATH = '/org/niribridge/NiriBridge'


class InterfaceOpen(RuntimeError):
    pass


def installation_lock(prefix):
    """A process-held lock prevents overlapping installation and removal writes."""
    state = Path.home() / '.local/state/niri-bridge'
    state.mkdir(parents=True, exist_ok=True)
    path = state / 'installation.lock'
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        os.close(fd)
        raise RuntimeError('Another installation or removal is running. Wait for it to finish, then retry.') from None
    return os.fdopen(fd, 'w')


def ensure_offline():
    runtime = Path(os.environ.get('XDG_RUNTIME_DIR', '/nonexistent'))
    if (runtime / 'bus').exists():
        raise RuntimeError('An active desktop session requires normal application removal.')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        try:
            client.connect(str(runtime / 'niri-bridge/control.sock'))
        except (FileNotFoundError, ConnectionRefusedError):
            return
    raise RuntimeError('Stop the manually started sharing process before removing the package.')


def close_interface(timeout=45):
    from gi.repository import Gio, GLib
    connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(destination, path, interface, method, parameters):
        return connection.call_sync(destination, path, interface, method, parameters,
                                    None, Gio.DBusCallFlags.NO_AUTO_START, 3000, None)

    def is_open():
        return call('org.freedesktop.DBus', '/org/freedesktop/DBus',
                    'org.freedesktop.DBus', 'NameHasOwner', GLib.Variant('(s)', (APP_ID,))).unpack()[0]

    if not is_open():
        return
    actions = call(APP_ID, APP_PATH, 'org.gtk.Actions', 'DescribeAll', None).unpack()[0]
    if 'quit' not in actions:
        raise InterfaceOpen('Save your edits and choose Quit from the NiriBridge tray menu, then try again.')
    call(APP_ID, APP_PATH, 'org.gtk.Actions', 'Activate', GLib.Variant('(sava{sv})', ('quit', [], {})))
    deadline = time.monotonic() + timeout
    while is_open():
        if time.monotonic() >= deadline:
            raise InterfaceOpen('NiriBridge is still open. Save your edits and quit from the tray, then try again.')
        time.sleep(0.2)


def desktop_model(binary):
    # Both the release tree and installed installer directory have a sibling ui/.
    directory = Path(__file__).resolve().parent.parent / 'ui'
    sys.path.insert(0, str(directory))
    spec = importlib.util.spec_from_file_location('niri_bridge_install_model', directory / 'model.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.Model(binary=binary)


def stop_sharing(binary, restore_legacy=False):
    model = desktop_model(binary)
    previous = model.poll()
    model.stop_for_exit()
    # An older GUI may have stopped its backend before this installer started.
    # Complete the neutral touchpad handoff even in that upgrade case.
    if restore_legacy and previous['service'].get('ActiveState') not in ('active', 'activating', 'deactivating') and previous['service'].get('ExecMainCode') not in ('2', '3') and model.config.is_file():
        model.command([str(binary), 'restore-input', '--config', str(model.config)], timeout=20)
