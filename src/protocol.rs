// SPDX-License-Identifier: GPL-3.0-or-later
//! Versioned input frames. Payloads deliberately do not implement Debug.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u16 = 5;
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollSource {
    Wheel,
    Finger,
    Continuous,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputEvent {
    Touchpad {
        device: u8,
        time_us: u64,
        events: Vec<crate::touchpad::Event>,
    },
    Key {
        code: u16,
        pressed: bool,
    },
    Button {
        code: u16,
        pressed: bool,
    },
    Motion {
        dx: f64,
        dy: f64,
    },
    Absolute {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Scroll {
        axis: Axis,
        amount: f64,
        source: ScrollSource,
    },
    ScrollStop {
        axis: Axis,
        source: ScrollSource,
    },
}

impl InputEvent {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Touchpad {
                device,
                time_us,
                events,
            } => {
                ensure!(*device < 4, "Invalid touchpad index");
                ensure!(
                    (1..=1_000_000_000_000_000).contains(time_us),
                    "Invalid touchpad event time"
                );
                crate::touchpad::validate_frame(events)?;
            }
            Self::Key { code, .. } => {
                ensure!((1..=255).contains(code), "Unsupported keyboard code")
            }
            Self::Button { code, .. } => {
                ensure!((272..=279).contains(code), "Unsupported pointer button")
            }
            Self::Motion { dx, dy } => ensure!(
                [dx, dy].iter().all(|v| v.is_finite() && v.abs() <= 32768.0),
                "Invalid relative motion"
            ),
            Self::Absolute {
                x,
                y,
                width,
                height,
            } => ensure!(
                *width > 0 && *height > 0 && *x < *width && *y < *height,
                "Invalid absolute position"
            ),
            Self::Scroll { amount, .. } => ensure!(
                amount.is_finite() && amount.abs() <= 32768.0,
                "Invalid scroll amount"
            ),
            Self::ScrollStop { .. } => {}
        }
        Ok(())
    }

    pub fn held_transition(&self) -> Option<(u16, bool)> {
        match self {
            Self::Key { code, pressed } | Self::Button { code, pressed } => Some((*code, *pressed)),
            _ => None,
        }
    }

    pub fn release(code: u16) -> Self {
        if code >= 272 {
            Self::Button {
                code,
                pressed: false,
            }
        } else {
            Self::Key {
                code,
                pressed: false,
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgePosition {
    pub edge_id: String,
    pub fraction: f64,
}

impl EdgePosition {
    fn validate(&self) -> Result<()> {
        ensure!(
            crate::bridge::valid_edge_id(&self.edge_id),
            "Invalid edge identifier"
        );
        ensure!(
            self.fraction.is_finite() && (0.0..=1.0).contains(&self.fraction),
            "Invalid edge position"
        );
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    Desktop {
        info: crate::control::Desktop,
    },
    Layout {
        message: crate::control::LayoutMessage,
    },
    Touchpads {
        devices: Vec<crate::touchpad::Descriptor>,
    },
    Hello {
        version: u16,
    },
    Begin {
        session: u64,
        entry: EdgePosition,
    },
    Input {
        session: u64,
        sequence: u64,
        event: InputEvent,
    },
    End {
        session: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit: Option<EdgePosition>,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    LockState {
        locked: bool,
    },
}

impl Message {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Desktop { info } => info.validate()?,
            Self::Layout { message } => message.validate()?,
            Self::Hello { version } => ensure!(*version == VERSION, "Unsupported protocol version"),
            Self::Touchpads { devices } => {
                ensure!(devices.len() <= 4, "Too many touchpads");
                for descriptor in devices {
                    descriptor.validate()?;
                }
            }
            Self::Begin { session, entry } => {
                ensure!(*session != 0, "Session identifier cannot be zero");
                entry.validate()?;
            }
            Self::Input { session, event, .. } => {
                ensure!(*session != 0, "Session identifier cannot be zero");
                event.validate()?;
            }
            Self::End { session, exit } => {
                ensure!(*session != 0, "Session identifier cannot be zero");
                if let Some(exit) = exit {
                    exit.validate()?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

pub async fn write_frame(writer: &mut (impl AsyncWrite + Unpin), message: &Message) -> Result<()> {
    message.validate()?;
    let bytes = serde_json::to_vec(message)?;
    ensure!(bytes.len() <= MAX_FRAME_BYTES, "Protocol frame too large");
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(&bytes);
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

/// If this future is cancelled after a partial read, the connection must be closed, not reused.
pub async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> Result<Message> {
    let length = reader.read_u32().await? as usize;
    ensure!(
        length > 0 && length <= MAX_FRAME_BYTES,
        "Invalid protocol frame length"
    );
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    let message: Message =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Invalid protocol frame"))?;
    message.validate()?;
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fragmented_frame_is_reassembled() {
        let (mut a, mut b) = tokio::io::duplex(8);
        let sender = tokio::spawn(async move {
            write_frame(
                &mut a,
                &Message::Input {
                    session: 1,
                    sequence: 0,
                    event: InputEvent::Key {
                        code: 30,
                        pressed: true,
                    },
                },
            )
            .await
            .unwrap();
        });
        assert!(matches!(
            read_frame(&mut b).await.unwrap(),
            Message::Input {
                session: 1,
                sequence: 0,
                event: InputEvent::Key {
                    code: 30,
                    pressed: true
                }
            }
        ));
        sender.await.unwrap();
    }

    #[tokio::test]
    async fn oversize_header_is_rejected_without_reading_the_body() {
        let data = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
        assert!(read_frame(&mut data.as_slice()).await.is_err());
    }

    #[test]
    fn invalid_numeric_values_never_reach_a_backend() {
        assert!(
            InputEvent::Motion {
                dx: f64::NAN,
                dy: 0.0
            }
            .validate()
            .is_err()
        );
        assert!(
            InputEvent::Key {
                code: 0,
                pressed: true
            }
            .validate()
            .is_err()
        );
        assert!(
            InputEvent::Absolute {
                x: 100,
                y: 0,
                width: 100,
                height: 100
            }
            .validate()
            .is_err()
        );
        assert!(
            Message::Hello {
                version: VERSION + 1
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn entry_and_return_positions_require_bounded_ids_and_finite_fractions() {
        let valid = EdgePosition {
            edge_id: "side-connection".into(),
            fraction: 0.75,
        };
        assert!(
            Message::Begin {
                session: 1,
                entry: valid.clone()
            }
            .validate()
            .is_ok()
        );
        assert!(
            Message::End {
                session: 1,
                exit: Some(valid)
            }
            .validate()
            .is_ok()
        );
        for (id, fraction) in [
            ("".into(), 0.5),
            ("x".repeat(65), 0.5),
            ("../edge".into(), 0.5),
            ("side".into(), f64::NAN),
            ("side".into(), 1.1),
        ] {
            let position = EdgePosition {
                edge_id: id,
                fraction,
            };
            assert!(
                Message::Begin {
                    session: 1,
                    entry: position.clone()
                }
                .validate()
                .is_err()
            );
            assert!(
                Message::End {
                    session: 1,
                    exit: Some(position)
                }
                .validate()
                .is_err()
            );
        }
    }
}
