#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Package hooks delegate user-file changes to the owning user, never to root."""
import os
from pathlib import Path
import pwd
import subprocess
import sys


def main():
    if os.geteuid() != 0 or sys.argv[1:] not in (['register'], ['unregister']):
        raise SystemExit('This helper is only for package maintenance.')
    helper = Path(__file__).resolve().with_name('package.py')
    failed = False
    for user in pwd.getpwall():
        home = Path(user.pw_dir)
        if not 1000 <= user.pw_uid < 65534 or not home.is_absolute() or not home.is_dir() or home.stat().st_uid != user.pw_uid:
            continue
        # Do not inspect a user-controlled record with root privileges.
        environment = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'USER': user.pw_name,
                       'LOGNAME': user.pw_name, 'XDG_RUNTIME_DIR': '/run/user/' + str(user.pw_uid),
                       'DBUS_SESSION_BUS_ADDRESS': 'unix:path=/run/user/' + str(user.pw_uid) + '/bus'}
        result = subprocess.run(['/usr/sbin/runuser', '-u', user.pw_name, '--', '/usr/bin/python3', '-I', str(helper),
                                 '--' + sys.argv[1] + '-user'], env=environment, capture_output=True)
        failed |= result.returncode != 0
    if failed:
        raise SystemExit('Some desktop installations could not be updated. Save edits and quit NiriBridge from each open session, then retry package maintenance.')


if __name__ == '__main__':
    main()
