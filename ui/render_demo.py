#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Render documentation screenshots using synthetic data on an isolated display."""
import argparse
from pathlib import Path
import sys
from unittest import mock

from app import Application, GLib, Gtk
from test_ui import FakeModel
from canvas import LayoutDraft


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--language', choices=('en', 'zh_CN'), default='en')
    parser.add_argument('--gtk-theme', help='Optional theme for isolated visual regression checks')
    parser.add_argument('--decoration-layout', help='Optional GTK title-button layout for isolated checks')
    args = parser.parse_args()
    settings = Gtk.Settings.get_default()
    if args.gtk_theme:
        settings.set_property('gtk-theme-name', args.gtk_theme)
    if args.decoration_layout:
        settings.set_property('gtk-decoration-layout', args.decoration_layout)
    args.output.mkdir(parents=True, exist_ok=True)
    for name in ('overview', 'layout', 'pair', 'preferences'):
        (args.output / (name + '.png')).unlink(missing_ok=True)
    model = FakeModel()
    draft = LayoutDraft(model.data['status']['local'], model.data['status']['peer'])
    draft.add_connection()
    for role in ('local', 'peer'):
        model.data['status'][role]['edges'] = getattr(draft, role)['edges']
    model.data['config']['edges'] = draft.local['edges']
    app = Application(model, language=args.language)
    app.set_application_id('org.niribridge.NiriBridge.Demo')
    def ready():
        window = app.window
        if window.info is None or window.busy:
            return True
        window.connection_combo.set_active_id(draft.selected_id)
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
    with mock.patch('app.Window.setup_tray'), mock.patch.object(model, 'stop_for_exit', side_effect=model.poll):
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
