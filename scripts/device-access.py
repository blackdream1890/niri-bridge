#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Review and grant narrowly scoped input and LAN access for NiriBridge."""
import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import stat
import subprocess
import tempfile
import tomllib

RULE = Path('/etc/udev/rules.d/71-niri-bridge-input.rules')
STATE = Path('/var/lib/niri-bridge/device-access.json')
FIREWALL_STATE = Path('/var/lib/niri-bridge/firewall-access.json')
ROOT_UID = 0
CLASSES = ('ID_INPUT_KEYBOARD', 'ID_INPUT_MOUSE', 'ID_INPUT_TOUCHPAD')


class FirewallError(ValueError):
    """Stable UI errors never include privileged command output or local data."""
    def __init__(self, code):
        self.code = code
        super().__init__(code)


def firewall_command(arguments, error='firewall_unavailable'):
    try:
        return command(arguments)
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as cause:
        raise FirewallError(error) from cause


def validate_firewall_scope(data):
    keys = {'version', 'kind', 'peer', 'local', 'interface', 'port'}
    if (not isinstance(data, dict) or set(data) != keys
            or type(data['version']) is not int or data['version'] != 1
            or data['kind'] != 'lan-firewall'
            or type(data['port']) is not int or not 1 <= data['port'] <= 65535
            or not isinstance(data['interface'], str)
            or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.:-]{0,14}', data['interface'])):
        raise FirewallError('firewall_plan_invalid')
    try:
        for key in ('peer', 'local'):
            if not isinstance(data[key], str) or lan_address(data[key]) != data[key]:
                raise ValueError()
        if data['peer'] == data['local']:
            raise ValueError()
    except ValueError as cause:
        raise FirewallError('firewall_peer_invalid') from cause
    return data


def physical_interface(name):
    return (isinstance(name, str) and bool(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.:-]{0,14}', name))
            and (Path('/sys/class/net') / name / 'device').exists())


def network_info(config):
    """Expose usable local LAN choices without exposing hardware identities."""
    port, addresses = None, set()
    try:
        settings = tomllib.loads(Path(config).read_text())
        candidate = int(settings['connection']['address'].rsplit(':', 1)[1])
        if 1 <= candidate <= 65535:
            port = candidate
    except (OSError, AttributeError, KeyError, TypeError, ValueError):
        pass
    try:
        for interface in json.loads(command(['ip', '-j', 'address', 'show', 'up'])):
            name = interface.get('ifname')
            if not physical_interface(name) or not {'UP', 'LOWER_UP'}.issubset(interface.get('flags', [])):
                continue
            for item in interface.get('addr_info', []):
                if item.get('family') != 'inet':
                    continue
                try:
                    address = lan_address(item.get('local'))
                except ValueError:
                    continue
                if address:
                    addresses.add((name, address))
    except (OSError, AttributeError, TypeError, ValueError, RuntimeError, subprocess.TimeoutExpired):
        pass
    return {'ok': True, 'addresses': [{'address': address, 'interface': name}
                                     for name, address in sorted(addresses)], 'port': port}


def firewall_route(peer):
    try:
        if not isinstance(peer, str) or lan_address(peer) != peer:
            raise ValueError()
    except ValueError as cause:
        raise FirewallError('firewall_peer_invalid') from cause
    try:
        routes = json.loads(firewall_command(['ip', '-j', 'route', 'get', peer], 'firewall_route_changed'))
        if len(routes) != 1 or routes[0].get('type', 'unicast') != 'unicast':
            raise ValueError()
        route = routes[0]
        interface = route['dev']
        local = lan_address(route.get('prefsrc') or route.get('src'))
        if (not local or local == peer or not isinstance(interface, str)
                or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.:-]{0,14}', interface)):
            raise ValueError()
        interfaces = json.loads(firewall_command(
            ['ip', '-j', 'address', 'show', 'dev', interface], 'firewall_route_changed'))
        if (len(interfaces) != 1 or interfaces[0].get('ifname') != interface
                or not physical_interface(interface)
                or not {'UP', 'LOWER_UP'}.issubset(interfaces[0].get('flags', []))):
            raise ValueError()
        addresses = {item['local'] for item in interfaces[0].get('addr_info', [])
                     if item.get('family') == 'inet'}
        if local not in addresses:
            raise ValueError()
        return interface, local, addresses
    except (AttributeError, KeyError, TypeError, ValueError) as cause:
        if isinstance(cause, FirewallError):
            raise
        raise FirewallError('firewall_route_changed') from cause


def firewall_plan(config, destination, peer):
    try:
        settings = tomllib.loads(Path(config).read_text())
        connection = settings['connection']
        host, port = connection['address'].rsplit(':', 1)
        address = ipaddress.ip_address(host)
        if connection['mode'] != 'listen' or address.version != 4 or not 1 <= int(port) <= 65535:
            raise ValueError()
    except (KeyError, TypeError, ValueError) as cause:
        raise FirewallError('firewall_listen_required') from cause
    interface, local, addresses = firewall_route(peer)
    if not address.is_unspecified:
        if str(address) not in addresses:
            raise FirewallError('firewall_route_changed')
        local = str(address)
    data = validate_firewall_scope({'version': 1, 'kind': 'lan-firewall', 'peer': peer,
                                    'local': local, 'interface': interface, 'port': int(port)})
    content = (json.dumps(data, sort_keys=True, indent=2) + '\n').encode()
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, 'wb') as output:
        os.fchmod(output.fileno(), 0o600)
        output.write(content)
        output.flush()
        os.fsync(output.fileno())
    return {'ok': True, 'plan': data, 'sha256': hashlib.sha256(content).hexdigest()}


def read_firewall_file(path, limit, root_owned=False):
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, 'rb') as source:
            info = os.fstat(source.fileno())
            if (not stat.S_ISREG(info.st_mode) or info.st_size > limit
                    or (root_owned and (info.st_uid != ROOT_UID or info.st_mode & 0o077))):
                raise FirewallError('firewall_plan_invalid')
            content = source.read(limit + 1)
            if len(content) > limit:
                raise FirewallError('firewall_plan_invalid')
            return content
    except OSError as cause:
        raise FirewallError('firewall_plan_invalid') from cause


def verify_firewall_plan(path, expected, check_route=True):
    if not isinstance(expected, str) or not re.fullmatch(r'[a-f0-9]{64}', expected):
        raise FirewallError('firewall_plan_invalid')
    content = read_firewall_file(path, 8192)
    if not secrets.compare_digest(hashlib.sha256(content).hexdigest(), expected):
        raise FirewallError('firewall_plan_invalid')
    try:
        data = validate_firewall_scope(json.loads(content))
    except (UnicodeError, ValueError) as cause:
        if isinstance(cause, FirewallError):
            raise
        raise FirewallError('firewall_plan_invalid') from cause
    if check_route:
        interface, _local, addresses = firewall_route(data['peer'])
        if data['interface'] != interface or data['local'] not in addresses:
            raise FirewallError('firewall_route_changed')
    return data


def firewall_root_directory(create=True):
    if os.geteuid() != ROOT_UID:
        raise FirewallError('firewall_failed')
    directory = FIREWALL_STATE.parent
    if create:
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = directory.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != ROOT_UID or info.st_mode & 0o077:
        raise FirewallError('firewall_changed')


@contextmanager
def firewall_lock():
    firewall_root_directory()
    descriptor = os.open(FIREWALL_STATE.with_suffix('.lock'),
                         os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, 'r+b') as lock:
        info = os.fstat(lock.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != ROOT_UID or info.st_mode & 0o077:
            raise FirewallError('firewall_changed')
        try:
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as cause:
            raise FirewallError('firewall_busy') from cause
        try:
            yield
        finally:
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)


def firewall_key(plan):
    return tuple(plan[key] for key in ('peer', 'local', 'interface', 'port'))


def load_firewall_state():
    if not FIREWALL_STATE.exists() and not FIREWALL_STATE.is_symlink():
        return {'version': 1, 'rules': []}
    try:
        state = json.loads(read_firewall_file(FIREWALL_STATE, 262144, root_owned=True))
        if (set(state) != {'version', 'rules'} or type(state['version']) is not int
                or state['version'] != 1 or not isinstance(state['rules'], list)
                or len(state['rules']) > 128):
            raise ValueError()
        keys, tags = set(), set()
        for rule in state['rules']:
            if (set(rule) != {'plan', 'tag', 'status'} or rule['status'] not in ('pending', 'installed')
                    or not isinstance(rule['tag'], str)
                    or not re.fullmatch(r'NiriBridge-LAN-[a-f0-9]{16}', rule['tag'])):
                raise ValueError()
            key = firewall_key(validate_firewall_scope(rule['plan']))
            if key in keys or rule['tag'] in tags:
                raise ValueError()
            keys.add(key)
            tags.add(rule['tag'])
        return state
    except (KeyError, TypeError, ValueError) as cause:
        raise FirewallError('firewall_changed') from cause


def save_firewall_state(state):
    write_atomic(FIREWALL_STATE, json.dumps(state, indent=2) + '\n', 0o600)


def parse_firewall_rule(line):
    """Read UFW's normalized 'show added' form, including logging/action drift."""
    try:
        # UFW wraps comments in quotes without shell-escaping quotes inside them.
        # The comment is always last; parse the network tuple independently.
        scope, separator, comment = line.partition(' comment ')
        if separator:
            if len(comment) >= 2 and comment[0] == "'" and comment[-1] == "'":
                comment = comment[1:-1]
        tokens = shlex.split(scope)
        if len(tokens) < 2 or tokens[0] != 'ufw' or tokens[1] not in ('allow', 'deny', 'reject', 'limit'):
            return None
        result = {'action': tokens[1], 'interface': '', 'peer': 'any', 'local': 'any',
                  'port': 'any', 'protocol': 'any', 'comment': comment if separator else '', 'log': ''}
        index = 2
        while index < len(tokens):
            token = tokens[index]
            if token == 'in':
                if tokens[index + 1] != 'on':
                    return None
                result['interface'] = tokens[index + 2]
                index += 3
            elif token in ('log', 'log-all'):
                result['log'] = token
                index += 1
            elif token in ('from', 'to'):
                key = 'peer' if token == 'from' else 'local'
                result[key] = tokens[index + 1]
                index += 2
                if index < len(tokens) and tokens[index] == 'port':
                    if token == 'from':
                        if tokens[index + 1] != 'any':
                            return None
                    else:
                        result['port'] = tokens[index + 1]
                    index += 2
            elif token == 'proto':
                result['protocol'] = tokens[index + 1]
                index += 2
            else:
                return None
        for key in ('peer', 'local'):
            network = ipaddress.ip_network(result[key], strict=False)
            if network.version != 4 or network.num_addresses != 1:
                return None
            result[key] = str(network.network_address)
        if result.pop('protocol') != 'tcp':
            return None
        result['port'] = int(result['port'])
        return result
    except (IndexError, ValueError):
        return None


def firewall_inventory():
    output = firewall_command(['ufw', 'show', 'added'])
    return [(line, parse_firewall_rule(line)) for line in output.splitlines() if line.startswith('ufw ')]


def matching_firewall_rules(inventory, plan):
    return [rule for _line, rule in inventory if rule and firewall_key(rule) == firewall_key(plan)]


def owned_firewall_rule(inventory, entry):
    tagged = [(line, rule) for line, rule in inventory if entry['tag'] in line]
    if not tagged:
        return False
    if (len(tagged) != 1 or not tagged[0][1]
            or tagged[0][1]['comment'] != entry['tag']
            or firewall_key(tagged[0][1]) != firewall_key(entry['plan'])
            or tagged[0][1]['action'] != 'allow' or tagged[0][1]['log']):
        raise FirewallError('firewall_changed')
    return True


def firewall_arguments(plan, tag):
    return ['allow', 'in', 'on', plan['interface'], 'from', plan['peer'],
            'to', plan['local'], 'port', str(plan['port']), 'proto', 'tcp', 'comment', tag]


def firewall_install(path, expected):
    if os.geteuid() != ROOT_UID:
        raise FirewallError('firewall_failed')
    data = verify_firewall_plan(path, expected)
    if 'Status: active' not in firewall_command(['ufw', 'status']):
        raise FirewallError('firewall_inactive')
    with firewall_lock():
        state = load_firewall_state()
        inventory = firewall_inventory()
        entry = next((r for r in state['rules'] if firewall_key(r['plan']) == firewall_key(data)), None)
        if entry and owned_firewall_rule(inventory, entry):
            entry['status'] = 'installed'
            save_firewall_state(state)
            return {'ok': True, 'already_allowed': True, 'managed': True}
        matches = matching_firewall_rules(inventory, data)
        if matches:
            if entry or any(rule['action'] != 'allow' for rule in matches):
                raise FirewallError('firewall_changed')
            # UFW replaces comments on an otherwise equal rule: never adopt it.
            return {'ok': True, 'already_allowed': True, 'managed': False}
        if entry is None:
            if len(state['rules']) >= 128:
                raise FirewallError('firewall_failed')
            entry = {'plan': data, 'tag': 'NiriBridge-LAN-' + secrets.token_hex(8), 'status': 'pending'}
            state['rules'].append(entry)
        entry['status'] = 'pending'
        save_firewall_state(state)
        firewall_command(['ufw', *firewall_arguments(data, entry['tag'])], 'firewall_failed')
        if not owned_firewall_rule(firewall_inventory(), entry):
            raise FirewallError('firewall_failed')
        entry['status'] = 'installed'
        save_firewall_state(state)
        return {'ok': True, 'already_allowed': False, 'managed': True}


def firewall_remove(plan=None):
    if os.geteuid() != ROOT_UID:
        raise FirewallError('firewall_failed')
    if not FIREWALL_STATE.parent.exists():
        return {'ok': True, 'removed': 0}
    with firewall_lock():
        state = load_firewall_state()
        inventory = firewall_inventory()
        selected = [entry for entry in state['rules']
                    if plan is None or firewall_key(entry['plan']) == firewall_key(plan)]
        for entry in selected:
            if not owned_firewall_rule(inventory, entry) and matching_firewall_rules(inventory, entry['plan']):
                raise FirewallError('firewall_changed')
        removed = 0
        for entry in selected:
            if owned_firewall_rule(inventory, entry):
                firewall_command(['ufw', 'delete', *firewall_arguments(entry['plan'], entry['tag'])], 'firewall_failed')
                inventory = firewall_inventory()
                if owned_firewall_rule(inventory, entry):
                    raise FirewallError('firewall_failed')
                removed += 1
            state['rules'].remove(entry)
            save_firewall_state(state)
        if not state['rules'] and FIREWALL_STATE.exists():
            FIREWALL_STATE.unlink()
        return {'ok': True, 'removed': removed}


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
    has_firewall = FIREWALL_STATE.exists() or FIREWALL_STATE.is_symlink()
    if not STATE.exists() and not has_firewall:
        raise ValueError('No managed installation was found')
    if has_firewall:
        firewall_remove()
    if not STATE.exists():
        print('Managed LAN firewall rules removed; input permissions were unchanged.')
        return
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
    network = sub.add_parser('network-info')
    network.add_argument('--config', required=True)
    firewall_prepare = sub.add_parser('firewall-plan')
    firewall_prepare.add_argument('--config', required=True)
    firewall_prepare.add_argument('--peer', required=True)
    firewall_prepare.add_argument('--output', required=True)
    for action in ('firewall-install', 'firewall-remove'):
        firewall_apply = sub.add_parser(action)
        firewall_apply.add_argument('--plan', required=True)
        firewall_apply.add_argument('--sha256', required=True)
    args = parser.parse_args()
    try:
        if args.action == 'plan':
            plan(args.config, args.output, args.allow_from)
        elif args.action == 'install':
            install(args.plan)
        elif args.action == 'devices':
            list_devices(args.config)
        elif args.action == 'network-info':
            print(json.dumps(network_info(args.config)))
        elif args.action == 'firewall-plan':
            print(json.dumps(firewall_plan(args.config, args.output, args.peer)))
        elif args.action == 'firewall-install':
            print(json.dumps(firewall_install(args.plan, args.sha256)))
        elif args.action == 'firewall-remove':
            data = verify_firewall_plan(args.plan, args.sha256, check_route=False)
            print(json.dumps(firewall_remove(data)))
        else:
            uninstall()
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        if args.action.startswith('firewall-'):
            print(json.dumps({'ok': False, 'error': error.code if isinstance(error, FirewallError) else 'firewall_failed'}))
            parser.exit(1)
        parser.exit(1, f'Input access setup stopped: {error}\n')


if __name__ == '__main__':
    main()
