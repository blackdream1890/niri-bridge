// SPDX-License-Identifier: GPL-3.0-or-later
//! Native multi-touch forwarding. Device identities and touch coordinates are never logged.
use anyhow::{Context, Result, ensure};
use evdev::raw_stream::RawDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, InputId, KeyCode, PropType, UinputAbsSetup,
    uinput::VirtualDevice,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io,
    os::{
        fd::{AsFd, AsRawFd, OwnedFd},
        unix::fs::OpenOptionsExt,
    },
    path::PathBuf,
    time::UNIX_EPOCH,
};
use tokio::io::unix::AsyncFd;

pub type CapturedFrame = (u8, u64, Vec<Event>);

fn monotonic_us() -> Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    ensure!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } == 0,
        "Cannot read the input clock"
    );
    Ok(time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1_000)
}

#[derive(Default)]
struct RemoteClock {
    offset: Option<i128>,
    last: u64,
}
impl RemoteClock {
    fn map(&mut self, source: u64, now: u64) -> u64 {
        let observed = i128::from(now) - i128::from(source);
        let offset = self
            .offset
            .map_or(observed, |previous| previous.min(observed));
        self.offset = Some(offset);
        let time = (i128::from(source) + offset).max(1) as u64;
        let time = time.max(self.last).min(now);
        self.last = time;
        time
    }
}

const TOUCH: u16 = 330;
const SLOT: u16 = 47;
const TRACKING: u16 = 57;
pub const DEVICE_NAME: &str = "NiriBridge Virtual Touchpad";

fn touch_key(code: u16) -> bool {
    matches!(code,272..=274|325|328|330|333..=335)
}
fn touch_axis(code: u16) -> bool {
    matches!(code, 0 | 1 | 24 | 28 | 47..=61)
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AxisInfo {
    pub code: u16,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub keys: Vec<u16>,
    pub properties: Vec<u16>,
    pub axes: Vec<AxisInfo>,
}
impl Descriptor {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (2..=16).contains(&self.keys.len()) && self.keys.iter().all(|code| touch_key(*code)),
            "Unsupported touchpad keys"
        );
        ensure!(
            self.keys.iter().copied().collect::<BTreeSet<_>>().len() == self.keys.len(),
            "Duplicate touchpad key"
        );
        ensure!(
            self.keys.contains(&TOUCH) && self.keys.contains(&325),
            "Touchpad contact capabilities are missing"
        );
        ensure!(
            self.properties.len() <= 5
                && self.properties.contains(&0)
                && self
                    .properties
                    .iter()
                    .all(|p| matches!(*p, 0 | 2 | 3 | 4 | 6)),
            "Unsupported touchpad properties"
        );
        ensure!(
            (4..=20).contains(&self.axes.len()),
            "Invalid touchpad axis count"
        );
        let mut codes = BTreeSet::new();
        for axis in &self.axes {
            ensure!(
                touch_axis(axis.code) && codes.insert(axis.code),
                "Unsupported or duplicate touchpad axis"
            );
            ensure!(
                (-1_000_000..=1_000_000).contains(&axis.minimum)
                    && axis.minimum <= axis.maximum
                    && axis.maximum <= 1_000_000,
                "Invalid touchpad axis range"
            );
            ensure!(
                [axis.fuzz, axis.flat, axis.resolution]
                    .iter()
                    .all(|v| (0..=100_000).contains(v)),
                "Invalid touchpad axis calibration"
            );
        }
        ensure!(
            [0, 1, SLOT, 53, 54, TRACKING]
                .iter()
                .all(|code| codes.contains(code)),
            "A slot-based multi-touch touchpad is required"
        );
        let slot = self.axes.iter().find(|a| a.code == SLOT).unwrap();
        ensure!(
            slot.minimum == 0 && (0..32).contains(&slot.maximum),
            "Unsupported touchpad contact count"
        );
        Ok(())
    }
    pub fn from_device(device: &RawDevice) -> Result<Self> {
        let keys = device
            .supported_keys()
            .context("Touchpad keys are missing")?
            .iter()
            .map(|k| k.code())
            .filter(|k| touch_key(*k))
            .collect();
        let properties = device.properties().iter().map(|p| p.0).collect();
        let axes = device
            .get_absinfo()?
            .map(|(code, a)| AxisInfo {
                code: code.0,
                minimum: a.minimum(),
                maximum: a.maximum(),
                fuzz: a.fuzz(),
                flat: a.flat(),
                resolution: a.resolution(),
            })
            .collect();
        let value = Self {
            keys,
            properties,
            axes,
        };
        value.validate()?;
        Ok(value)
    }
    fn slots(&self) -> usize {
        (self.axes.iter().find(|a| a.code == SLOT).unwrap().maximum + 1) as usize
    }
    fn validate_frame(&self, frame: &[Event]) -> Result<()> {
        validate_frame(frame)?;
        for event in frame {
            match event.kind {
                Kind::Key => ensure!(
                    self.keys.contains(&event.code),
                    "Touchpad key was not advertised"
                ),
                Kind::Absolute => {
                    let axis = self
                        .axes
                        .iter()
                        .find(|a| a.code == event.code)
                        .context("Touchpad axis was not advertised")?;
                    ensure!(
                        (axis.minimum..=axis.maximum).contains(&event.value)
                            || (event.code == TRACKING && event.value == -1),
                        "Touchpad value is outside its axis range"
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Key,
    Absolute,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub kind: Kind,
    pub code: u16,
    pub value: i32,
}
impl Event {
    fn key(code: u16, value: i32) -> Self {
        Self {
            kind: Kind::Key,
            code,
            value,
        }
    }
    fn abs(code: u16, value: i32) -> Self {
        Self {
            kind: Kind::Absolute,
            code,
            value,
        }
    }
    fn kernel(&self, time_us: u64) -> evdev::InputEvent {
        libc::input_event {
            time: libc::timeval {
                tv_sec: (time_us / 1_000_000) as _,
                tv_usec: (time_us % 1_000_000) as _,
            },
            type_: match self.kind {
                Kind::Key => 1,
                Kind::Absolute => 3,
            },
            code: self.code,
            value: self.value,
        }
        .into()
    }
}
pub fn validate_frame(events: &[Event]) -> Result<()> {
    ensure!(
        !events.is_empty() && events.len() <= 512,
        "Invalid touchpad frame size"
    );
    for e in events {
        ensure!(
            match e.kind {
                Kind::Key => touch_key(e.code) && (0..=1).contains(&e.value),
                Kind::Absolute => touch_axis(e.code) && (-1_000_000..=1_000_000).contains(&e.value),
            },
            "Invalid touchpad event"
        );
    }
    Ok(())
}

/// Current contacts only, used to preserve a stroke as its destination changes.
struct TouchState {
    slot: usize,
    keys: BTreeSet<u16>,
    axes: BTreeMap<u16, i32>,
    contacts: Vec<BTreeMap<u16, i32>>,
}
impl TouchState {
    fn read(device: &RawDevice, descriptor: &Descriptor) -> Result<Self> {
        let mut state = Self::new(descriptor);
        state.keys = device
            .get_key_state()?
            .iter()
            .map(|k| k.code())
            .filter(|code| touch_key(*code))
            .collect();
        for (code, info) in device.get_absinfo()? {
            if code.0 < SLOT {
                state.axes.insert(code.0, info.value());
            } else if code.0 == SLOT {
                state.slot = info.value() as usize;
            } else {
                let mut values = vec![0i32; descriptor.slots() + 1];
                values[0] = code.0 as i32;
                let request = 0x8000_450a_u64 | ((values.len() * 4) as u64) << 16;
                ensure!(
                    unsafe {
                        libc::ioctl(
                            device.as_raw_fd(),
                            request as libc::c_ulong,
                            values.as_mut_ptr(),
                        )
                    } >= 0,
                    "Cannot inspect touchpad contacts"
                );
                for (index, value) in values[1..].iter().enumerate() {
                    state.contacts[index].insert(code.0, *value);
                }
            }
        }
        ensure!(
            state.slot < state.contacts.len(),
            "Touchpad has an invalid current slot"
        );
        Ok(state)
    }
    fn idle(&self) -> bool {
        !self.keys.contains(&TOUCH)
            && self
                .contacts
                .iter()
                .all(|c| c.get(&TRACKING).is_none_or(|v| *v < 0))
    }
    fn new(descriptor: &Descriptor) -> Self {
        Self {
            slot: 0,
            keys: BTreeSet::new(),
            axes: descriptor
                .axes
                .iter()
                .filter(|a| a.code < SLOT)
                .map(|a| (a.code, a.minimum))
                .collect(),
            contacts: (0..descriptor.slots())
                .map(|_| BTreeMap::from([(TRACKING, -1)]))
                .collect(),
        }
    }
    fn apply(&mut self, events: &[Event]) {
        for event in events {
            match event.kind {
                Kind::Key => {
                    if event.value == 0 {
                        self.keys.remove(&event.code);
                    } else {
                        self.keys.insert(event.code);
                    }
                }
                Kind::Absolute if event.code == SLOT => self.slot = event.value as usize,
                Kind::Absolute if event.code > SLOT => {
                    self.contacts[self.slot].insert(event.code, event.value);
                }
                Kind::Absolute => {
                    self.axes.insert(event.code, event.value);
                }
            }
        }
    }
    fn snapshot(&self) -> Vec<Event> {
        let mut events = vec![Event::key(TOUCH, i32::from(self.keys.contains(&TOUCH)))];
        for (index, contact) in self.contacts.iter().enumerate() {
            let tracking = *contact.get(&TRACKING).unwrap_or(&-1);
            if tracking >= 0 {
                events.push(Event::abs(SLOT, index as i32));
                events.push(Event::abs(TRACKING, tracking));
                events.extend(
                    contact
                        .iter()
                        .filter(|(c, _)| **c != TRACKING)
                        .map(|(c, v)| Event::abs(*c, *v)),
                );
            }
        }
        events.extend(self.axes.iter().map(|(c, v)| Event::abs(*c, *v)));
        events.extend(
            self.keys
                .iter()
                .filter(|c| **c != TOUCH)
                .map(|c| Event::key(*c, 1)),
        );
        events.push(Event::abs(SLOT, self.slot as i32));
        events
    }
    fn releases(&self) -> Vec<Event> {
        let mut events = vec![Event::key(TOUCH, 0)];
        for index in 0..self.contacts.len() {
            events.push(Event::abs(SLOT, index as i32));
            events.push(Event::abs(TRACKING, -1));
        }
        events.extend(
            self.keys
                .iter()
                .filter(|c| **c != TOUCH)
                .map(|c| Event::key(*c, 0)),
        );
        events
    }
}

pub struct VirtualTouchpad {
    device: VirtualDevice,
    descriptor: Descriptor,
    state: TouchState,
    remote_clock: RemoteClock,
    last_time: u64,
}
impl VirtualTouchpad {
    pub fn create(descriptor: &Descriptor) -> Result<Self> {
        descriptor.validate()?;
        let keys: AttributeSet<KeyCode> =
            descriptor.keys.iter().copied().map(KeyCode::new).collect();
        let properties: AttributeSet<PropType> = descriptor
            .properties
            .iter()
            .copied()
            .map(PropType)
            .collect();
        let mut builder = VirtualDevice::builder()?
            .name(DEVICE_NAME)
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1))
            .with_phys(c"niri-bridge/touchpad")?
            .with_keys(&keys)?
            .with_properties(&properties)?;
        for a in &descriptor.axes {
            builder = builder.with_absolute_axis(&UinputAbsSetup::new(
                AbsoluteAxisCode(a.code),
                AbsInfo::new(
                    if a.code == TRACKING { -1 } else { a.minimum },
                    a.minimum,
                    a.maximum,
                    a.fuzz,
                    a.flat,
                    a.resolution,
                ),
            ))?;
        }
        let mut device = builder.build()?;
        crate::uinput::wait_for_niri(&mut device)?;
        Ok(Self {
            device,
            descriptor: descriptor.clone(),
            state: TouchState::new(descriptor),
            remote_clock: RemoteClock::default(),
            last_time: 0,
        })
    }
    pub fn emit(&mut self, events: &[Event]) -> Result<()> {
        self.emit_at(events, monotonic_us()?)
    }
    pub fn emit_remote(&mut self, events: &[Event], time_us: u64) -> Result<()> {
        let now = monotonic_us()?;
        let local = self.remote_clock.map(time_us, now);
        self.emit_at(events, local)
    }
    fn emit_at(&mut self, events: &[Event], time_us: u64) -> Result<()> {
        self.descriptor.validate_frame(events)?;
        let now = monotonic_us()?;
        ensure!(
            time_us <= now && now - time_us <= 200_000,
            "Touchpad input became too delayed; restoring local control"
        );
        let time_us = time_us.max(self.last_time).min(now);
        self.device.emit(
            &events
                .iter()
                .map(|event| event.kernel(time_us))
                .collect::<Vec<_>>(),
        )?;
        self.last_time = time_us;
        self.state.apply(events);
        Ok(())
    }
    pub fn reset(&mut self) -> Result<()> {
        let release = self.state.releases();
        self.emit(&release)?;
        self.state = TouchState::new(&self.descriptor);
        self.remote_clock = RemoteClock::default();
        Ok(())
    }
}
impl Drop for VirtualTouchpad {
    fn drop(&mut self) {
        let _ = self.reset();
    }
}

struct PhysicalTouchpad {
    device: RawDevice,
    readiness: AsyncFd<OwnedFd>,
    descriptor: Descriptor,
    state: TouchState,
    mirror: VirtualTouchpad,
    grabbed: bool,
    frame: Vec<Event>,
}
/// Mirrors locally while idle and forwards the same native touch data while sharing.
/// Grabbing starts only with all fingers up, so the original compositor device is neutral.
pub struct Router {
    pads: Vec<PhysicalTouchpad>,
    remote: bool,
    pending: Vec<CapturedFrame>,
}
impl Router {
    pub fn new(paths: &[PathBuf]) -> Result<Self> {
        let mut pads = Vec::new();
        let mut seen = BTreeSet::new();
        for path in paths {
            let path = match path.canonicalize() {
                Ok(p) => p,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)?;
            let device = RawDevice::from_fd(file.into())?;
            let is_pad = device.properties().contains(PropType::POINTER)
                && !device.properties().contains(PropType::DIRECT)
                && device
                    .supported_keys()
                    .is_some_and(|keys| keys.contains(KeyCode::BTN_TOOL_FINGER));
            if !is_pad {
                continue;
            }
            ensure!(
                !device
                    .physical_path()
                    .is_some_and(|p| p.starts_with("niri-bridge/")),
                "A virtual device cannot be a physical touchpad source"
            );
            ensure!(pads.len() < 4, "At most four touchpads may be shared");
            let descriptor = Descriptor::from_device(&device)?;
            let clock = libc::CLOCK_MONOTONIC;
            ensure!(
                unsafe { libc::ioctl(device.as_raw_fd(), 0x4004_45a0 as libc::c_ulong, &clock) }
                    >= 0,
                "Cannot select the touchpad input clock"
            );
            let readiness = AsyncFd::new(device.as_fd().try_clone_to_owned()?)?;
            let mirror = VirtualTouchpad::create(&descriptor)?;
            let state = TouchState::new(&descriptor);
            pads.push(PhysicalTouchpad {
                device,
                readiness,
                descriptor,
                state,
                mirror,
                grabbed: false,
                frame: Vec::new(),
            });
        }
        Ok(Self {
            pads,
            remote: false,
            pending: Vec::new(),
        })
    }
    pub fn descriptors(&self) -> Vec<Descriptor> {
        self.pads.iter().map(|p| p.descriptor.clone()).collect()
    }
    pub fn sample(&mut self) -> Result<bool> {
        let mut active = false;
        for (index, pad) in self.pads.iter_mut().enumerate() {
            let mut events = Vec::new();
            loop {
                match pad.device.fetch_events() {
                    Ok(batch) => {
                        let before = events.len();
                        events.extend(batch);
                        ensure!(events.len() <= 4096, "Touchpad input queue is full");
                        if events.len() == before {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => anyhow::bail!("Touchpad disconnected or became unavailable"),
                }
            }
            for event in events {
                if !pad.grabbed {
                    continue;
                }
                active |= matches!(event.event_type().0, 1 | 3);
                match event.event_type().0 {
                    0 if event.code() == 3 => {
                        anyhow::bail!("Touchpad events were lost; restoring local control")
                    }
                    0 if event.code() == 0 && !pad.frame.is_empty() => {
                        ensure!(
                            self.pending.len() < 128,
                            "Touchpad forwarding queue is full"
                        );
                        let frame = std::mem::take(&mut pad.frame);
                        pad.descriptor.validate_frame(&frame)?;
                        pad.state.apply(&frame);
                        let time_us =
                            event.timestamp().duration_since(UNIX_EPOCH)?.as_micros() as u64;
                        if self.remote {
                            self.pending.push((index as u8, time_us, frame));
                        } else {
                            pad.mirror.emit_at(&frame, time_us)?;
                        }
                    }
                    1 if touch_key(event.code()) && event.value() != 2 => {
                        pad.frame.push(Event::key(event.code(), event.value()))
                    }
                    3 if touch_axis(event.code()) => {
                        pad.frame.push(Event::abs(event.code(), event.value()))
                    }
                    _ => {}
                }
                ensure!(pad.frame.len() <= 512, "Touchpad frame is too large");
            }
            if !pad.grabbed && !pad.device.get_key_state()?.contains(KeyCode::BTN_TOUCH) {
                pad.device
                    .grab()
                    .context("Cannot acquire the selected touchpad")?;
                // Earlier frames were already delivered to Niri before this grab.
                loop {
                    match pad.device.fetch_events() {
                        Ok(events) => {
                            if events.count() == 0 {
                                break;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e.into()),
                    }
                }
                let state = TouchState::read(&pad.device, &pad.descriptor)?;
                if state.idle() {
                    pad.state = state;
                    pad.grabbed = true;
                } else {
                    pad.device.ungrab()?;
                }
            }
        }
        Ok(active)
    }
    pub async fn wait_ready(&self) -> Result<()> {
        if self.pads.is_empty() {
            return std::future::pending().await;
        }
        let waiting = self
            .pads
            .iter()
            .map(|pad| {
                Box::pin(async {
                    let mut guard = pad.readiness.readable().await?;
                    guard.clear_ready();
                    Ok::<(), anyhow::Error>(())
                })
            })
            .collect::<Vec<_>>();
        futures_util::future::select_all(waiting).await.0
    }
    pub fn route(&mut self, remote: bool) -> Result<Vec<CapturedFrame>> {
        if self.remote == remote {
            return Ok(Vec::new());
        }
        self.remote = remote;
        self.pending.clear();
        let mut snapshots = Vec::new();
        for (index, pad) in self.pads.iter_mut().enumerate().filter(|(_, p)| p.grabbed) {
            let snapshot = pad.state.snapshot();
            if remote {
                pad.mirror.reset()?;
                snapshots.push((index as u8, monotonic_us()?, snapshot));
            } else {
                pad.mirror.emit(&snapshot)?;
            }
        }
        Ok(snapshots)
    }
    pub fn drain(&mut self) -> Vec<CapturedFrame> {
        std::mem::take(&mut self.pending)
    }
}
impl Drop for Router {
    fn drop(&mut self) {
        for pad in &mut self.pads {
            let _ = pad.mirror.reset();
            if pad.grabbed {
                let _ = pad.device.ungrab();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batched_arrivals_keep_hardware_frame_intervals_for_pointer_acceleration() {
        let mut clock = RemoteClock::default();
        assert_eq!(clock.map(100_000, 2_000_000), 2_000_000);
        assert_eq!(clock.map(108_000, 2_010_000), 2_008_000);
        assert_eq!(clock.map(116_000, 2_018_000), 2_016_000);
        let first = clock.map(124_000, 2_040_000);
        let second = clock.map(132_000, 2_040_000);
        assert_eq!(second - first, 8_000);
        assert!(second <= 2_040_000);
    }
    #[test]
    fn clock_rebasing_handles_different_uptimes_and_never_moves_backwards() {
        let mut clock = RemoteClock::default();
        let first = clock.map(9_000_000, 10_000);
        let second = clock.map(9_008_000, 18_001);
        assert_eq!(second - first, 8_000);
        let third = clock.map(9_016_000, 24_000);
        assert!((second..=24_000).contains(&third));
        assert!(clock.map(9_015_000, 25_000) >= third);
    }
    fn descriptor() -> Descriptor {
        Descriptor {
            keys: vec![272, 325, 328, 330, 333, 334, 335],
            properties: vec![0, 2],
            axes: [
                (0, 1000),
                (1, 1000),
                (47, 4),
                (53, 1000),
                (54, 1000),
                (57, 65535),
            ]
            .map(|(code, maximum)| AxisInfo {
                code,
                minimum: 0,
                maximum,
                fuzz: 0,
                flat: 0,
                resolution: if matches!(code, 0 | 1 | 53 | 54) {
                    10
                } else {
                    0
                },
            })
            .to_vec(),
        }
    }
    #[test]
    fn native_contact_snapshot_preserves_each_finger_and_releases_all() {
        let descriptor = descriptor();
        descriptor.validate().unwrap();
        let mut state = TouchState::new(&descriptor);
        let frame = vec![
            Event::key(330, 1),
            Event::key(334, 1),
            Event::abs(47, 0),
            Event::abs(57, 12),
            Event::abs(53, 250),
            Event::abs(54, 600),
            Event::abs(47, 1),
            Event::abs(57, 13),
            Event::abs(53, 650),
            Event::abs(54, 600),
            Event::abs(0, 250),
            Event::abs(1, 600),
        ];
        descriptor.validate_frame(&frame).unwrap();
        state.apply(&frame);
        let snapshot = state.snapshot();
        descriptor.validate_frame(&snapshot).unwrap();
        let mut destination = TouchState::new(&descriptor);
        destination.apply(&snapshot);
        assert!(
            destination.contacts == state.contacts
                && destination.keys == state.keys
                && destination.axes == state.axes
        );
        let releases = destination.releases();
        descriptor.validate_frame(&releases).unwrap();
        destination.apply(&releases);
        assert!(destination.idle());
        assert!(destination.keys.is_empty());
    }
    #[test]
    fn rejects_touchscreen_keyboard_keys_and_unbounded_axes() {
        let mut d = descriptor();
        d.properties.push(1);
        assert!(d.validate().is_err());
        let mut d = descriptor();
        d.keys.push(116);
        assert!(d.validate().is_err());
        let mut d = descriptor();
        d.axes.iter_mut().find(|a| a.code == 47).unwrap().maximum = 128;
        assert!(d.validate().is_err());
        let mut d = descriptor();
        d.axes[0].maximum = i32::MAX;
        assert!(d.validate().is_err());
    }
    #[test]
    fn rejects_frames_before_they_can_index_a_slot_or_emit_unsupported_input() {
        let d = descriptor();
        assert!(d.validate_frame(&[Event::abs(47, 5)]).is_err());
        assert!(d.validate_frame(&[Event::abs(53, -1)]).is_err());
        assert!(d.validate_frame(&[Event::abs(57, -1)]).is_ok());
        assert!(d.validate_frame(&[Event::key(116, 1)]).is_err());
        assert!(d.validate_frame(&[Event::key(330, 2)]).is_err());
        assert!(d.validate_frame(&vec![Event::abs(0, 1); 513]).is_err());
    }
}
