# SPDX-License-Identifier: GPL-3.0-or-later
"""Screen connection geometry and a GTK/Cairo editor; physical monitor arrangement stays intact."""
from i18n import _, N_
import copy
import math
import uuid
import gi
gi.require_version('Gtk', '3.0')
gi.require_version('PangoCairo', '1.0')
from gi.repository import Gtk, Gdk, GObject, Pango, PangoCairo

OPPOSITE = {'top': 'bottom', 'bottom': 'top', 'left': 'right', 'right': 'left'}
EDGE_NAMES = {'top': N_("Above"), 'bottom': N_("Below"), 'left': N_("Left"), 'right': N_("Right")}
BOUNDARY_NAMES = {'top': N_("Top edge"), 'bottom': N_("Bottom edge"), 'left': N_("Left edge"), 'right': N_("Right edge")}
MAX_EDGES = 16
EDGE_COLORS = [(.32, .89, .77), (.97, .69, .32), (.62, .65, 1.), (.95, .53, .72)]


def desktop_edges(node):
    edges = copy.deepcopy(node.get('edges', [node['edge']] if node.get('edge') else []))
    for edge in edges:
        edge.setdefault('id', 'default')
    return edges


class LayoutDraft:
    def __init__(self, local, peer):
        self.local = copy.deepcopy(local)
        self.peer = copy.deepcopy(peer)
        self.supports_multiple = 'edges' in local and 'edges' in peer
        self.free_origin = None
        self.repaired = False
        for node in (self.local, self.peer):
            node['edges'] = desktop_edges(node)
        ids = list(dict.fromkeys(edge['id'] for node in (self.local, self.peer) for edge in node['edges']))
        for node, other in ((self.local, self.peer), (self.peer, self.local)):
            for identifier in ids:
                if not any(edge['id'] == identifier for edge in node['edges']):
                    counterpart = next(edge for edge in other['edges'] if edge['id'] == identifier)
                    node['edges'].append({'id': identifier, 'output': next(iter(node['outputs'])),
                        'boundary': {'edge': OPPOSITE[counterpart['boundary']['edge']], 'start': 0., 'end': 1.}})
                    self.repaired = True
        self.selected_id = next((identifier for identifier in ids if all(
            any(edge['id'] == identifier and edge['output'] in node['outputs'] for edge in node['edges'])
            for node in (self.local, self.peer))), ids[0])
        self.select(self.selected_id)

    def connection_ids(self):
        return [edge['id'] for edge in self.local['edges']]

    def select(self, identifier):
        self.selected_id = identifier
        for node in (self.local, self.peer):
            node['edge'] = next(edge for edge in node['edges'] if edge['id'] == identifier)
        self.free_origin = None
        self.reset_scale()

    def reset_scale(self):
        a, b = self.selected('local'), self.selected('peer')
        x = self.local['edge']['boundary']
        y = self.peer['edge']['boundary']
        axis = 'width' if x['edge'] in ('top', 'bottom') else 'height'
        other_axis = 'width' if y['edge'] in ('top', 'bottom') else 'height'
        self.peer_scale = a[axis] * max(0.000001, x['end'] - x['start']) / (b[other_axis] * max(0.000001, y['end'] - y['start']))
        self.peer_scale = max(.15, min(6.0, self.peer_scale))

    def selected(self, role):
        node = self.local if role == 'local' else self.peer
        return node['outputs'].get(node['edge']['output'], next(iter(node['outputs'].values())))

    def origin(self):
        if self.free_origin is not None:
            return self.free_origin
        a, b = self.selected('local'), self.selected('peer')
        x, y = self.local['edge']['boundary'], self.peer['edge']['boundary']
        bw, bh = b['width'] * self.peer_scale, b['height'] * self.peer_scale
        gap = min(a['width'], a['height']) * .10
        if x['edge'] in ('top', 'bottom'):
            peer_x = bw * y['start'] if y['edge'] in ('top', 'bottom') else (0 if y['edge'] == 'left' else bw)
            tangent = a['width'] * x['start'] - peer_x
            return (tangent, -bh - gap if x['edge'] == 'top' else a['height'] + gap)
        peer_y = bh * y['start'] if y['edge'] in ('left', 'right') else (0 if y['edge'] == 'top' else bh)
        tangent = a['height'] * x['start'] - peer_y
        return (-bw - gap if x['edge'] == 'left' else a['width'] + gap, tangent)

    def rectangles(self):
        rectangles = []
        for role, node, factor, origin in [('local', self.local, 1., (0., 0.)), ('peer', self.peer, self.peer_scale, self.origin())]:
            selected = self.selected(role)
            for name, o in node['outputs'].items():
                rectangles.append({'role': role, 'name': name,
                    'selected': name == node['edge']['output'],
                    'x': origin[0] + (o['x'] - selected['x']) * factor,
                    'y': origin[1] + (o['y'] - selected['y']) * factor,
                    'width': o['width'] * factor, 'height': o['height'] * factor})
        return rectangles

    def finish_drag(self, x, y):
        a, b = self.selected('local'), self.selected('peer')
        bw, bh = b['width'] * self.peer_scale, b['height'] * self.peer_scale
        cx, cy = (x + bw / 2 - a['width'] / 2) / a['width'], (y + bh / 2 - a['height'] / 2) / a['height']
        edge = ('right' if cx >= 0 else 'left') if abs(cx) > abs(cy) else ('bottom' if cy >= 0 else 'top')
        horizontal = edge in ('top', 'bottom')
        extent, other_extent, tangent = (a['width'], bw, x) if horizontal else (a['height'], bh, y)
        minimum = min(extent, other_extent) * .08
        tangent = max(-other_extent + minimum, min(extent - minimum, tangent))
        start, end = max(0., tangent), min(extent, tangent + other_extent)
        self.local['edge']['boundary'] = {'edge': edge, 'start': round(start / extent, 6), 'end': round(end / extent, 6)}
        self.peer['edge']['boundary'] = {'edge': OPPOSITE[edge], 'start': round((start - tangent) / other_extent, 6), 'end': round((end - tangent) / other_extent, 6)}
        self.free_origin = None

    def request(self):
        return {'local_edges': self.local['edges'], 'peer_edges': self.peer['edges'],
                'local_revision': self.local['revision'], 'peer_revision': self.peer['revision']}

    @staticmethod
    def overlaps(node, candidate, ignore_id=None):
        boundary = candidate['boundary']
        return any(edge['id'] != ignore_id and edge['output'] == candidate['output']
                   and edge['boundary']['edge'] == boundary['edge']
                   and boundary['start'] < edge['boundary']['end']
                   and edge['boundary']['start'] < boundary['end'] for edge in node['edges'])

    def validation_error(self):
        if not 1 <= len(self.local['edges']) <= MAX_EDGES:
            return 'layout_count_invalid'
        for node in (self.local, self.peer):
            for edge in node['edges']:
                output = node['outputs'].get(edge['output'])
                if output is None:
                    return 'output_unavailable'
                b = edge['boundary']
                if not all(math.isfinite(b[key]) for key in ('start', 'end')) or not 0 <= b['start'] < b['end'] <= 1:
                    return 'layout_invalid'
                extent = output['width' if b['edge'] in ('top', 'bottom') else 'height']
                if (b['end'] - b['start']) * extent < 1:
                    return 'layout_invalid'
                if self.overlaps(node, edge, edge['id']):
                    return 'layout_overlap'
        return None

    @staticmethod
    def free_ranges(node, output, side):
        spans = sorted((e['boundary']['start'], e['boundary']['end']) for e in node['edges']
                       if e['output'] == output and e['boundary']['edge'] == side)
        result, end = [], 0.
        for start, next_end in spans:
            if start > end:
                result.append((end, start))
            end = max(end, next_end)
        if end < 1:
            result.append((end, 1.))
        return result

    def add_connection(self):
        if len(self.local['edges']) >= MAX_EDGES:
            raise ValueError('layout_count_invalid')
        identifier = uuid.uuid4().hex
        candidates = []
        rects = self.rectangles()
        for a in (r for r in rects if r['role'] == 'local'):
            for b in (r for r in rects if r['role'] == 'peer'):
                for side in OPPOSITE:
                    horizontal = side in ('top', 'bottom')
                    axis, extent = ('x', 'width') if horizontal else ('y', 'height')
                    gaps = {'top': a['y'] - b['y'] - b['height'], 'bottom': b['y'] - a['y'] - a['height'],
                            'left': a['x'] - b['x'] - b['width'], 'right': b['x'] - a['x'] - a['width']}
                    start, end = max(a[axis], b[axis]), min(a[axis] + a[extent], b[axis] + b[extent])
                    if gaps[side] < 0 or end <= start:
                        continue
                    pair = [{'id': identifier, 'output': r['name'], 'boundary': {'edge': e,
                             'start': round((start - r[axis]) / r[extent], 6), 'end': round((end - r[axis]) / r[extent], 6)}}
                            for r, e in ((a, side), (b, OPPOSITE[side]))]
                    if not any(self.overlaps(node, edge) for node, edge in zip((self.local, self.peer), pair)):
                        candidates.append((gaps[side], pair))
        if candidates:
            pair = min(candidates, key=lambda candidate: candidate[0])[1]
        else:
            choices = []
            for side in OPPOSITE:
                for a in self.local['outputs']:
                    for b in self.peer['outputs']:
                        left = self.free_ranges(self.local, a, side)
                        right = self.free_ranges(self.peer, b, OPPOSITE[side])
                        if left and right:
                            spans = [max(ranges, key=lambda span: span[1] - span[0]) for ranges in (left, right)]
                            choices.append([{'id': identifier, 'output': output, 'boundary': {'edge': direction, 'start': span[0], 'end': span[1]}}
                                            for output, direction, span in zip((a, b), (side, OPPOSITE[side]), spans)])
            if not choices:
                raise ValueError('layout_no_space')
            pair = choices[0]
        for node, edge in zip((self.local, self.peer), pair):
            node['edges'].append(edge)
        self.select(identifier)
        return identifier

    def remove_connection(self):
        if len(self.local['edges']) <= 1:
            raise ValueError('layout_count_invalid')
        index = self.connection_ids().index(self.selected_id)
        for node in (self.local, self.peer):
            node['edges'] = [edge for edge in node['edges'] if edge['id'] != self.selected_id]
        self.select(self.connection_ids()[min(index, len(self.local['edges']) - 1)])


def rounded(cr, x, y, w, h, r):
    r = min(r, w / 2, h / 2)
    cr.new_sub_path()
    for cx, cy, start in [(x + w - r, y + r, -math.pi / 2), (x + w - r, y + h - r, 0), (x + r, y + h - r, math.pi / 2), (x + r, y + r, math.pi)]:
        cr.arc(cx, cy, r, start, start + math.pi / 2)
    cr.close_path()


class ScreenCanvas(Gtk.DrawingArea):
    __gsignals__ = {'layout-changed': (GObject.SignalFlags.RUN_FIRST, None, ())}

    def __init__(self, editable=False):
        super().__init__()
        self.editable = editable
        self.draft = None
        self.last_selection = None
        self.peer_name = _("Other computer")
        self.control_role = 'local'
        self.drag = None
        self.transform = (1, 0, 0)
        self.hit_rects = []
        self.set_size_request(360, 300)
        self.set_hexpand(True)
        self.set_vexpand(True)
        self.add_events(Gdk.EventMask.BUTTON_PRESS_MASK | Gdk.EventMask.BUTTON_RELEASE_MASK | Gdk.EventMask.POINTER_MOTION_MASK)
        self.connect('draw', self.draw)
        self.connect('button-press-event', self.press)
        self.connect('motion-notify-event', self.motion)
        self.connect('button-release-event', self.release)
        self.get_accessible().set_name(_("Screen connection layout"))
        self.set_tooltip_text(_("Drag the other computer's screens to adjust the selected connection.") if editable else _("The keyboard and gestures follow the pointer across the highlighted edges."))

    def set_desktops(self, local, peer, peer_name=N_("Other computer")):
        self.peer_name = _(peer_name)
        if self.draft:
            self.last_selection = self.draft.selected_id
        if local and peer and local.get('outputs') and peer.get('outputs'):
            self.draft = LayoutDraft(local, peer)
            if self.editable and self.last_selection in self.draft.connection_ids():
                self.draft.select(self.last_selection)
        else:
            self.draft = None
        self.queue_draw()

    def text(self, cr, text, x, y, width, size=11, color=(.72, .78, .86), weight=False):
        layout = PangoCairo.create_layout(cr)
        layout.set_text(str(text), -1)
        font = Pango.FontDescription(f'Sans {size}')
        if weight:
            font.set_weight(Pango.Weight.SEMIBOLD)
        layout.set_font_description(font)
        layout.set_width(int(width * Pango.SCALE))
        layout.set_ellipsize(Pango.EllipsizeMode.END)
        layout.set_alignment(Pango.Alignment.CENTER)
        cr.set_source_rgb(*color)
        cr.move_to(x, y)
        PangoCairo.show_layout(cr, layout)

    def draw(self, widget, cr):
        width, height = self.get_allocated_width(), self.get_allocated_height()
        cr.set_source_rgb(.063, .086, .126)
        rounded(cr, 0, 0, width, height, 12)
        cr.fill()
        cr.set_source_rgba(.30, .41, .56, .17)
        for x in range(18, width, 22):
            for y in range(18, height, 22):
                cr.arc(x, y, .8, 0, math.pi * 2)
                cr.fill()
        if self.draft is None:
            self.text(cr, _("Your screens will appear when connected"), 25, height / 2 - 22, width - 50, 14, weight=True)
            self.text(cr, _("Turn on sharing and wait for the paired computer"), 25, height / 2 + 12, width - 50)
            return False
        rects = self.draft.rectangles()
        if not self.drag:
            x0, y0 = min(r['x'] for r in rects), min(r['y'] for r in rects)
            x1, y1 = max(r['x'] + r['width'] for r in rects), max(r['y'] + r['height'] for r in rects)
            scale = min((width - 88) / max(1, x1 - x0), (height - 68) / max(1, y1 - y0))
            self.transform = (scale, (width - (x1 - x0) * scale) / 2 - x0 * scale, (height - (y1 - y0) * scale) / 2 - y0 * scale)
        scale, ox, oy = self.transform
        self.hit_rects = []
        centers = {}
        identifiers = self.draft.connection_ids()
        for r in rects:
            x, y, w, h = r['x'] * scale + ox, r['y'] * scale + oy, r['width'] * scale, r['height'] * scale
            self.hit_rects.append((x, y, w, h, r['role']))
            active = (r['role'] == 'peer') == (self.control_role == 'sending')
            color = (.12, .23, .31) if r['role'] == 'peer' else (.12, .19, .31)
            cr.set_source_rgb(*color)
            rounded(cr, x, y, w, h, 7)
            cr.fill_preserve()
            cr.set_source_rgba(*((.32, .79, .70) if r['role'] == 'peer' else (.36, .57, .92)), .95 if active and r['selected'] else .46)
            cr.set_line_width(2 if r['selected'] else 1)
            cr.stroke()
            title = _("This computer") if r['role'] == 'local' else self.peer_name
            if r['selected']:
                self.text(cr, title, x + 5, y + h / 2 - 18, w - 10, 11, (.9, .94, 1.), True)
                self.text(cr, r['name'], x + 5, y + h / 2 + 3, w - 10, 9)
            elif w > 45 and h > 40:
                self.text(cr, r['name'], x + 4, y + h / 2 - 8, w - 8, 9)
            node = self.draft.local if r['role'] == 'local' else self.draft.peer
            for edge in node['edges']:
                if edge['output'] != r['name']:
                    continue
                b = edge['boundary']
                if b['edge'] in ('top', 'bottom'):
                    yy = y if b['edge'] == 'top' else y + h
                    begin, end = (x + w * b['start'], yy), (x + w * b['end'], yy)
                else:
                    xx = x if b['edge'] == 'left' else x + w
                    begin, end = (xx, y + h * b['start']), (xx, y + h * b['end'])
                index = identifiers.index(edge['id'])
                color = EDGE_COLORS[index % len(EDGE_COLORS)]
                cr.set_source_rgb(*color)
                cr.set_line_width(4 if edge['id'] == self.draft.selected_id else 2)
                cr.set_line_cap(1)
                cr.move_to(*begin)
                cr.line_to(*end)
                cr.stroke()
                center = ((begin[0] + end[0]) / 2, (begin[1] + end[1]) / 2)
                centers.setdefault(edge['id'], {})[r['role']] = center
                nx = center[0] + (12 if b['edge'] == 'left' else -12 if b['edge'] == 'right' else 0)
                ny = center[1] + (12 if b['edge'] == 'top' else -12 if b['edge'] == 'bottom' else 0)
                cr.arc(nx, ny, 10, 0, math.pi * 2)
                cr.set_source_rgb(.063, .086, .126)
                cr.fill_preserve()
                cr.set_source_rgb(*color)
                cr.set_line_width(1)
                cr.stroke()
                self.text(cr, str(index + 1), nx - 9, ny - 7, 18, 8, color, True)
        for identifier, points in centers.items():
            if len(points) != 2:
                continue
            a, b = points['local'], points['peer']
            color = EDGE_COLORS[identifiers.index(identifier) % len(EDGE_COLORS)]
            cr.set_source_rgba(*color, .65 if identifier == self.draft.selected_id else .4)
            cr.set_line_width(2)
            cr.set_dash([3, 5])
            cr.move_to(*a)
            cr.line_to(*b)
            cr.stroke()
            cr.set_dash([])
        return False

    def press(self, widget, event):
        if not self.editable or not self.draft or event.button != 1:
            return False
        if any(role == 'peer' and x <= event.x <= x + w and y <= event.y <= y + h for x, y, w, h, role in self.hit_rects):
            self.drag = (event.x, event.y, self.draft.origin())
            if self.get_window():
                self.get_window().set_cursor(Gdk.Cursor.new_from_name(self.get_display(), 'grabbing'))
            return True
        return False

    def motion(self, widget, event):
        if not self.drag:
            return False
        x, y, origin = self.drag
        scale = self.transform[0]
        self.draft.free_origin = (origin[0] + (event.x - x) / scale, origin[1] + (event.y - y) / scale)
        self.queue_draw()
        return True

    def release(self, widget, event):
        if not self.drag:
            return False
        origin = self.draft.origin()
        self.drag = None
        self.draft.finish_drag(*origin)
        if self.get_window():
            self.get_window().set_cursor(None)
        self.queue_draw()
        self.emit('layout-changed')
        return True
