#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Render documentation screenshots using synthetic data on an isolated display."""
import argparse
from pathlib import Path
import sys
from unittest import mock

from app import Application, GLib
from test_ui import FakeModel


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--language', choices=('en', 'zh_CN'), default='en')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    for name in ('overview', 'layout', 'pair', 'preferences'):
        (args.output / (name + '.png')).unlink(missing_ok=True)
    model = FakeModel()
    app = Application(model, language=args.language)
    app.set_application_id('org.niribridge.NiriBridge.Demo')
    def ready():
        window = app.window
        if window.info is None or window.busy:
            return True
        window.resize(1120, 1080)
        window.render_dir = args.output
        GLib.timeout_add(300, window.render_pages)
        GLib.timeout_add(100, done)
        return False
    def done():
        if all((args.output / (name + '.png')).is_file() for name in ('overview', 'layout', 'pair', 'preferences')):
            app.quit()
            return False
        return True
    with mock.patch('app.Window.setup_tray'):
        app.connect('activate', lambda *_: GLib.timeout_add(100, ready))
        try:
            status = app.run([sys.argv[0]])
        finally:
            model.directory.cleanup()
    if model.actions:
        raise RuntimeError('Documentation rendering must not perform backend actions')
    return status


if __name__ == '__main__':
    raise SystemExit(main())
