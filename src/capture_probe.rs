// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit, bounded capture experiment. No network, virtual input, or key-content logging.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Write,
    os::fd::{AsFd, AsRawFd, FromRawFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
};
use wayland_protocols::wp::{
    keyboard_shortcuts_inhibit::zv1::client::{
        zwp_keyboard_shortcuts_inhibit_manager_v1 as inhibit_manager,
        zwp_keyboard_shortcuts_inhibitor_v1 as inhibitor,
    },
    pointer_constraints::zv1::client::{
        zwp_locked_pointer_v1 as locked_pointer, zwp_pointer_constraints_v1 as constraints,
    },
    relative_pointer::zv1::client::{
        zwp_relative_pointer_manager_v1 as relative_manager,
        zwp_relative_pointer_v1 as relative_pointer,
    },
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1 as layer_shell, zwlr_layer_surface_v1 as layer_surface,
};

use crate::{
    doctor,
    geometry::{Boundary, Edge},
    niri,
    protocol::{Axis, InputEvent, ScrollSource},
};

pub enum Event {
    Armed,
    Started { edge: usize, fraction: f64 },
    Ready,
    Input(InputEvent),
    Finished { reason: String },
}

pub struct StreamEvent {
    pub generation: u64,
    pub captured_at: Instant,
    pub event: Event,
}

pub struct StreamContext {
    pub generation: u64,
    pub sender: tokio::sync::mpsc::Sender<StreamEvent>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Args)]
pub struct Options {
    /// The exact connector name shown by doctor (for example eDP-1).
    #[arg(long)]
    pub output: String,
    #[arg(long, value_enum)]
    pub edge: Edge,
    #[arg(long, default_value_t = 0.0)]
    pub start: f64,
    #[arg(long, default_value_t = 1.0)]
    pub end: f64,
    /// Total experiment duration, including waiting for the pointer to enter the strip.
    #[arg(long, default_value_t=15, value_parser=clap::value_parser!(u64).range(1..=30))]
    pub seconds: u64,
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct Counts {
    pub pointer_lock_observed: bool,
    pub keyboard_focus_observed: bool,
    pub shortcuts_inhibited_observed: bool,
    pub relative_motion_events: u64,
    pub keyboard_events: u64,
    pub button_events: u64,
    pub scroll_events: u64,
    pub stop_reason: String,
}

impl Options {
    fn validate(&self) -> Result<()> {
        Boundary {
            edge: self.edge,
            start: self.start,
            end: self.end,
        }
        .validate()?;
        ensure!(!self.output.is_empty(), "Choose an output from doctor");
        ensure!(
            (1..=30).contains(&self.seconds),
            "Capture duration must be between 1 and 30 seconds"
        );
        Ok(())
    }
}

/// Run under a separate watchdog process so a blocked Wayland roundtrip still releases capture.
pub fn run(options: Options) -> Result<Counts> {
    options.validate()?;
    let mut child = std::process::Command::new(std::env::current_exe()?);
    child.args([
        "probe-capture",
        "--output",
        &options.output,
        "--edge",
        options.edge.as_str(),
        "--start",
        &options.start.to_string(),
        "--end",
        &options.end.to_string(),
        "--seconds",
        &options.seconds.to_string(),
    ]);
    eprintln!(
        "A narrow blue strip will appear on the selected edge. Move into it to test capture."
    );
    eprintln!(
        "Press Escape to return immediately; the experiment also ends automatically after {} seconds. Only event counts are retained.",
        options.seconds
    );
    let output = doctor::bounded_output(&mut child, Duration::from_secs(options.seconds + 5))?;
    Ok(serde_json::from_str(&output)?)
}

struct Strip {
    surface: wl_surface::WlSurface,
    layer: layer_surface::ZwlrLayerSurfaceV1,
    output_id: u32,
    horizontal: bool,
    extent: u32,
    armed: bool,
    pointer_inside: bool,
}

struct State {
    counts: Counts,
    finished: bool,
    requested: bool,
    outputs: BTreeMap<String, wl_output::WlOutput>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    relative: Option<relative_pointer::ZwpRelativePointerV1>,
    strips: Vec<Strip>,
    selected: Option<usize>,
    seat: wl_seat::WlSeat,
    constraints: constraints::ZwpPointerConstraintsV1,
    inhibit_manager: inhibit_manager::ZwpKeyboardShortcutsInhibitManagerV1,
    relative_manager: relative_manager::ZwpRelativePointerManagerV1,
    locked: Option<locked_pointer::ZwpLockedPointerV1>,
    inhibitor: Option<inhibitor::ZwpKeyboardShortcutsInhibitorV1>,
    shm: wl_shm::WlShm,
    buffers: Vec<wl_buffer::WlBuffer>,
    stream: Option<StreamContext>,
    ready_reported: bool,
    start_fraction: f64,
    pending_inputs: Vec<InputEvent>,
    pressed_keys: BTreeSet<u16>,
    scroll_source: ScrollSource,
}

impl State {
    fn selected_surface(&self) -> Option<&wl_surface::WlSurface> {
        self.selected
            .and_then(|index| self.strips.get(index))
            .map(|strip| &strip.surface)
    }
    fn publish(&mut self, event: Event) {
        if self.finished {
            return;
        }
        if let Some(stream) = &self.stream
            && stream
                .sender
                .try_send(StreamEvent {
                    generation: stream.generation,
                    captured_at: Instant::now(),
                    event,
                })
                .is_err()
        {
            self.finish("event_queue_unavailable");
        }
    }

    fn input(&mut self, event: InputEvent) {
        if event.validate().is_err() {
            self.finish("unsupported_input");
            return;
        }
        if self.ready_reported {
            self.publish(Event::Input(event));
        } else if self.pending_inputs.len() < 128 {
            self.pending_inputs.push(event);
        } else {
            self.finish("event_queue_unavailable");
        }
    }

    fn report_ready(&mut self) {
        if !self.ready_reported
            && self.counts.pointer_lock_observed
            && self.counts.keyboard_focus_observed
            && self.counts.shortcuts_inhibited_observed
        {
            self.ready_reported = true;
            self.publish(Event::Started {
                edge: self.selected.expect("A ready capture has a selected edge"),
                fraction: self.start_fraction,
            });
            for event in std::mem::take(&mut self.pending_inputs) {
                self.publish(Event::Input(event));
            }
            self.publish(Event::Ready);
        }
    }
    fn finish(&mut self, reason: &str) {
        if !self.finished {
            self.counts.stop_reason = reason.to_owned();
        }
        self.finished = true;
    }

    fn request_capture(&mut self, qh: &QueueHandle<Self>, edge: usize, fraction: f64) {
        if self.requested || self.finished {
            return;
        }
        if self
            .stream
            .as_ref()
            .is_some_and(|s| s.cancel.load(Ordering::Acquire))
        {
            self.finish("cancelled");
            return;
        }
        self.start_fraction = fraction.clamp(0.0, 1.0);
        if self.finished {
            return;
        }
        let (Some(strip), Some(pointer)) = (self.strips.get(edge), &self.pointer) else {
            return;
        };
        self.selected = Some(edge);
        self.requested = true;
        self.inhibitor =
            Some(
                self.inhibit_manager
                    .inhibit_shortcuts(&strip.surface, &self.seat, qh, ()),
            );
        self.locked = Some(self.constraints.lock_pointer(
            &strip.surface,
            pointer,
            None,
            constraints::Lifetime::Oneshot,
            qh,
            (),
        ));
        strip
            .layer
            .set_keyboard_interactivity(layer_surface::KeyboardInteractivity::Exclusive);
        strip.surface.commit();
    }

    fn paint(
        &mut self,
        index: usize,
        width: u32,
        height: u32,
        qh: &QueueHandle<Self>,
    ) -> Result<()> {
        ensure!(
            width > 0 && height > 0 && width <= 32768 && height <= 32768,
            "Invalid capture strip size"
        );
        let len = u64::from(width) * u64::from(height) * 4;
        ensure!(len <= 4 * 1024 * 1024, "Capture strip exceeds buffer limit");
        // Anonymous memory-backed file: no keyboard data or persistent files are written.
        let fd =
            unsafe { libc::memfd_create(c"niri-bridge-capture-strip".as_ptr(), libc::MFD_CLOEXEC) };
        ensure!(fd >= 0, "Could not allocate the capture strip");
        let mut file = unsafe { File::from_raw_fd(fd) };
        let pixel = if self.stream.is_some() {
            0u32
        } else {
            0xff2486d6u32
        }
        .to_ne_bytes();
        let data = pixel.repeat((len / 4) as usize);
        file.write_all(&data)?;
        let pool = self.shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            (width * 4) as i32,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );
        let surface = &self
            .strips
            .get(index)
            .context("Capture surface missing")?
            .surface;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        surface.commit();
        self.buffers.push(buffer);
        pool.destroy();
        Ok(())
    }

    fn cleanup(&mut self) {
        for strip in &self.strips {
            strip
                .layer
                .set_keyboard_interactivity(layer_surface::KeyboardInteractivity::None);
        }
        if let Some(lock) = self.locked.take() {
            lock.destroy();
        }
        if let Some(inhibitor) = self.inhibitor.take() {
            inhibitor.destroy();
        }
        for strip in self.strips.drain(..) {
            strip.layer.destroy();
            strip.surface.destroy();
        }
        if let Some(relative) = self.relative.take() {
            relative.destroy();
        }
        for buffer in self.buffers.drain(..) {
            buffer.destroy();
        }
    }
}

/// Internal worker. Call through run() to retain the independent timeout watchdog.
pub fn worker(options: Options) -> Result<Counts> {
    worker_internal(vec![options], None)
}

/// Continuous capture worker for the bridge. Ordinary Escape is forwarded; the emergency
/// release combination is Ctrl+Alt+Shift+Escape. The owner must service the event queue and cancel.
pub fn worker_stream(options: Vec<Options>, context: StreamContext) -> Result<Counts> {
    worker_internal(options, Some(context))
}

fn worker_internal(options: Vec<Options>, stream: Option<StreamContext>) -> Result<Counts> {
    ensure!(
        !options.is_empty() && options.len() <= crate::bridge::MAX_EDGES,
        "Invalid capture edge count"
    );
    for option in &options {
        option.validate()?;
    }
    let limited = stream.is_none();
    ensure!(
        !limited || options.len() == 1,
        "Capture probes use one edge"
    );
    let seconds = options[0].seconds;
    let outputs = niri::outputs()?;
    let connection = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
    let qh = queue.handle();
    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 4..=6, ())?;
    let layers: layer_shell::ZwlrLayerShellV1 = globals.bind(&qh, 1..=5, ())?;
    let mut state = State {
        counts: Counts::default(),
        finished: false,
        requested: false,
        outputs: BTreeMap::new(),
        pointer: None,
        keyboard: None,
        relative: None,
        strips: Vec::new(),
        selected: None,
        locked: None,
        inhibitor: None,
        buffers: Vec::new(),
        stream,
        ready_reported: false,
        start_fraction: 0.0,
        pending_inputs: Vec::new(),
        pressed_keys: BTreeSet::new(),
        scroll_source: ScrollSource::Wheel,
        seat: globals.bind(&qh, 1..=9, ())?,
        constraints: globals.bind(&qh, 1..=1, ())?,
        inhibit_manager: globals.bind(&qh, 1..=1, ())?,
        relative_manager: globals.bind(&qh, 1..=1, ())?,
        shm: globals.bind(&qh, 1..=2, ())?,
    };
    for global in globals
        .contents()
        .clone_list()
        .iter()
        .filter(|g| g.interface == "wl_output" && g.version >= 4)
    {
        globals
            .registry()
            .bind::<wl_output::WlOutput, _, _>(global.name, 4, &qh, ());
    }
    queue.roundtrip(&mut state)?;
    ensure!(
        state.pointer.is_some() && state.keyboard.is_some(),
        "A keyboard and pointer seat is required"
    );
    for options in options {
        let logical = outputs
            .get(&options.output)
            .context("Output not found in the Niri session")?
            .logical
            .context("The selected output is disabled")?;
        let target = state
            .outputs
            .get(&options.output)
            .context("Output name not found in the Wayland registry")?;
        let surface = compositor.create_surface(&qh, ());
        let layer = layers.get_layer_surface(
            &surface,
            Some(target),
            layer_shell::Layer::Overlay,
            if limited {
                "niri-bridge-capture-test"
            } else {
                "niri-bridge-edge"
            }
            .into(),
            &qh,
            (),
        );
        let horizontal = matches!(options.edge, Edge::Top | Edge::Bottom);
        let length = if horizontal {
            logical.width
        } else {
            logical.height
        };
        let offset = (options.start * f64::from(length)).floor() as i32;
        let extent = ((options.end - options.start) * f64::from(length)).floor() as u32;
        ensure!(
            extent >= 1,
            "Capture span is narrower than one logical pixel"
        );
        use layer_surface::Anchor;
        let anchor = match options.edge {
            Edge::Top => Anchor::Top | Anchor::Left,
            Edge::Bottom => Anchor::Bottom | Anchor::Left,
            Edge::Left => Anchor::Top | Anchor::Left,
            Edge::Right => Anchor::Top | Anchor::Right,
        };
        layer.set_anchor(anchor);
        layer.set_margin(
            if horizontal { 0 } else { offset },
            0,
            0,
            if horizontal { offset } else { 0 },
        );
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(layer_surface::KeyboardInteractivity::None);
        layer.set_size(
            if horizontal { extent } else { 2 },
            if horizontal { 2 } else { extent },
        );
        surface.commit();
        state.strips.push(Strip {
            surface,
            layer,
            output_id: target.id().protocol_id(),
            horizontal,
            extent,
            armed: limited,
            pointer_inside: false,
        });
    }
    if !limited {
        // Require a pointer already at the edge to leave and re-enter before capturing.
        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;
        for strip in &mut state.strips {
            strip.armed = !strip.pointer_inside;
        }
        state.publish(Event::Armed);
    }
    let started = Instant::now();
    let outcome = (|| -> Result<()> {
        while !state.finished && (!limited || started.elapsed() < Duration::from_secs(seconds)) {
            if state
                .stream
                .as_ref()
                .is_some_and(|s| s.cancel.load(Ordering::Acquire))
            {
                state.finish("cancelled");
                break;
            }
            queue.dispatch_pending(&mut state)?;
            connection.flush()?;
            if state.finished {
                break;
            }
            if let Some(guard) = queue.prepare_read() {
                let mut fd = libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut fd, 1, 50) };
                if ready > 0 {
                    guard.read()?;
                } else if ready < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
        }
        Ok(())
    })();
    if !state.finished {
        state.finish(if outcome.is_ok() {
            "timeout"
        } else {
            "connection_error"
        });
    }
    state.cleanup();
    let _ = connection.flush();
    let _ = queue.roundtrip(&mut state);
    if let Some(stream) = &state.stream {
        let _ = stream.sender.try_send(StreamEvent {
            generation: stream.generation,
            captured_at: Instant::now(),
            event: Event::Finished {
                reason: state.counts.stop_reason.clone(),
            },
        });
    }
    outcome?;
    Ok(state.counts)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if state
            .strips
            .iter()
            .any(|strip| strip.output_id == proxy.id().protocol_id())
            && matches!(
                &event,
                wl_output::Event::Geometry { .. }
                    | wl_output::Event::Scale { .. }
                    | wl_output::Event::Mode { .. }
            )
        {
            state.finish("output_configuration_changed");
        }
        if let wl_output::Event::Name { name } = event {
            state.outputs.insert(name, proxy.clone());
        }
    }
}
impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                let pointer = seat.get_pointer(qh, ());
                state.relative = Some(state.relative_manager.get_relative_pointer(
                    &pointer,
                    qh,
                    (),
                ));
                state.pointer = Some(pointer);
            }
            if caps.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if state.requested
                && (!caps.contains(wl_seat::Capability::Pointer)
                    || !caps.contains(wl_seat::Capability::Keyboard))
            {
                state.finish("seat_removed");
            }
        }
    }
}
impl Dispatch<layer_surface::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        layer: &layer_surface::ZwlrLayerSurfaceV1,
        event: layer_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            layer_surface::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                let index = state
                    .strips
                    .iter()
                    .position(|strip| strip.layer.id() == layer.id());
                if index.is_none_or(|index| state.paint(index, width, height, qh).is_err()) {
                    state.finish("surface_error");
                }
            }
            layer_surface::Event::Closed => state.finish("surface_closed"),
            _ => {}
        }
    }
}
impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                let Some(index) = state
                    .strips
                    .iter()
                    .position(|strip| strip.surface.id() == surface.id())
                else {
                    return;
                };
                let strip = &mut state.strips[index];
                strip.pointer_inside = true;
                if strip.armed {
                    pointer.set_cursor(serial, None, 0, 0);
                    let along = if strip.horizontal {
                        surface_x
                    } else {
                        surface_y
                    };
                    let fraction = along / f64::from(strip.extent);
                    state.request_capture(qh, index, fraction);
                }
            }
            wl_pointer::Event::Leave { surface, .. } => {
                if let Some(strip) = state
                    .strips
                    .iter_mut()
                    .find(|strip| strip.surface.id() == surface.id())
                {
                    strip.pointer_inside = false;
                    strip.armed = true;
                }
            }
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(pressed),
                ..
            } if state.requested => {
                state.counts.button_events += 1;
                if let Ok(code) = u16::try_from(button) {
                    state.input(InputEvent::Button {
                        code,
                        pressed: pressed == wl_pointer::ButtonState::Pressed,
                    });
                }
            }
            wl_pointer::Event::AxisSource {
                axis_source: WEnum::Value(source),
            } => {
                state.scroll_source = match source {
                    wl_pointer::AxisSource::Finger => ScrollSource::Finger,
                    wl_pointer::AxisSource::Continuous => ScrollSource::Continuous,
                    _ => ScrollSource::Wheel,
                };
            }
            wl_pointer::Event::Axis {
                axis: WEnum::Value(axis),
                value,
                ..
            } if state.requested => {
                state.counts.scroll_events += 1;
                let axis = if axis == wl_pointer::Axis::HorizontalScroll {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                state.input(InputEvent::Scroll {
                    axis,
                    amount: value,
                    source: state.scroll_source,
                });
            }
            wl_pointer::Event::AxisStop {
                axis: WEnum::Value(axis),
                ..
            } if state.requested => {
                let axis = if axis == wl_pointer::Axis::HorizontalScroll {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                state.input(InputEvent::ScrollStop {
                    axis,
                    source: state.scroll_source,
                });
            }
            _ => {}
        }
    }
}
impl Dispatch<relative_pointer::ZwpRelativePointerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &relative_pointer::ZwpRelativePointerV1,
        event: relative_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let relative_pointer::Event::RelativeMotion { dx, dy, .. } = event
            && state.requested
        {
            state.counts.relative_motion_events += 1;
            state.input(InputEvent::Motion { dx, dy });
        }
    }
}
impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, keys, .. }
                if state
                    .selected_surface()
                    .is_some_and(|s| s.id() == surface.id()) =>
            {
                state.counts.keyboard_focus_observed = true;
                let (chunks, remainder) = keys.as_chunks::<4>();
                if !remainder.is_empty() {
                    state.finish("invalid_key_state");
                    return;
                }
                for bytes in chunks {
                    if let Ok(code) = u16::try_from(u32::from_ne_bytes(*bytes))
                        && state.pressed_keys.insert(code)
                    {
                        state.input(InputEvent::Key {
                            code,
                            pressed: true,
                        });
                    }
                }
                state.report_ready();
            }
            wl_keyboard::Event::Leave { .. } if state.counts.keyboard_focus_observed => {
                state.finish("keyboard_focus_lost")
            }
            wl_keyboard::Event::Key {
                key,
                state: key_state,
                ..
            } if state.requested => {
                state.counts.keyboard_events += 1;
                let Ok(code) = u16::try_from(key) else {
                    state.finish("unsupported_input");
                    return;
                };
                let pressed = key_state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                let changed = if pressed {
                    state.pressed_keys.insert(code)
                } else {
                    state.pressed_keys.remove(&code)
                };
                let emergency = crate::input::emergency_modifiers(&state.pressed_keys);
                if code == 1 && pressed && (state.stream.is_none() || emergency) {
                    state.finish("escape");
                } else if changed {
                    state.input(InputEvent::Key { code, pressed });
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<locked_pointer::ZwpLockedPointerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &locked_pointer::ZwpLockedPointerV1,
        event: locked_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            locked_pointer::Event::Locked => {
                state.counts.pointer_lock_observed = true;
                state.report_ready();
            }
            locked_pointer::Event::Unlocked => state.finish("pointer_unlocked"),
            _ => {}
        }
    }
}
impl Dispatch<inhibitor::ZwpKeyboardShortcutsInhibitorV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &inhibitor::ZwpKeyboardShortcutsInhibitorV1,
        event: inhibitor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            inhibitor::Event::Active => {
                state.counts.shortcuts_inhibited_observed = true;
                state.report_ready();
            }
            inhibitor::Event::Inactive if state.requested => {
                state.finish("shortcuts_inhibitor_lost")
            }
            _ => {}
        }
    }
}
delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore layer_shell::ZwlrLayerShellV1);
delegate_noop!(State: ignore constraints::ZwpPointerConstraintsV1);
delegate_noop!(State: ignore inhibit_manager::ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(State: ignore relative_manager::ZwpRelativePointerManagerV1);
