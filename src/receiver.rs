// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{Result, ensure};
use std::collections::BTreeSet;

use crate::{
    input::{ControlState, Mode, StopReason},
    protocol::InputEvent,
};

pub trait InputSink {
    fn emit(&mut self, event: &InputEvent) -> Result<()>;
    fn select_output(&mut self, _output: &str) -> Result<()> {
        anyhow::bail!("This input backend cannot select a pointer output")
    }
    fn configure_touchpads(&mut self, devices: &[crate::touchpad::Descriptor]) -> Result<()> {
        ensure!(
            devices.is_empty(),
            "This input backend does not support native touchpads"
        );
        Ok(())
    }
    fn reset_touchpads(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Owns held-input cleanup for one authenticated connection; never logs input payloads.
pub struct Receiver<S: InputSink> {
    sink: S,
    state: ControlState,
    session: Option<u64>,
    expected_sequence: u64,
    pending_releases: BTreeSet<u16>,
}

impl<S: InputSink> Receiver<S> {
    pub fn configure_touchpads(&mut self, devices: &[crate::touchpad::Descriptor]) -> Result<()> {
        ensure!(
            self.session.is_none() && devices.len() <= 4,
            "Cannot change touchpad devices during input"
        );
        self.sink.configure_touchpads(devices)
    }
    pub fn new(sink: S) -> Self {
        Self {
            sink,
            state: ControlState::default(),
            session: None,
            expected_sequence: 0,
            pending_releases: BTreeSet::new(),
        }
    }

    pub fn begin(&mut self, session: u64) -> Result<bool> {
        ensure!(session != 0, "Session identifier cannot be zero");
        ensure!(
            self.pending_releases.is_empty(),
            "Backend cleanup must complete before a new session"
        );
        if !self.state.start(Mode::Receiving) {
            return Ok(false);
        }
        self.session = Some(session);
        self.expected_sequence = 0;
        Ok(true)
    }

    /// Returns false for a stale or inactive session without injecting its events.
    pub fn input(&mut self, session: u64, sequence: u64, event: &InputEvent) -> Result<bool> {
        if self.session != Some(session) || self.state.mode() != Mode::Receiving {
            return Ok(false);
        }
        if sequence != self.expected_sequence {
            self.stop(StopReason::Disconnected)?;
            anyhow::bail!("Input sequence mismatch");
        }
        if let Err(error) = event.validate() {
            self.stop(StopReason::Disconnected)?;
            return Err(error);
        }
        let Some(next_sequence) = self.expected_sequence.checked_add(1) else {
            self.stop(StopReason::Disconnected)?;
            anyhow::bail!("Input sequence exhausted");
        };
        if let Some((code, true)) = event.held_transition() {
            self.state.press(code);
        }
        // Remember presses before attempting emission so a partially successful backend write
        // is still followed by a release on failure.
        if let Err(error) = self.sink.emit(event) {
            let _ = self.stop(StopReason::Disconnected);
            return Err(error);
        }
        if let Some((code, false)) = event.held_transition() {
            self.state.release(code);
        }
        self.expected_sequence = next_sequence;
        Ok(true)
    }

    pub fn end(&mut self, session: u64) -> Result<()> {
        if self.session == Some(session) {
            self.stop(StopReason::Returned)?;
        }
        Ok(())
    }

    pub fn stop(&mut self, reason: StopReason) -> Result<()> {
        self.session = None;
        let effects = self.state.stop(reason);
        let release = self.release(effects.release_codes);
        let pads = self.sink.reset_touchpads();
        release?;
        pads
    }

    pub fn local_lock(&mut self, locked: bool) -> Result<()> {
        let effects = self.state.local_lock_changed(locked);
        if self.state.mode() != Mode::Receiving {
            self.session = None;
            return self.release_with_touchpads(effects.release_codes);
        }
        self.release(effects.release_codes)
    }

    pub fn physical_activity(&mut self) -> Result<()> {
        if let Some(effects) = self.state.receiver_physical_activity() {
            self.session = None;
            self.release_with_touchpads(effects.release_codes)?;
        }
        Ok(())
    }

    pub fn peer_lock(&mut self, locked: bool) -> Result<()> {
        let effects = self.state.peer_lock_changed(locked);
        if self.state.mode() != Mode::Receiving {
            self.session = None;
            return self.release_with_touchpads(effects.release_codes);
        }
        self.release(effects.release_codes)
    }

    pub fn position(&mut self, event: &InputEvent) -> Result<()> {
        ensure!(
            matches!(event, InputEvent::Absolute { .. }),
            "Only an absolute pointer placement is allowed here"
        );
        event.validate()?;
        self.sink.emit(event)
    }

    pub fn position_on(&mut self, output: &str, event: &InputEvent) -> Result<()> {
        ensure!(
            self.pending_releases.is_empty()
                && (self.session.is_none() || self.expected_sequence == 0),
            "Release active input before changing pointer output"
        );
        ensure!(
            matches!(event, InputEvent::Absolute { .. }),
            "Only an absolute pointer placement is allowed here"
        );
        event.validate()?;
        self.sink.select_output(output)?;
        self.sink.emit(event)
    }

    fn release_with_touchpads(&mut self, codes: Vec<u16>) -> Result<()> {
        let keys = self.release(codes);
        let pads = self.sink.reset_touchpads();
        keys?;
        pads
    }
    fn release(&mut self, codes: Vec<u16>) -> Result<()> {
        self.pending_releases.extend(codes);
        let mut first_error = None;
        for code in self.pending_releases.clone() {
            match self.sink.emit(&InputEvent::release(code)) {
                Ok(()) => {
                    self.pending_releases.remove(&code);
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }
}

impl<S: InputSink> Drop for Receiver<S> {
    fn drop(&mut self) {
        let _ = self.stop(StopReason::Disconnected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    struct Sink(Arc<Mutex<Vec<InputEvent>>>);
    impl InputSink for Sink {
        fn emit(&mut self, event: &InputEvent) -> Result<()> {
            self.0.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    #[test]
    fn output_switches_require_releasing_active_input_and_select_the_requested_display() {
        struct SelectingSink(Arc<Mutex<Vec<String>>>);
        impl InputSink for SelectingSink {
            fn select_output(&mut self, output: &str) -> Result<()> {
                self.0.lock().unwrap().push(format!("output:{output}"));
                Ok(())
            }
            fn emit(&mut self, event: &InputEvent) -> Result<()> {
                self.0.lock().unwrap().push(match event {
                    InputEvent::Button { pressed, .. } => format!("button:{pressed}"),
                    InputEvent::Absolute { .. } => "position".into(),
                    _ => unreachable!(),
                });
                Ok(())
            }
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut receiver = Receiver::new(SelectingSink(events.clone()));
        let position = InputEvent::Absolute {
            x: 4,
            y: 600,
            width: 1080,
            height: 1920,
        };
        receiver.begin(1).unwrap();
        receiver
            .input(
                1,
                0,
                &InputEvent::Button {
                    code: 272,
                    pressed: true,
                },
            )
            .unwrap();
        assert!(receiver.position_on("portrait", &position).is_err());
        receiver.end(1).unwrap();
        receiver.position_on("portrait", &position).unwrap();
        receiver.position_on("landscape", &position).unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            [
                "button:true",
                "button:false",
                "output:portrait",
                "position",
                "output:landscape",
                "position"
            ]
        );
    }

    #[test]
    fn dropping_connection_releases_both_modifier_and_mouse_button() {
        let events = Arc::new(Mutex::new(Vec::new()));
        {
            let mut rx = Receiver::new(Sink(events.clone()));
            rx.begin(1).unwrap();
            rx.input(
                1,
                0,
                &InputEvent::Key {
                    code: 29,
                    pressed: true,
                },
            )
            .unwrap();
            rx.input(
                1,
                1,
                &InputEvent::Button {
                    code: 272,
                    pressed: true,
                },
            )
            .unwrap();
        }
        let actual = events.lock().unwrap();
        assert!(
            actual.as_slice()
                == [
                    InputEvent::Key {
                        code: 29,
                        pressed: true
                    },
                    InputEvent::Button {
                        code: 272,
                        pressed: true
                    },
                    InputEvent::Key {
                        code: 29,
                        pressed: false
                    },
                    InputEvent::Button {
                        code: 272,
                        pressed: false
                    }
                ]
        );
    }

    #[test]
    fn stale_input_and_end_cannot_change_a_new_session() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut rx = Receiver::new(Sink(events.clone()));
        rx.begin(2).unwrap();
        assert!(
            !rx.input(
                1,
                0,
                &InputEvent::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
        );
        rx.end(1).unwrap();
        assert!(
            rx.input(
                2,
                0,
                &InputEvent::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
        );
        assert_eq!(events.lock().unwrap().len(), 1);
    }

    #[test]
    fn invalid_sequence_releases_before_returning_an_error() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut rx = Receiver::new(Sink(events.clone()));
        rx.begin(1).unwrap();
        rx.input(
            1,
            0,
            &InputEvent::Key {
                code: 125,
                pressed: true,
            },
        )
        .unwrap();
        assert!(
            rx.input(
                1,
                3,
                &InputEvent::Key {
                    code: 30,
                    pressed: true
                }
            )
            .is_err()
        );
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(InputEvent::Key {
                code: 125,
                pressed: false
            })
        ));
    }

    #[test]
    fn physical_takeover_and_lock_reject_further_input() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut rx = Receiver::new(Sink(events));
        rx.begin(1).unwrap();
        rx.physical_activity().unwrap();
        assert!(
            !rx.input(
                1,
                0,
                &InputEvent::Key {
                    code: 30,
                    pressed: true
                }
            )
            .unwrap()
        );
        rx.local_lock(true).unwrap();
        assert!(!rx.begin(2).unwrap());
        rx.local_lock(false).unwrap();
        assert!(rx.begin(3).unwrap());
    }
}
