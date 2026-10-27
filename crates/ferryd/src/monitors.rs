//! This peer's own monitors, read from Hyprland IPC (read-only) and re-read 100 ms after the
//! last socket2 `monitoradded`, `monitorremoved` or `configreloaded`.

use std::path::PathBuf;
use std::time::Duration;

use ferry_core::geom::{MonitorInfo, WireRect};
use ferry_hypr_ipc as hypr;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::chan::CtlMsg;
use crate::ratelimit::log_limited;

/// How long the monitor layout must stay quiet before it is read again.
pub const DEBOUNCE: Duration = Duration::from_millis(100);
const RECONNECT: Duration = Duration::from_secs(2);

/// Where the own monitors come from.
#[derive(Debug, Clone, Default)]
pub enum MonitorSource {
    /// Hyprland: `HYPRLAND_INSTANCE_SIGNATURE`, else the newest live instance.
    #[default]
    Hyprland,
    /// Hyprland instances under this runtime dir only; reads nothing from the environment.
    HyprlandIn(PathBuf),
    /// A fixed list (tests, and a peer without a compositor).
    Fixed(Vec<MonitorInfo>),
}

impl MonitorSource {
    /// The Hyprland instance this source reads (none for `Fixed`).
    pub fn instance(&self) -> hypr::Result<hypr::Instance> {
        match self {
            Self::Hyprland => hypr::Instance::discover(None),
            Self::HyprlandIn(dir) => hypr::Instance::discover_in(dir, None),
            Self::Fixed(_) => Err(hypr::Error::NoRuntimeDir),
        }
    }

    /// One read. A missing compositor is an error the caller logs, not a crash.
    pub fn read(&self) -> hypr::Result<Vec<MonitorInfo>> {
        match self {
            Self::Fixed(m) => Ok(m.clone()),
            _ => Ok(from_hypr(&self.instance()?.monitors()?)),
        }
    }
}

/// Hyprland's monitors as ferry's wire monitors: enabled ones, local logical rectangles.
pub fn from_hypr(monitors: &[hypr::Monitor]) -> Vec<MonitorInfo> {
    monitors
        .iter()
        .filter(|m| !m.disabled)
        // A name that fails the check would make the peers drop our Hello, and a terminal
        // would not be safe showing it: the monitor is left out and the log says so.
        .filter(|m| match ferry_core::geom::validate_monitor_text(&m.name) {
            Ok(()) => true,
            Err(why) => {
                log_limited!(
                    warn,
                    why,
                    "a Hyprland monitor name is not allowed: left out"
                );
                false
            }
        })
        .map(|m| {
            let (x, y, w, h) = m.logical_rect();
            MonitorInfo {
                name: m.name.clone(),
                description: clean_description(&m.description),
                rect: WireRect {
                    x: x as i32,
                    y: y as i32,
                    w: w.max(1.0) as u32,
                    h: h.max(1.0) as u32,
                },
                scale: m.scale,
                transform: u8::try_from(m.transform).unwrap_or(0).min(7),
                // The anchor shift is worked out from the reading, by the layout store.
                shift: None,
            }
        })
        .collect()
}

/// A description is only shown, never matched: control characters become spaces and the text is cut
/// to the wire cap, so a peer never drops our `Hello` over it.
fn clean_description(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(ferry_core::wire::caps::MONITOR_TEXT));
    for c in text.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > ferry_core::wire::caps::MONITOR_TEXT {
            break;
        }
        out.push(c);
    }
    out
}

/// Why one pass of [`watch`] ended; logged at `debug`, then retried.
#[derive(Debug, thiserror::Error)]
enum MonitorWatchError {
    #[error("Hyprland IPC: {0}")]
    Hypr(#[from] hypr::Error),
    #[error("Hyprland event socket: {0}")]
    Io(#[from] std::io::Error),
    #[error("Hyprland closed the event socket")]
    Closed,
    #[error("the monitor read did not finish: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// Watches socket2 and sends the monitors to `ctl` whenever they change. Runs until the task
/// is dropped; a vanished compositor is retried every 2 s.
pub async fn watch(source: MonitorSource, tx: mpsc::Sender<CtlMsg>) {
    if matches!(source, MonitorSource::Fixed(_)) {
        return;
    }
    let mut last: Option<Vec<MonitorInfo>> = None;
    loop {
        match watch_once(&source, &tx, &mut last).await {
            Ok(()) => return,
            Err(e) => tracing::debug!(error = %e, "Hyprland monitor watch ended; retrying"),
        }
        tokio::time::sleep(RECONNECT).await;
    }
}

async fn watch_once(
    source: &MonitorSource,
    tx: &mpsc::Sender<CtlMsg>,
    last: &mut Option<Vec<MonitorInfo>>,
) -> Result<(), MonitorWatchError> {
    let instance = source.instance()?;
    // Subscribe before reading, so an event that lands in between is not lost. The event
    // socket is a plain stream of `name>>data` lines (ferry-hypr-ipc parses each one).
    let stream = UnixStream::connect(instance.events_socket_path()).await?;
    let mut lines = BufReader::new(stream).lines();
    let mut deadline = Some(Instant::now());
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else {
                    return Err(MonitorWatchError::Closed);
                };
                let ev = hypr::Event::parse(&line);
                if ev.affects_monitor_layout() {
                    deadline = Some(Instant::now() + DEBOUNCE);
                }
                // `input:repeat_delay` may have changed.
                if ev.name == "configreloaded" && tx.send(CtlMsg::HyprConfigReloaded).await.is_err() {
                    return Ok(());
                }
            }
            () = sleep_until(deadline), if deadline.is_some() => {
                deadline = None;
                let inst = instance.clone();
                let read = tokio::task::spawn_blocking(move || inst.monitors()).await?;
                let mons = from_hypr(&read?);
                if last.as_ref() != Some(&mons) {
                    *last = Some(mons.clone());
                    tracing::info!(count = mons.len(), "own monitors changed");
                    if tx.send(CtlMsg::OwnMonitors(mons)).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(
        name: &str,
        w: u32,
        h: u32,
        x: i32,
        scale: f32,
        transform: i32,
        disabled: bool,
    ) -> hypr::Monitor {
        hypr::monitors_from_json(&format!(
            r#"[{{"id":0,"name":"{name}","description":"d {name}","width":{w},"height":{h},"x":{x},"y":0,"scale":{scale},"transform":{transform},"disabled":{disabled}}}]"#
        ))
        .unwrap()
        .remove(0)
    }

    #[test]
    fn a_monitor_name_with_a_control_character_is_left_out() {
        // JSON \u001b is ESC; the check is where the name enters
        let bad = hypr::monitors_from_json(
            r#"[{"id":1,"name":"DP-1\u001b[2K","description":"x\ry","width":10,"height":10}]"#,
        )
        .unwrap()
        .remove(0);
        let got = from_hypr(&[bad, m("DP-2", 1920, 1080, 0, 1.0, 0, false)]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "DP-2");
        assert!(got.iter().all(|x| x.text_is_clean()));
        assert_eq!(clean_description("a\r\x1b[2Kb"), "a  [2Kb");
        assert_eq!(clean_description(&"é".repeat(100)).len(), 128);
    }

    #[test]
    fn converts_logical_rects_scale_and_transform() {
        let got = from_hypr(&[
            m("HDMI-A-1", 1920, 1080, 0, 1.0, 0, false),
            m("DVI-D-1", 1920, 1080, 1920, 1.0, 0, false),
            m("HiDPI", 2400, 1350, 3840, 2.0, 0, false),
            m("Rot", 1080, 1920, 0, 1.0, 1, false),
            m("Off", 1920, 1080, 0, 1.0, 0, true),
        ]);
        let names: Vec<_> = got.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["HDMI-A-1", "DVI-D-1", "HiDPI", "Rot"]);
        assert_eq!(
            got[1].rect,
            WireRect {
                x: 1920,
                y: 0,
                w: 1920,
                h: 1080
            }
        );
        assert_eq!(
            got[2].rect,
            WireRect {
                x: 3840,
                y: 0,
                w: 1200,
                h: 675
            }
        );
        assert_eq!(got[2].scale, 2.0);
        // transform 1 swaps width and height
        assert_eq!(
            got[3].rect,
            WireRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080
            }
        );
        assert_eq!(got[3].transform, 1);
        assert_eq!(got[0].description, "d HDMI-A-1");
    }

    #[test]
    fn a_fixed_source_reads_back() {
        let one = vec![MonitorInfo {
            name: "X".into(),
            description: String::new(),
            rect: WireRect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            },
            scale: 1.0,
            transform: 0,
            shift: None,
        }];
        assert_eq!(MonitorSource::Fixed(one.clone()).read().unwrap(), one);
    }

    #[test]
    fn no_compositor_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            MonitorSource::HyprlandIn(dir.path().to_path_buf())
                .read()
                .is_err()
        );
    }
}
