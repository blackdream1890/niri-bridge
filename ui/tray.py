# SPDX-License-Identifier: GPL-3.0-or-later
"""Wayland-friendly StatusNotifierItem and a small DBusMenu, using the existing GIO runtime.
Protocol references: KDE kstatusnotifieritem and com.canonical.dbusmenu.
"""
from pathlib import Path
import gi
gi.require_version('Gio', '2.0')
gi.require_version('GdkPixbuf', '2.0')
from gi.repository import Gio, GLib, GdkPixbuf

SNI = 'org.kde.StatusNotifierItem'
MENU = 'com.canonical.dbusmenu'
WATCHER = 'org.kde.StatusNotifierWatcher'
PATH = '/StatusNotifierItem'
MENU_PATH = '/NiriBridgeMenu'


def method(name, ins=(), outs=()):
    return '<method name="' + name + '">' + ''.join(f'<arg type="{kind}" direction="{direction}"/>' for direction, kinds in [('in', ins), ('out', outs)] for kind in kinds) + '</method>'


def interface_xml(name, properties, methods, signals):
    return '<node><interface name="' + name + '">' + ''.join(f'<property name="{key}" type="{kind}" access="read"/>' for key, kind in properties.items()) + ''.join(method(*item) for item in methods) + ''.join('<signal name="' + key + '">' + ''.join(f'<arg type="{kind}"/>' for kind in kinds) + '</signal>' for key, kinds in signals.items()) + '</interface></node>'


SNI_PROPERTIES = {'Category': 's', 'Id': 's', 'Title': 's', 'Status': 's', 'WindowId': 'i', 'IconThemePath': 's', 'Menu': 'o', 'ItemIsMenu': 'b', 'IconName': 's', 'IconPixmap': 'a(iiay)', 'OverlayIconName': 's', 'OverlayIconPixmap': 'a(iiay)', 'AttentionIconName': 's', 'AttentionIconPixmap': 'a(iiay)', 'AttentionMovieName': 's', 'ToolTip': '(sa(iiay)ss)'}
SNI_XML = interface_xml(SNI, SNI_PROPERTIES,
    [('Activate', ('i', 'i')), ('SecondaryActivate', ('i', 'i')), ('ContextMenu', ('i', 'i')), ('Scroll', ('i', 's')), ('ProvideXdgActivationToken', ('s',))],
    {'NewTitle': (), 'NewIcon': (), 'NewToolTip': (), 'NewMenu': (), 'NewStatus': ('s',)})
MENU_XML = interface_xml(MENU, {'Version': 'u', 'TextDirection': 's', 'Status': 's', 'IconThemePath': 'as'},
    [('GetLayout', ('i', 'i', 'as'), ('u', '(ia{sv}av)')), ('GetGroupProperties', ('ai', 'as'), ('a(ia{sv})',)), ('GetProperty', ('i', 's'), ('v',)),
     ('Event', ('i', 's', 'v', 'u')), ('EventGroup', ('a(isvu)',), ('ai',)), ('AboutToShow', ('i',), ('b',)), ('AboutToShowGroup', ('ai',), ('ai', 'ai'))],
    {'LayoutUpdated': ('u', 'i'), 'ItemsPropertiesUpdated': ('a(ia{sv})', 'a(ias)')})


class Tray:
    @staticmethod
    def available():
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        return bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus', 'NameHasOwner', GLib.Variant('(s)', (WATCHER,)), None, Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]

    def __init__(self, icon, open_window, toggle, quit_ui):
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        self.icon = Path(icon)
        self.callbacks = {1: open_window, 2: toggle, 4: quit_ui}
        self.title = 'NiriBridge'
        self.labels = ('Open NiriBridge', 'Pause sharing', 'Quit the interface')
        self.enabled = True
        self.revision = 1
        self.activation_token = None
        pixbuf = GdkPixbuf.Pixbuf.new_from_file_at_scale(str(icon), 48, 48, True)
        pixels = pixbuf.get_pixels()
        channels, stride = pixbuf.get_n_channels(), pixbuf.get_rowstride()
        image = bytearray()
        for y in range(pixbuf.get_height()):
            for x in range(pixbuf.get_width()):
                offset = y * stride + x * channels
                r, g, b = pixels[offset:offset + 3]
                alpha = pixels[offset + 3] if channels == 4 else 255
                image.extend((alpha, r, g, b))
        self.pixmaps = [(pixbuf.get_width(), pixbuf.get_height(), bytes(image))]
        self.registrations = [self.bus.register_object(PATH, Gio.DBusNodeInfo.new_for_xml(SNI_XML).interfaces[0], self.call, self.property, None),
                              self.bus.register_object(MENU_PATH, Gio.DBusNodeInfo.new_for_xml(MENU_XML).interfaces[0], self.call, self.property, None)]
        self.owner_subscription = self.bus.signal_subscribe('org.freedesktop.DBus', 'org.freedesktop.DBus', 'NameOwnerChanged', '/org/freedesktop/DBus', WATCHER, Gio.DBusSignalFlags.NONE, self.owner_changed)
        self.register()

    def register(self):
        self.bus.call(WATCHER, '/StatusNotifierWatcher', WATCHER, 'RegisterStatusNotifierItem', GLib.Variant('(s)', (PATH,)), None, Gio.DBusCallFlags.NONE, 3000, None, self.registered)

    def registered(self, connection, result):
        try:
            connection.call_finish(result)
            for signal in ('NewIcon', 'NewTitle', 'NewToolTip', 'NewMenu'):
                self.bus.emit_signal(None, PATH, SNI, signal, None)
        except GLib.Error:
            pass

    def owner_changed(self, connection, sender, path, interface, signal, parameters):
        name, old, new = parameters.unpack()
        if new:
            self.register()

    def update(self, title, labels, enabled=True):
        if self.title != title:
            self.title = title
            self.bus.emit_signal(None, PATH, SNI, 'NewTitle', None)
            self.bus.emit_signal(None, PATH, SNI, 'NewToolTip', None)
        labels = tuple(labels)
        if self.labels != labels or self.enabled != enabled:
            self.labels, self.enabled = labels, enabled
            self.revision += 1
            self.bus.emit_signal(None, MENU_PATH, MENU, 'LayoutUpdated', GLib.Variant('(ui)', (self.revision, 0)))

    def menu_properties(self, item, names=()):
        if item == 0:
            props = {'children-display': GLib.Variant('s', 'submenu')}
        elif item == 3:
            props = {'type': GLib.Variant('s', 'separator')}
        elif item in (1, 2, 4):
            props = {'label': GLib.Variant('s', self.labels[{1: 0, 2: 1, 4: 2}[item]]), 'enabled': GLib.Variant('b', self.enabled if item == 2 else True), 'visible': GLib.Variant('b', True)}
        else:
            props = {}
        return {key: value for key, value in props.items() if not names or key in names}

    def layout(self, parent, depth, names):
        children = [GLib.Variant('(ia{sv}av)', (item, self.menu_properties(item, names), [])) for item in (1, 2, 3, 4)] if parent == 0 and depth != 0 else []
        return parent, self.menu_properties(parent, names), children

    def property(self, connection, sender, path, interface, name):
        if interface == MENU:
            value = {'Version': ('u', 3), 'TextDirection': ('s', 'ltr'), 'Status': ('s', 'normal'), 'IconThemePath': ('as', [str(self.icon.parent)])}.get(name)
            return GLib.Variant(*value) if value else None
        value = {'Category': 'ApplicationStatus', 'Id': 'niri-bridge', 'Title': self.title, 'Status': 'Active', 'WindowId': 0,
                 'IconThemePath': str(self.icon.parent), 'Menu': MENU_PATH, 'ItemIsMenu': False, 'IconName': 'niri-bridge',
                 'IconPixmap': self.pixmaps, 'OverlayIconName': '', 'OverlayIconPixmap': [], 'AttentionIconName': '', 'AttentionIconPixmap': [],
                 'AttentionMovieName': '', 'ToolTip': ('niri-bridge', self.pixmaps, 'NiriBridge', self.title)}.get(name)
        return GLib.Variant(SNI_PROPERTIES[name], value) if name in SNI_PROPERTIES else None

    def dispatch(self, item):
        if item in self.callbacks and (item != 2 or self.enabled):
            def run():
                self.callbacks[item]()
                return False
            GLib.idle_add(run)

    def call(self, connection, sender, path, interface, name, parameters, invocation):
        args = parameters.unpack()
        response = None
        if interface == SNI:
            if name in ('Activate', 'ContextMenu'):
                self.dispatch(1)
            elif name == 'SecondaryActivate':
                self.dispatch(2)
            elif name == 'ProvideXdgActivationToken':
                self.activation_token = args[0]
        elif name == 'GetLayout':
            response = GLib.Variant('(u(ia{sv}av))', (self.revision, self.layout(*args)))
        elif name == 'GetGroupProperties':
            ids, names = args
            response = GLib.Variant('(a(ia{sv}))', ([(item, self.menu_properties(item, names)) for item in ids if item in range(5)],))
        elif name == 'GetProperty':
            item, prop = args
            value = self.menu_properties(item).get(prop, GLib.Variant('s', ''))
            response = GLib.Variant('(v)', (value,))
        elif name == 'Event':
            if args[1] == 'clicked':
                self.dispatch(args[0])
        elif name == 'EventGroup':
            errors = []
            for item, event, data, timestamp in args[0]:
                if item not in self.callbacks:
                    errors.append(item)
                elif event == 'clicked':
                    self.dispatch(item)
            response = GLib.Variant('(ai)', (errors,))
        elif name == 'AboutToShow':
            response = GLib.Variant('(b)', (False,))
        elif name == 'AboutToShowGroup':
            response = GLib.Variant('(aiai)', ([], []))
        invocation.return_value(response)

    def close(self):
        self.bus.signal_unsubscribe(self.owner_subscription)
        for registration in self.registrations:
            self.bus.unregister_object(registration)
