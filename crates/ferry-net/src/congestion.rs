//! quinn's CUBIC with a floor under the congestion window.
//!
//! quinn 0.11 sends nothing but its own loss probes once the bytes in flight fill the window
//! (quinn-proto 0.11.19 `connection/mod.rs` L604-L620: "Tail loss probes must not be blocked by
//! congestion"), and it counts a packet as in flight until an acknowledgement shows it lost. In
//! a Wi-Fi stall the motion and Pings already sent stay in flight, so the window fills, the
//! application's own Pings stop leaving, and the only packets left are the probes, whose timeout
//! doubles at each one (`pto_time_and_space`, L1831-L1833). After the link is back the key or
//! `Enter` waits for the next probe: seconds after a long stall. Persistent congestion makes it
//! worse, as CUBIC drops to two packets (`congestion/cubic.rs` L201-L213), and an
//! application-limited sender never grows the window back (L102-L109): ferry is always
//! application-limited.
//!
//! The floor keeps the window at least [`crate::EndpointConfig::min_window`], so a small Ping
//! leaves during a stall, and the first one after the link is back is acknowledged at once: quinn
//! then declares the stall's packets lost, resends their stream data and resets the probe
//! backoff. ferry's own traffic is input at human rates; a bulk transfer still gets CUBIC above
//! the floor.

use std::any::Any;
use std::sync::Arc;

use quinn_proto::RttEstimator;
use quinn_proto::congestion::{Controller, ControllerFactory, CubicConfig};
use std::time::Instant;

/// Builds [`Floored`] controllers.
#[derive(Debug)]
pub(crate) struct FlooredConfig {
    pub(crate) floor: u64,
    pub(crate) cubic: Arc<CubicConfig>,
}

impl FlooredConfig {
    pub(crate) fn new(floor: u64) -> Self {
        Self {
            floor,
            cubic: Arc::new(CubicConfig::default()),
        }
    }
}

impl ControllerFactory for FlooredConfig {
    fn build(self: Arc<Self>, now: Instant, current_mtu: u16) -> Box<dyn Controller> {
        Box::new(Floored {
            floor: self.floor,
            inner: self.cubic.clone().build(now, current_mtu),
        })
    }
}

/// CUBIC, with `window()` never below `floor`.
pub(crate) struct Floored {
    floor: u64,
    inner: Box<dyn Controller>,
}

impl Controller for Floored {
    fn on_sent(&mut self, now: Instant, bytes: u64, last_packet_number: u64) {
        self.inner.on_sent(now, bytes, last_packet_number);
    }

    fn on_ack(
        &mut self,
        now: Instant,
        sent: Instant,
        bytes: u64,
        app_limited: bool,
        rtt: &RttEstimator,
    ) {
        self.inner.on_ack(now, sent, bytes, app_limited, rtt);
    }

    fn on_end_acks(
        &mut self,
        now: Instant,
        in_flight: u64,
        app_limited: bool,
        largest_packet_num_acked: Option<u64>,
    ) {
        self.inner
            .on_end_acks(now, in_flight, app_limited, largest_packet_num_acked);
    }

    fn on_congestion_event(
        &mut self,
        now: Instant,
        sent: Instant,
        is_persistent_congestion: bool,
        lost_bytes: u64,
    ) {
        self.inner
            .on_congestion_event(now, sent, is_persistent_congestion, lost_bytes);
    }

    fn on_mtu_update(&mut self, new_mtu: u16) {
        self.inner.on_mtu_update(new_mtu);
    }

    fn window(&self) -> u64 {
        self.inner.window().max(self.floor)
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(Self {
            floor: self.floor,
            inner: self.inner.clone_box(),
        })
    }

    fn initial_window(&self) -> u64 {
        self.inner.initial_window().max(self.floor)
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MTU: u16 = 1200;

    fn build(floor: u64) -> (Box<dyn Controller>, Box<dyn Controller>) {
        let now = Instant::now();
        let cubic = Arc::new(CubicConfig::default()).build(now, MTU);
        let floored = Arc::new(FlooredConfig::new(floor)).build(now, MTU);
        (cubic, floored)
    }

    #[test]
    fn persistent_congestion_drops_cubic_to_two_packets_but_not_below_the_floor() {
        let (mut cubic, mut floored) = build(64 * 1024);
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_millis(1500);
        cubic.on_congestion_event(t1, t0, true, 30_000);
        floored.on_congestion_event(t1, t0, true, 30_000);
        assert_eq!(cubic.window(), 2 * u64::from(MTU));
        assert_eq!(floored.window(), 64 * 1024);
    }

    #[test]
    fn above_the_floor_it_is_cubic() {
        let (cubic, floored) = build(1000);
        assert_eq!(floored.window(), cubic.window());
        assert_eq!(floored.initial_window(), cubic.initial_window());
    }

    #[test]
    fn a_zero_floor_is_cubic_after_loss_too() {
        let (mut cubic, mut floored) = build(0);
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_millis(10);
        cubic.on_congestion_event(t1, t0, false, 1200);
        floored.on_congestion_event(t1, t0, false, 1200);
        assert_eq!(floored.window(), cubic.window());
        // A loss sent after the recovery began is a new episode; persistent, it drops to 2 MTU.
        let t2 = t1 + std::time::Duration::from_millis(10);
        let mut c = floored.clone_box();
        c.on_congestion_event(t2 + std::time::Duration::from_millis(10), t2, true, 1200);
        assert_eq!(c.window(), 2 * u64::from(MTU));
        assert_eq!(
            floored.window(),
            cubic.window(),
            "clone_box copies, not shares"
        );
    }
}
