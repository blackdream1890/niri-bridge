#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Plan and install narrowly scoped, active-session input access for NiriBridge."""
import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import stat
import subprocess
import tempfile
import tomllib

RULE = Path('/etc/udev/rules.d/71-niri-bridge-input.rules')
STATE = Path('/var/lib/niri-bridge/device-access.json')
CLASSES = ('ID_INPUT_KEYBOARD', 'ID_INPUT_MOUSE', 'ID_INPUT_TOUCHPAD')


def command(args):
    result = subprocess.run(args, text=True, capture_output=True,
                            env=dict(os.environ, LC_ALL='C'), timeout=20)
    if result.returncode:
        raise RuntimeError(f'{args[0]} failed; no password or raw device data was logged')
    return result.stdout


def device_info(path):
    original = Path(path)
    resolved = original.resolve(strict=True)
    if not str(resolved).startswith('/dev/input/event') or not stat.S_ISCHR(resolved.stat().st_mode):
        raise ValueError('Activity paths must resolve to input event devices')
    props = dict(line.split('=', 1) for line in command(
        ['udevadm', 'info', '--query=property', '--name=' + str(resolved)]
    ).splitlines() if '=' in line)
    identity = props.get('ID_PATH', '')
    kinds = [kind for kind in CLASSES if props.get(kind) == '1']
    if not identity or not re.fullmatch(r'[A-Za-z0-9:._/-]+', identity) or not kinds:
        raise ValueError('Device lacks a supported physical path and input classification')
    return {'path': str(original), 'id_path': identity, 'classes': kinds,
            'sys_path': '/sys' + props['DEVPATH']}


def render_rules(devices):
    lines = {'SUBSYSTEM=="misc", KERNEL=="uinput", TAG+="uaccess", OPTIONS+="static_node=uinput"'}
    for device in devices:
        identity = device['id_path']
        if not re.fullmatch(r'[A-Za-z0-9:._/-]+', identity):
            raise ValueError('Unsafe device identifier')
        for kind in device['classes']:
            if kind not in CLASSES:
                raise ValueError('Unsupported device classification')
            lines.add(f'SUBSYSTEM=="input", KERNEL=="event*", ENV{{ID_PATH}}=="{identity}", ENV{{{kind}}}=="1", TAG+="uaccess"')
    return '# Managed by NiriBridge device-access.py\n' + '\n'.join(sorted(lines)) + '\n'


def stable_identity(device):
    return {key: device[key] for key in ('path', 'id_path', 'classes')}


def boot_marker():
    # A private hash is used only to avoid restoring stale ACLs after a reboot.
    return hashlib.sha256(Path('/proc/sys/kernel/random/boot_id').read_bytes()).hexdigest()


def lan_address(value):
    if value is None:
        return None
    address = ipaddress.ip_address(value)
    networks = ('10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16')
    if address.version != 4 or not any(address in ipaddress.ip_network(n) for n in networks):
        raise ValueError('Firewall exception must identify one private IPv4 peer')
    return str(address)


def write_atomic(path, content, mode):
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix='.niri-bridge-', delete=False) as handle:
        temporary = Path(handle.name)
        try:
            os.fchmod(handle.fileno(), mode)
            handle.write(content.encode())
            handle.flush()
            os.fsync(handle.fileno())
            os.replace(temporary, path)
        finally:
            if temporary.exists():
                temporary.unlink()


def plan(config, destination, allow_from):
    settings = tomllib.loads(Path(config).read_text())
    paths = settings.get('activity_devices', [])
    if not isinstance(paths, list) or not 1 <= len(paths) <= 32:
        raise ValueError('Expected one to 32 physical input paths')
    devices = [stable_identity(device_info(path)) for path in paths]
    data = {'version': 1, 'devices': devices, 'rules': render_rules(devices),
            'allow_from': lan_address(allow_from)}
    destination = Path(destination)
    if destination.exists():
        raise ValueError('Existing plan was preserved; choose a new destination')
    with destination.open('x') as handle:
        os.fchmod(handle.fileno(), 0o600)
        handle.write(json.dumps(data, indent=2) + '\n')
    print(f'Prepared input access for {len(devices)} physical devices and uinput.')
    print('A peer-only TCP 42420 firewall exception is included.' if allow_from else 'No firewall change is included.')


def list_devices(config):
    selected = []
    if config and Path(config).is_file():
        selected = tomllib.loads(Path(config).read_text()).get('activity_devices', [])
    selected_nodes = {str(Path(p).resolve()) for p in selected}
    paths = list(map(Path, selected))
    for directory in ('/dev/input/by-path', '/dev/input/by-id'):
        paths.extend(sorted(Path(directory).glob('*')))
    seen, result = set(), []
    for path in paths:
        try:
            node = path.resolve(strict=True)
            if str(node) in seen:
                continue
            info = device_info(path)
            name = (Path('/sys/class/input') / node.name / 'device/name').read_text().strip()
            if name.startswith('NiriBridge '):
                continue
            seen.add(str(node))
            result.append({'path': str(path), 'name': name,
                           'classes': info['classes'], 'selected': str(node) in selected_nodes,
                           'readable': os.access(path, os.R_OK), 'writable': os.access(path, os.W_OK), 'available': True})
        except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired):
            if str(path) in selected and str(path) not in seen:
                seen.add(str(path))
                result.append({'path': str(path), 'name': '暂未连接的设备', 'classes': [],
                               'selected': True, 'readable': False, 'writable': False, 'available': False})
            continue
    print(json.dumps({'devices': result, 'uinput_writable': os.access('/dev/uinput', os.W_OK)}))


def verify_plan(path):
    data = json.loads(Path(path).read_text())
    if set(data) != {'version', 'devices', 'rules', 'allow_from'} or data['version'] != 1:
        raise ValueError('Unsupported access plan')
    if not 1 <= len(data['devices']) <= 32:
        raise ValueError('Invalid device count')
    actual = [stable_identity(device_info(d['path'])) for d in data['devices']]
    if actual != data['devices'] or render_rules(actual) != data['rules']:
        raise ValueError('Device information changed; prepare a new plan before installing')
    data['allow_from'] = lan_address(data['allow_from'])
    return data


def snapshot(path):
    path = Path(path).resolve(strict=True)
    info = path.stat()
    return {'path': str(path), 'inode': info.st_ino, 'rdev': info.st_rdev,
            'acl': command(['getfacl', '--absolute-names', str(path)])}


def refresh(devices):
    command(['udevadm', 'control', '--reload-rules'])
    command(['udevadm', 'trigger', '--action=change', '--subsystem-match=misc', '--sysname-match=uinput'])
    for device in devices:
        if Path(device['path']).exists():
            current = device_info(device['path'])
            if stable_identity(current) != device:
                raise ValueError('A physical input path now identifies a different device')
            command(['udevadm', 'trigger', '--action=change', current['sys_path']])
    command(['udevadm', 'settle', '--timeout=10'])


def require_root_state(create=True):
    if os.geteuid() != 0:
        raise PermissionError('This step requires Ubuntu administrator authentication through sudo')
    if create:
        STATE.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    elif not STATE.parent.exists():
        raise ValueError('No managed installation was found')
    info = STATE.parent.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077:
        raise ValueError('The rollback directory must be root-owned with mode 0700')
    if STATE.exists():
        info = STATE.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077:
            raise ValueError('The rollback state has unsafe ownership or permissions')


def install(path):
    data = verify_plan(path)
    require_root_state()
    command(['modprobe', 'uinput'])
    if STATE.exists():
        previous = json.loads(STATE.read_text())
        if not RULE.is_file() or hashlib.sha256(RULE.read_bytes()).hexdigest() != previous['rule_sha256']:
            raise ValueError('Existing rules were modified; they were preserved')
        if data != previous['plan']:
            raise ValueError('An existing installation has a different scope; uninstall it before changing access')
        state = previous
    else:
        if RULE.exists() or RULE.is_symlink():
            raise ValueError('An existing rule file was preserved')
        paths = ['/dev/uinput'] + [d['path'] for d in data['devices']]
        state = {'plan': data, 'snapshots': [snapshot(p) for p in paths], 'boot': boot_marker(),
                 'rule_sha256': hashlib.sha256(data['rules'].encode()).hexdigest(),
                 'firewall_tag': 'NiriBridge-' + secrets.token_hex(4)}
        write_atomic(STATE, json.dumps(state, indent=2) + '\n', 0o600)
    write_atomic(RULE, data['rules'], 0o644)
    refresh(data['devices'])
    if data['allow_from']:
        status = command(['ufw', 'status'])
        if 'Status: active' in status:
            command(['ufw', 'allow', 'proto', 'tcp', 'from', data['allow_from'],
                     'to', 'any', 'port', '42420', 'comment', state['firewall_tag']])
    print('NiriBridge input access installed for the active desktop session.')
    print('Existing firewall enablement was preserved; only the planned peer exception was requested.')


def uninstall():
    require_root_state(create=False)
    if not STATE.exists():
        raise ValueError('No managed installation was found')
    state = json.loads(STATE.read_text())
    if RULE.exists():
        if hashlib.sha256(RULE.read_bytes()).hexdigest() != state['rule_sha256']:
            raise ValueError('Rules were edited outside NiriBridge and were preserved')
        RULE.unlink()
    refresh(state['plan']['devices'])
    same_boot = state.get('boot') == boot_marker()
    for saved in state['snapshots'] if same_boot else []:
        path = Path(saved['path'])
        if path.exists():
            now = path.stat()
            if now.st_ino == saved['inode'] and now.st_rdev == saved['rdev']:
                result = subprocess.run(['setfacl', '--restore=-'], input=saved['acl'], text=True, capture_output=True)
                if result.returncode:
                    raise RuntimeError('Could not restore a saved device ACL')
    peer = state['plan']['allow_from']
    if peer and state['firewall_tag'] in command(['ufw', 'status']):
        command(['ufw', 'delete', 'allow', 'proto', 'tcp', 'from', peer, 'to', 'any',
                 'port', '42420', 'comment', state['firewall_tag']])
    STATE.unlink()
    try:
        STATE.parent.rmdir()
    except OSError:
        pass
    print('Managed rules removed; ACLs on unchanged device nodes were restored.')
    if not same_boot:
        print('This is a later boot. Reboot once to discard transient device ACLs; old ACL snapshots were not reapplied.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    prepare = sub.add_parser('plan')
    prepare.add_argument('--config', required=True)
    prepare.add_argument('--output', required=True)
    prepare.add_argument('--allow-from')
    apply = sub.add_parser('install')
    apply.add_argument('--plan', required=True)
    sub.add_parser('uninstall')
    devices = sub.add_parser('devices')
    devices.add_argument('--config')
    args = parser.parse_args()
    try:
        if args.action == 'plan':
            plan(args.config, args.output, args.allow_from)
        elif args.action == 'install':
            install(args.plan)
        elif args.action == 'devices':
            list_devices(args.config)
        else:
            uninstall()
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f'Input access setup stopped: {error}\n')


if __name__ == '__main__':
    main()
