# SPDX-License-Identifier: GPL-3.0-or-later
"""Desktop-facing operations. Worker threads return plain data; GTK stays on its main thread."""
from __future__ import annotations
from i18n import _, N_
import json
import copy
import os
from pathlib import Path
import socket
import subprocess
import tempfile

ERRORS = {
    'peer_offline': N_("The other computer is offline. Turn on sharing on both computers first."),
    'session_locked': N_("One computer is locked. Unlock it before changing screen connections."),
    'settings_conflict': N_("Settings changed elsewhere. Reload before saving."),
    'settings_busy': N_("Another settings change is being saved. Please try again shortly."),
    'busy': N_("The background service is busy. Please try again shortly."),
    'layout_unconfirmed': N_("The save result could not be confirmed on both computers. Wait for reconnection, then check the screen settings."),
    'config_invalid': N_("The configuration is incomplete or invalid. Check the connection and input device settings."),
    'config_exists': N_("This computer is already configured. Reload its existing settings."),
    'certificate_invalid': N_("This pairing file could not be read. Select the public certificate exported by the other computer."),
    'private_key_selected': N_("You selected a private key. Keep it on its original computer and select the exported pairing file instead."),
    'certificate_name_ambiguous': N_("This certificate does not have a unique device name. Export it again with NiriBridge."),
    'certificate_changed': N_("The pairing file changed after verification. Select it again and recheck the fingerprint."),
    'pair_same_device': N_("This is your own pairing file. Select the file exported by the other computer."),
    'output_unavailable': N_("The selected display is unavailable. Choose an active display."),
    'layout_invalid': N_("The entry range is invalid. Its start must be before its end."),
    'layout_overlap': N_("Connections overlap on the same screen edge. Adjust their ranges or remove a connection."),
    'layout_count_invalid': N_("Keep between 1 and 16 screen connections."),
    'layout_ids_invalid': N_("The connections could not be matched between computers. Reload and review both endpoints."),
    'layout_no_space': N_("There is no unused edge range. Shorten or remove a connection before adding another."),
    'backend_update_required': N_("Update NiriBridge on both computers to add more screen connections."),
    'stop_failed': N_("Sharing could not be stopped. NiriBridge is still open. Try Stop sharing again before quitting."),
    'input_selection_empty': N_("Select at least one keyboard, mouse or touchpad."),
    'input_path_invalid': N_("The input device path is unavailable. Select the device again from the list."),
    'address_invalid': N_("The address or port is invalid. Check it and try again."),
    'settings_write_failed': N_("Settings could not be saved. Check access to the configuration directory."),
    'settings_permissions': N_("Configuration permissions are incorrect. Your settings were not changed."),
    'backend_unavailable': N_("The background program is unavailable. Check that NiriBridge is installed."),
    'manual_instance': N_('NiriBridge was started outside the user service. Stop that instance before managing sharing here.'),
    'config_changed': N_('The running configuration changed. Reload the active configuration before continuing.'),
    'operation_failed': N_("The operation did not complete. Check the settings and device permissions, then try again."),
}


class OperationError(Exception):
    def __init__(self, code='operation_failed'):
        self.code = code
        super().__init__(_(ERRORS.get(code, ERRORS['operation_failed'])))


def endpoint(host, port):
    host = host.strip()
    if not host or any(c.isspace() for c in host) or not 1 <= int(port) <= 65535:
        raise OperationError('address_invalid')
    if ':' in host and not host.startswith('['):
        host = '[' + host + ']'
    return f'{host}:{int(port)}'


def split_endpoint(value):
    host, sep, port = value.rpartition(':')
    if not sep:
        return value, 42420
    return host.strip('[]'), int(port)


class Model:
    def __init__(self, config=None, binary=None, helper=None):
        self.explicit_config = config is not None
        self.config = Path(config or Path.home() / '.config/niri-bridge/config.toml')
        self.binary = str(binary or os.environ.get('NIRI_BRIDGE_BIN') or Path.home() / '.local/bin/niri-bridge')
        self.helper = str(helper or os.environ.get('NIRI_BRIDGE_INPUT_HELPER') or Path.home() / '.local/bin/niri-bridge-setup-input')
        self.control_path = str(Path(os.environ.get('XDG_RUNTIME_DIR', '/nonexistent')) / 'niri-bridge/control.sock')

    def command(self, arguments, timeout=12):
        try:
            result = subprocess.run(arguments, capture_output=True, text=True, timeout=timeout)
        except (OSError, subprocess.TimeoutExpired) as exc:
            raise OperationError('backend_unavailable') from exc
        if result.returncode:
            raise OperationError()
        return result.stdout

    def manage(self, action, *arguments):
        try:
            reply = json.loads(self.command([self.binary, 'manage', action, *map(str, arguments)]))
        except ValueError as exc:
            raise OperationError('backend_unavailable') from exc
        if not reply.get('ok'):
            raise OperationError(reply.get('error', 'operation_failed'))
        return reply.get('data', {})

    def rpc(self, request, timeout=12):
        request = dict(request)
        if request.get('command') != 'status':
            request['config_path'] = str(self.config.resolve())
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                client.settimeout(timeout)
                client.connect(self.control_path)
                client.sendall(json.dumps(request).encode() + b'\n')
                buffer = bytearray()
                while b'\n' not in buffer:
                    block = client.recv(65536)
                    if not block or len(buffer) + len(block) > 262144:
                        raise ValueError('invalid response')
                    buffer.extend(block)
                result = json.loads(buffer.split(b'\n', 1)[0])
        except (OSError, ValueError) as exc:
            raise OperationError('peer_offline') from exc
        if not result.get('ok'):
            raise OperationError(result.get('error', 'operation_failed'))
        return result['data']

    def focus_interface(self):
        try:
            windows = json.loads(self.command(['niri', 'msg', '--json', 'windows']))
            own = next((w for w in windows if w.get('app_id') == 'org.niribridge.NiriBridge'), None)
            if own and not own.get('is_focused'):
                self.command(['niri', 'msg', 'action', 'focus-window', '--id', str(own['id'])])
        except (OperationError, ValueError, KeyError):
            pass

    def service(self, unit='niri-bridge.service'):
        output = self.command(['systemctl', '--user', 'show', unit,
                               '--property=ActiveState', '--property=SubState', '--property=UnitFileState', '--property=MainPID', '--property=ExecMainStatus', '--property=ExecMainCode'])
        return dict(line.split('=', 1) for line in output.splitlines() if '=' in line)

    def poll(self):
        service = self.service()
        try:
            status = self.rpc({'command': 'status'}, timeout=1)
        except OperationError:
            status = None
        if status and status.get('config_path') and Path(status['config_path']).resolve() != self.config.resolve():
            status = dict(status, reason='config_changed')
        unmanaged = bool(status and str(status.get('pid', '')) != service.get('MainPID', ''))
        startup = self.service('niri-bridge-ui.service').get('UnitFileState')
        return {'service': service, 'status': status, 'unmanaged': unmanaged,
                'autostart': startup in ('enabled', 'enabled-runtime')}

    def load(self):
        if not self.explicit_config:
            try:
                status = self.rpc({'command': 'status'}, timeout=1)
                if status.get('config_path'):
                    self.config = Path(status['config_path'])
            except OperationError:
                pass
        config = self.manage('show', '--config', self.config) if self.config.exists() else None
        devices = json.loads(self.command(['/usr/bin/python3', self.helper, 'devices', '--config', str(self.config)]))
        try:
            outputs = json.loads(self.command(['niri', 'msg', '--json', 'outputs']))
            outputs = {name: item['logical'] for name, item in outputs.items() if item.get('logical')}
        except (OperationError, ValueError, KeyError):
            outputs = {}
        version = self.command([self.binary, '--version']).strip().rsplit(' ', 1)[-1]
        return {'config': config, 'devices': devices, 'outputs': outputs, 'version': version, **self.poll()}

    def ensure_managed(self):
        if self.poll().get('unmanaged'):
            raise OperationError('manual_instance')

    def controls_current_config(self):
        try:
            status = self.rpc({'command': 'status'}, timeout=1)
            return not status.get('config_path') or Path(status['config_path']).resolve() == self.config.resolve()
        except OperationError:
            return self.config.resolve() == (Path.home() / '.config/niri-bridge/config.toml').resolve()

    def set_running(self, running):
        self.ensure_managed()
        if not self.controls_current_config():
            raise OperationError('config_changed')
        previous = self.poll() if not running else None
        if running or previous['service'].get('ActiveState') not in ('inactive', 'failed'):
            self.command(['systemctl', '--user', 'start' if running else 'stop', 'niri-bridge.service'])
        result = self.poll()
        if not running and result['service'].get('ActiveState') not in ('inactive', 'failed'):
            raise OperationError('stop_failed')
        unclean_exit = result['service'].get('ExecMainCode') in ('2', '3')  # CLD_KILLED / CLD_DUMPED
        if previous and (previous['service'].get('ActiveState') in ('active', 'activating', 'deactivating') or unclean_exit):
            legacy = not (previous.get('status') or {}).get('safe_touchpad_release')
            failed_cleanup = result['service'].get('ExecMainStatus', '0') != '0'
            if (legacy or failed_cleanup or unclean_exit) and self.config.is_file():
                self.command([self.binary, 'restore-input', '--config', str(self.config)], timeout=20)
        return result

    def stop_for_exit(self):
        """Do not acknowledge an application exit while its managed sharing is active."""
        try:
            result = self.set_running(False)
            if result['service'].get('ActiveState') not in ('inactive', 'failed'):
                raise OperationError('stop_failed')
            return result
        except OperationError as error:
            if error.code in ('manual_instance', 'config_changed'):
                raise
            raise OperationError('stop_failed') from error

    def set_autostart(self, enabled):
        self.ensure_managed()
        self.command(['systemctl', '--user', 'disable', 'niri-bridge.service'])
        self.command(['systemctl', '--user', 'enable' if enabled else 'disable', 'niri-bridge-ui.service'])
        return self.poll()

    def release(self):
        return self.rpc({'command': 'release'})

    def apply_layout(self, layout):
        status = self.rpc({'command': 'status'})
        if not all('edges' in (status.get(role) or {}) for role in ('local', 'peer')):
            if len(layout['local_edges']) != 1 or len(layout['peer_edges']) != 1:
                raise OperationError('backend_update_required')
            layout = copy.deepcopy(layout)
            for role in ('local', 'peer'):
                edge = layout.pop(role + '_edges')[0]
                edge.pop('id', None)
                layout[role + '_edge'] = edge
        return self.rpc({'command': 'apply_layout', 'layout': layout})

    def _with_request(self, action, data, extra=()):
        directory = self.config.parent
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(mode='w', dir=directory, prefix='.ui-request-', suffix='.json') as request:
            json.dump(data, request)
            request.flush()
            return self.manage(action, '--config', self.config, '--request', request.name, *extra)

    def save_settings(self, revision, settings):
        self.ensure_managed()
        active = self.service().get('ActiveState') == 'active' and self.controls_current_config()
        result = self._with_request('save', {'revision': revision, 'settings': settings})
        if active:
            self.command(['systemctl', '--user', 'restart', 'niri-bridge.service'])
        return result

    def initialize(self, name, settings, edge):
        return self._with_request('initialize', {'settings': settings, 'edge': edge}, ('--name', name))

    def inspect_certificate(self, path):
        return self.manage('certificate', path)

    def import_peer(self, path, revision, fingerprint):
        self.ensure_managed()
        active = self.service().get('ActiveState') == 'active' and self.controls_current_config()
        if active:
            self.set_running(False)
        try:
            result = self.manage('import-peer', '--config', self.config, '--candidate', path,
                                 '--revision', revision, '--fingerprint', fingerprint)
        finally:
            if active:
                self.set_running(True)
        return result

    def grant_devices(self):
        # The existing reviewed installer owns all privileged behavior.
        with tempfile.TemporaryDirectory(prefix='niri-bridge-permissions-') as directory:
            plan = str(Path(directory) / 'plan.json')
            self.command(['/usr/bin/python3', self.helper, 'plan', '--config', str(self.config), '--output', plan])
            self.command(['pkexec', '/usr/bin/python3', self.helper, 'install', '--plan', plan], timeout=180)
        return self.load()

    def diagnostics(self):
        # doctor intentionally excludes peer addresses, identities, and input contents.
        return self.command([self.binary, 'doctor', '--json'])
