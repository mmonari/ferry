//! Whole-desktop pointer mapping (new in ferry, not in gliff): turn a global
//! logical position into `motion_absolute` arguments for either one unbound
//! pointer or one output-bound pointer per monitor. Pure logic.

/// One monitor in global logical coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputRect {
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl OutputRect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// Squared distance from a point to this rectangle (0 inside).
    fn dist2(&self, x: f64, y: f64) -> f64 {
        let dx = (self.x - x).max(0.0).max(x - (self.x + self.w - 1.0));
        let dy = (self.y - y).max(0.0).max(y - (self.y + self.h - 1.0));
        dx * dx + dy * dy
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub outputs: Vec<OutputRect>,
}

/// Arguments for one `motion_absolute(x, y, x_extent, y_extent)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abs {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Layout {
    /// The layout of Hyprland's enabled monitors, each as its logical rectangle
    /// (mode after transform, divided by the fractional scale). Monitors keep
    /// the order Hyprland lists them in.
    pub fn from_monitors(monitors: &[ferry_hypr_ipc::Monitor]) -> Self {
        let outputs = monitors
            .iter()
            .filter(|m| !m.disabled)
            .map(|m| {
                let (x, y, w, h) = m.logical_rect();
                OutputRect {
                    name: m.name.clone(),
                    x,
                    y,
                    w,
                    h,
                }
            })
            .collect();
        Self { outputs }
    }

    /// Bounding box `(x, y, w, h)` of every output.
    pub fn bbox(&self) -> (f64, f64, f64, f64) {
        if self.outputs.is_empty() {
            return (0.0, 0.0, 1.0, 1.0);
        }
        let x0 = self.outputs.iter().map(|o| o.x).fold(f64::MAX, f64::min);
        let y0 = self.outputs.iter().map(|o| o.y).fold(f64::MAX, f64::min);
        let x1 = self
            .outputs
            .iter()
            .map(|o| o.x + o.w)
            .fold(f64::MIN, f64::max);
        let y1 = self
            .outputs
            .iter()
            .map(|o| o.y + o.h)
            .fold(f64::MIN, f64::max);
        (x0, y0, x1 - x0, y1 - y0)
    }

    /// The output containing the point, else the nearest one (gaps in a
    /// non-rectangular layout snap to the closest monitor).
    pub fn locate(&self, x: f64, y: f64) -> Option<usize> {
        if let Some(i) = self.outputs.iter().position(|o| o.contains(x, y)) {
            return Some(i);
        }
        self.outputs
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.dist2(x, y).total_cmp(&b.1.dist2(x, y)))
            .map(|(i, _)| i)
    }

    /// Unbound pointer: position against the layout bounding box.
    pub fn unbound(&self, x: f64, y: f64) -> Abs {
        let (bx, by, bw, bh) = self.bbox();
        abs_in(x - bx, y - by, bw, bh)
    }

    /// Bound pointer: index of the output and the position in its space.
    pub fn bound(&self, x: f64, y: f64) -> Option<(usize, Abs)> {
        let i = self.locate(x, y)?;
        let o = &self.outputs[i];
        Some((i, abs_in(x - o.x, y - o.y, o.w, o.h)))
    }
}

/// Aim at the *centre* of the target pixel: send `2x+1` against an extent of
/// `2w`. Hyprland computes `x / w * box_w` in floating point, and with integer
/// pixels `39 / 1080 * 1080` comes out as 38.999…, which it truncates to 38,
/// so some pixels (y = 39, 78, 156 … on a 1080 output) were unreachable.
fn abs_in(x: f64, y: f64, w: f64, h: f64) -> Abs {
    let w = (w.round() as u32).max(1);
    let h = (h.round() as u32).max(1);
    let px = x.floor().clamp(0.0, f64::from(w - 1)) as u32;
    let py = y.floor().clamp(0.0, f64::from(h - 1)) as u32;
    Abs {
        x: 2 * px + 1,
        y: 2 * py + 1,
        w: 2 * w,
        h: 2 * h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two() -> Layout {
        let o = |name: &str, x, w, h| OutputRect {
            name: name.into(),
            x,
            y: 0.0,
            w,
            h,
        };
        Layout {
            outputs: vec![o("A", 0.0, 1280.0, 800.0), o("B", 1280.0, 1024.0, 768.0)],
        }
    }

    /// The pixel Hyprland lands on: `floor(x / w * box)`.
    fn land(a: Abs, bw: f64, bh: f64) -> (f64, f64) {
        (
            (f64::from(a.x) / f64::from(a.w) * bw).floor(),
            (f64::from(a.y) / f64::from(a.h) * bh).floor(),
        )
    }

    #[test]
    fn layout_from_hyprland_monitors() {
        let monitors: Vec<ferry_hypr_ipc::Monitor> = serde_json_monitors(
            r#"[
              {"id":0,"name":"DP-1","width":2560,"height":1440,"x":0,"y":0,"scale":1.25,"transform":0},
              {"id":1,"name":"HDMI-A-1","width":1080,"height":1920,"x":2048,"y":-100,"scale":1.0,"transform":1},
              {"id":2,"name":"OFF","width":800,"height":600,"x":9999,"y":0,"scale":1.0,"disabled":true}
            ]"#,
        );
        let l = Layout::from_monitors(&monitors);
        assert_eq!(
            l.outputs.len(),
            2,
            "a disabled monitor is not in the layout"
        );
        assert_eq!(
            (l.outputs[0].x, l.outputs[0].w, l.outputs[0].h),
            (0.0, 2048.0, 1152.0)
        );
        // Transform 1 swaps width and height before the scale divides.
        assert_eq!(
            (
                l.outputs[1].x,
                l.outputs[1].y,
                l.outputs[1].w,
                l.outputs[1].h
            ),
            (2048.0, -100.0, 1920.0, 1080.0)
        );
        assert_eq!(l.bbox(), (0.0, -100.0, 3968.0, 1252.0));
    }

    fn serde_json_monitors(json: &str) -> Vec<ferry_hypr_ipc::Monitor> {
        // `Monitor` is `Deserialize`; parse through the crate that owns the type.
        ferry_hypr_ipc::monitors_from_json(json).unwrap()
    }

    #[test]
    fn bbox_spans_both_outputs() {
        assert_eq!(two().bbox(), (0.0, 0.0, 2304.0, 800.0));
    }

    #[test]
    fn unbound_reaches_every_pixel() {
        let l = two();
        for x in 0..2304 {
            assert_eq!(
                land(l.unbound(f64::from(x), 39.0), 2304.0, 800.0),
                (f64::from(x), 39.0)
            );
        }
        for y in 0..800 {
            assert_eq!(
                land(l.unbound(7.0, f64::from(y)), 2304.0, 800.0),
                (7.0, f64::from(y))
            );
        }
        assert_eq!(
            land(l.unbound(9999.0, 9999.0), 2304.0, 800.0),
            (2303.0, 799.0)
        );
        assert_eq!(land(l.unbound(-5.0, -5.0), 2304.0, 800.0), (0.0, 0.0));
    }

    #[test]
    fn bound_picks_by_containment() {
        let l = two();
        let (i, a) = l.bound(1279.0, 0.0).unwrap();
        assert_eq!((i, land(a, 1280.0, 800.0)), (0, (1279.0, 0.0)));
        let (i, a) = l.bound(1280.0, 0.0).unwrap();
        assert_eq!((i, land(a, 1024.0, 768.0)), (1, (0.0, 0.0)));
        // The gap under B (y 768..800) snaps to the nearest output.
        let (i, a) = l.bound(2000.0, 790.0).unwrap();
        assert_eq!((i, land(a, 1024.0, 768.0)), (1, (720.0, 767.0)));
    }
}
