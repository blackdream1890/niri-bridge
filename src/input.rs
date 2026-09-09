// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::{BTreeMap, BTreeSet};

/// Aggregates selected physical keyboards without duplicating a key held on two devices.
/// Values remain in memory and are never included in diagnostics.
#[derive(Default)]
pub struct PhysicalKeys {
    devices: BTreeMap<usize, BTreeSet<u16>>,
    pending: Vec<crate::protocol::InputEvent>,
}

impl PhysicalKeys {
    pub fn update(&mut self, device: usize, code: u16, pressed: bool) -> anyhow::Result<()> {
        if !(1..=255).contains(&code) {
            return Ok(());
        }
        let before = self.devices.values().any(|keys| keys.contains(&code));
        let keys = self.devices.entry(device).or_default();
        if pressed {
            keys.insert(code);
        } else {
            keys.remove(&code);
        }
        let after = self.devices.values().any(|keys| keys.contains(&code));
        if before != after {
            anyhow::ensure!(
                self.pending.len() < 512,
                "Physical keyboard queue is full; restoring local control"
            );
            self.pending.push(crate::protocol::InputEvent::Key {
                code,
                pressed: after,
            });
        }
        Ok(())
    }
    pub fn disconnect(&mut self, device: usize) -> anyhow::Result<()> {
        let held = self.devices.get(&device).cloned().unwrap_or_default();
        for code in held {
            self.update(device, code, false)?;
        }
        self.devices.remove(&device);
        Ok(())
    }
    pub fn held(&self) -> BTreeSet<u16> {
        self.devices.values().flatten().copied().collect()
    }
    pub fn drain(&mut self) -> Vec<crate::protocol::InputEvent> {
        std::mem::take(&mut self.pending)
    }
}

pub fn emergency_modifiers(keys: &BTreeSet<u16>) -> bool {
    [&[29, 97][..], &[56, 100][..], &[42, 54][..]]
        .iter()
        .all(|group| group.iter().any(|code| keys.contains(code)))
}

/// Held-state tracking shared by keyboard keys and pointer buttons.
/// Values are kept in memory only; this type deliberately does not implement Debug or Serialize.
#[derive(Default)]
pub struct HeldInputs {
    keys: BTreeSet<u16>,
}

impl HeldInputs {
    pub fn press(&mut self, code: u16) -> bool {
        self.keys.insert(code)
    }
    pub fn release(&mut self, code: u16) -> bool {
        self.keys.remove(&code)
    }
    pub fn release_all(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.keys).into_iter().collect()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Local,
    Sending,
    Receiving,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Returned,
    Disconnected,
    Emergency,
    LocalTakeover,
    Locked,
}

pub struct StopEffects {
    pub release_codes: Vec<u16>,
    pub release_capture: bool,
    pub notify_peer: bool,
}

/// One machine's control state; remote sessions are only started after authentication.
/// Lock state and physical activity must come from trusted local backends.
pub struct ControlState {
    mode: Mode,
    local_locked: bool,
    peer_locked: bool,
    held: HeldInputs,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            mode: Mode::Local,
            local_locked: false,
            peer_locked: false,
            held: HeldInputs::default(),
        }
    }
}

impl ControlState {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn start(&mut self, mode: Mode) -> bool {
        if self.mode != Mode::Local
            || self.local_locked
            || self.peer_locked
            || !matches!(mode, Mode::Sending | Mode::Receiving)
        {
            return false;
        }
        self.mode = mode;
        true
    }

    pub fn press(&mut self, code: u16) -> bool {
        matches!(self.mode, Mode::Sending | Mode::Receiving) && self.held.press(code)
    }

    pub fn release(&mut self, code: u16) -> bool {
        self.held.release(code)
    }

    pub fn stop(&mut self, reason: StopReason) -> StopEffects {
        let active = matches!(self.mode, Mode::Sending | Mode::Receiving);
        let effects = StopEffects {
            release_codes: self.held.release_all(),
            release_capture: self.mode == Mode::Sending,
            notify_peer: active && reason != StopReason::Disconnected,
        };
        self.mode = if self.local_locked || self.peer_locked {
            Mode::Paused
        } else {
            Mode::Local
        };
        effects
    }

    pub fn local_lock_changed(&mut self, locked: bool) -> StopEffects {
        if self.local_locked == locked {
            return StopEffects {
                release_codes: vec![],
                release_capture: false,
                notify_peer: false,
            };
        }
        self.local_locked = locked;
        self.stop(StopReason::Locked)
    }

    pub fn peer_lock_changed(&mut self, locked: bool) -> StopEffects {
        if self.peer_locked == locked {
            return StopEffects {
                release_codes: vec![],
                release_capture: false,
                notify_peer: false,
            };
        }
        self.peer_locked = locked;
        self.stop(StopReason::Locked)
    }

    /// Must be called only for hardware-originated input at the receiver, never injected input.
    pub fn receiver_physical_activity(&mut self) -> Option<StopEffects> {
        (self.mode == Mode::Receiving).then(|| self.stop(StopReason::LocalTakeover))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_key_aggregation_preserves_fast_taps_and_overlapping_devices() {
        let mut keys = PhysicalKeys::default();
        keys.update(0, 42, true).unwrap();
        keys.update(1, 42, true).unwrap();
        keys.update(0, 42, false).unwrap();
        assert!(keys.held().contains(&42));
        keys.disconnect(1).unwrap();
        keys.update(0, 30, true).unwrap();
        keys.update(0, 30, false).unwrap();
        let events = keys.drain();
        assert!(
            events
                == [(42, true), (42, false), (30, true), (30, false)]
                    .map(|(code, pressed)| crate::protocol::InputEvent::Key { code, pressed })
        );
        assert!(keys.held().is_empty());
        assert!(keys.drain().is_empty());
    }

    #[test]
    fn physical_keys_ignore_pointer_buttons_and_bound_the_queue() {
        let mut keys = PhysicalKeys::default();
        keys.update(0, 272, true).unwrap();
        assert!(keys.held().is_empty());
        for index in 0..512 {
            keys.update(0, 30, index % 2 == 0).unwrap();
        }
        assert!(keys.update(0, 30, true).is_err());
    }

    #[test]
    fn disconnect_releases_modifiers_and_buttons_once() {
        let mut state = ControlState::default();
        assert!(state.start(Mode::Receiving));
        state.press(29);
        state.press(272);
        state.press(29);
        let effects = state.stop(StopReason::Disconnected);
        assert_eq!(effects.release_codes, vec![29, 272]);
        assert!(!effects.notify_peer);
        assert_eq!(state.mode(), Mode::Local);
        assert!(
            state
                .stop(StopReason::Disconnected)
                .release_codes
                .is_empty()
        );
    }

    #[test]
    fn receiver_physical_input_ends_remote_control() {
        let mut state = ControlState::default();
        state.start(Mode::Receiving);
        state.press(42);
        let effects = state.receiver_physical_activity().unwrap();
        assert_eq!(effects.release_codes, vec![42]);
        assert!(effects.notify_peer);
        assert_eq!(state.mode(), Mode::Local);
    }

    #[test]
    fn source_physical_input_does_not_end_its_own_sending_session() {
        let mut state = ControlState::default();
        state.start(Mode::Sending);
        assert!(state.receiver_physical_activity().is_none());
        assert_eq!(state.mode(), Mode::Sending);
    }

    #[test]
    fn repeated_unlocked_notification_preserves_an_active_session() {
        let mut state = ControlState::default();
        state.start(Mode::Sending);
        state.press(29);
        assert!(!state.local_lock_changed(false).release_capture);
        assert!(!state.peer_lock_changed(false).notify_peer);
        assert_eq!(state.mode(), Mode::Sending);
        assert_eq!(state.stop(StopReason::Emergency).release_codes, vec![29]);
    }

    #[test]
    fn either_lock_stops_capture_and_unlock_does_not_restart_sharing() {
        let mut state = ControlState::default();
        state.start(Mode::Sending);
        state.press(125);
        let effects = state.peer_lock_changed(true);
        assert!(effects.release_capture);
        assert_eq!(effects.release_codes, vec![125]);
        assert_eq!(state.mode(), Mode::Paused);
        assert!(!state.start(Mode::Sending));
        state.local_lock_changed(true);
        state.peer_lock_changed(false);
        assert_eq!(state.mode(), Mode::Paused);
        state.local_lock_changed(false);
        assert_eq!(state.mode(), Mode::Local);
    }
}
