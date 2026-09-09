// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn validate(self) -> Result<()> {
        ensure!(
            [
                self.x,
                self.y,
                self.width,
                self.height,
                self.x + self.width,
                self.y + self.height
            ]
            .iter()
            .all(|v| v.is_finite()),
            "Screen coordinates must be finite"
        );
        ensure!(
            self.width >= 1.0 && self.height >= 1.0,
            "Screen dimensions must be at least one logical pixel"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boundary {
    pub edge: Edge,
    pub start: f64,
    pub end: f64,
}

impl Boundary {
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.start.is_finite()
                && self.end.is_finite()
                && self.start >= 0.0
                && self.end <= 1.0
                && self.start < self.end,
            "Boundary span must satisfy 0 <= start < end <= 1"
        );
        Ok(())
    }

    /// Maps the tangent position along a screen edge to a position within the configured span.
    /// The caller must first establish an actual crossing of this edge.
    pub fn fraction_at(self, rect: Rect, x: f64, y: f64) -> Result<Option<f64>> {
        self.validate()?;
        rect.validate()?;
        ensure!(
            x.is_finite() && y.is_finite(),
            "Pointer coordinates must be finite"
        );
        let along = match self.edge {
            Edge::Top | Edge::Bottom => (x - rect.x) / rect.width,
            Edge::Left | Edge::Right => (y - rect.y) / rect.height,
        };
        // Coordinate subtraction/division can move an exact endpoint by a few ULPs.
        let epsilon = 16.0 * f64::EPSILON;
        Ok(
            (along >= self.start - epsilon && along <= self.end + epsilon)
                .then(|| ((along - self.start) / (self.end - self.start)).clamp(0.0, 1.0)),
        )
    }

    /// Places a mapped pointer inside a target screen; inset is supplied by the capture backend.
    pub fn point_at(self, rect: Rect, fraction: f64, inset: f64) -> Result<(f64, f64)> {
        self.validate()?;
        rect.validate()?;
        ensure!(
            fraction.is_finite() && (0.0..=1.0).contains(&fraction),
            "Boundary fraction must be between zero and one"
        );
        ensure!(
            inset.is_finite() && inset > 0.0,
            "Entry inset must be finite and positive"
        );
        let t = self.start + fraction * (self.end - self.start);
        let ix = inset.min(rect.width / 2.0);
        let iy = inset.min(rect.height / 2.0);
        let tangent_x = (rect.x + t * rect.width).clamp(rect.x + 0.5, rect.x + rect.width - 0.5);
        let tangent_y = (rect.y + t * rect.height).clamp(rect.y + 0.5, rect.y + rect.height - 0.5);
        Ok(match self.edge {
            Edge::Top => (tangent_x, rect.y + iy),
            Edge::Bottom => (tangent_x, rect.y + rect.height - iy),
            Edge::Left => (rect.x + ix, tangent_y),
            Edge::Right => (rect.x + rect.width - ix, tangent_y),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub screens: Vec<Screen>,
    pub link: Link,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Screen {
    pub device: String,
    pub output: String,
    pub rect: Rect,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub device: String,
    pub output: String,
    pub boundary: Boundary,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub a: Endpoint,
    pub b: Endpoint,
}

impl Layout {
    pub fn validate(&self) -> Result<()> {
        let mut screens = BTreeMap::new();
        for screen in &self.screens {
            ensure!(
                !screen.device.trim().is_empty() && !screen.output.trim().is_empty(),
                "Device and output names cannot be empty"
            );
            screen.rect.validate()?;
            ensure!(
                screens
                    .insert((&screen.device, &screen.output), &screen.rect)
                    .is_none(),
                "Duplicate screen in layout"
            );
        }
        ensure!(
            self.link.a.device != self.link.b.device,
            "A cross-device link must connect different devices"
        );
        for endpoint in [&self.link.a, &self.link.b] {
            endpoint.boundary.validate()?;
            ensure!(
                screens.contains_key(&(&endpoint.device, &endpoint.output)),
                "Link refers to an unknown screen"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_bottom_edge_maps_to_laptop_top_in_logical_coordinates() {
        let desktop = Rect {
            x: 0.0,
            y: -191.0,
            width: 2648.0,
            height: 1489.0,
        };
        let laptop = Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1200.0,
        };
        let from = Boundary {
            edge: Edge::Bottom,
            start: 0.1,
            end: 0.5,
        };
        let to = Boundary {
            edge: Edge::Top,
            start: 0.0,
            end: 1.0,
        };
        let f = from
            .fraction_at(desktop, 0.3 * 2648.0, 1298.0)
            .unwrap()
            .unwrap();
        let (x, y) = to.point_at(laptop, f, 2.0).unwrap();
        assert!((x - 960.0).abs() < 1e-9);
        assert_eq!(y, 2.0);
        assert!(
            from.fraction_at(desktop, 0.9 * 2648.0, 1298.0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn reverse_mapping_returns_to_the_same_tangent_position() {
        let rect = Rect {
            x: -1490.0,
            y: -579.0,
            width: 1489.0,
            height: 2648.0,
        };
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            let boundary = Boundary {
                edge,
                start: 0.2,
                end: 0.8,
            };
            for f in [0.0, 0.1, 0.5, 0.9, 1.0] {
                let (x, y) = boundary.point_at(rect, f, 2.0).unwrap();
                let actual = boundary.fraction_at(rect, x, y).unwrap().unwrap();
                assert!((actual - f).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn full_edge_endpoints_remain_inside_screen() {
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let b = Boundary {
            edge: Edge::Top,
            start: 0.0,
            end: 1.0,
        };
        assert_eq!(b.point_at(rect, 0.0, 2.0).unwrap(), (0.5, 2.0));
        assert_eq!(b.point_at(rect, 1.0, 2.0).unwrap(), (99.5, 2.0));
    }

    #[test]
    fn invalid_coordinates_and_empty_spans_are_rejected() {
        let r = Rect {
            x: 0.0,
            y: 0.0,
            width: f64::INFINITY,
            height: 100.0,
        };
        assert!(r.validate().is_err());
        assert!(
            Boundary {
                edge: Edge::Top,
                start: 0.5,
                end: 0.5
            }
            .validate()
            .is_err()
        );
        assert!(
            Boundary {
                edge: Edge::Top,
                start: f64::NAN,
                end: 1.0
            }
            .validate()
            .is_err()
        );
    }
}
