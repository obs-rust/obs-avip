//! Resume after a network cut, independent of how long the cut was, without
//! re-connecting to a sender that is reachable but sends nothing.
//!
//! The NDI SDK reconnects by itself, but with a back-off that grows with the
//! length of the cut (measured 2026-11-06 with a plain SDK receiver: 1.7 s after
//! a 5 s cut, 18.8 s after 10 s, 77.6 s after 45 s). Disconnecting and
//! connecting the receiver again brings the picture back 0.7 s after the
//! network is back, for any length.
//!
//! What cannot tell a cut (measured): `NDIlib_recv_get_no_connections` stays 1
//! during a cut (2026-11-06), and the finder keeps the source in its list
//! through a 45 s cut (2026-11-10). What can: whether the sender's NDI port
//! accepts a TCP connection. A plain TCP connect is not an NDI connection; it
//! did not count against the per-process connection limit of an encoder that
//! stops sending video after about 15 NDI connections (2026-11-10).
//!
//! Rule: when no frame came for `stall_after`, look up the sender's address in
//! the receiver's finder (made with the receiver, so it has the address before
//! a cut) and probe its port every `probe_every`:
//! - not reachable, then reachable again (a network cut): re-connect once;
//! - reachable the whole time (an encoder that restarts, lost its input or
//!   changes format): no re-connect, the SDK's connection gets the frames when
//!   they come back; after `warn_after`, one warning; after `safety_every`
//!   without a frame, one re-connect in case the connection itself is stuck;
//! - address unknown (not in the finder's list): re-connect with a back-off
//!   (2, 4, 8, 16, 30, 30 ... s).
//!
//! Any frame counts (video or audio): a cut stops both, and an audio-only
//! sender must not be re-connected forever. The same holds before the first
//! frame (the sender was offline at start).

use std::ffi::CString;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::ndi::{frame_type, Api, AudioFrame, Handle, RecvCreate, Source, VideoFrame};
use crate::tally::Tally;

pub const NS_PER_MS: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub enabled: bool,
    pub stall_after_ns: u64,
    pub probe_every_ns: u64,
    pub warn_after_ns: u64,
    pub safety_every_ns: u64,
    pub backoff_max_ns: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            stall_after_ns: 1_500 * NS_PER_MS,
            probe_every_ns: 2_000 * NS_PER_MS,
            warn_after_ns: 10_000 * NS_PER_MS,
            safety_every_ns: 30_000 * NS_PER_MS,
            backoff_max_ns: 30_000 * NS_PER_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No frame yet since the receiver was created.
    Connecting,
    Live,
    /// Frames stopped.
    Stalled,
}

/// What the receive loop does on this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Wait,
    /// Probe the sender now and report the result with [`Resume::probed`].
    Probe,
}

/// The result of one probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Reachable,
    Unreachable,
    /// The finder does not list the source (yet).
    NoAddress,
}

/// Why a re-connect happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The sender was not reachable and is reachable again.
    NetworkBack,
    /// Reachable, but no frame for `safety_every`.
    Safety,
    /// The address is unknown: back-off.
    NoAddress,
}

/// A state change, for one log line each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The first frame, `after_ns` after the receiver was created.
    Connected {
        after_ns: u64,
        reconnects: u32,
    },
    /// No frame for `stall_after`.
    Stalled,
    Reconnecting(Reason),
    /// The sender is reachable but sent no frame for `after_ns` (once per stall).
    ReachableNoFrames {
        after_ns: u64,
    },
    /// Frames again, `after_ns` after the last frame before the cut.
    Resumed {
        after_ns: u64,
        reconnects: u32,
    },
}

#[derive(Debug, Clone)]
pub struct Resume {
    cfg: Config,
    state: State,
    /// When the receiver was created, or the time of the last frame.
    since: u64,
    /// When the stall was found (also before the first frame).
    stalled_at: Option<u64>,
    next_probe: u64,
    last_reconnect: Option<u64>,
    backoff_ns: u64,
    /// A probe found the sender unreachable during this stall.
    seen_down: bool,
    warned: bool,
    reconnects: u32,
    /// The sender's frame interval (smoothed), to stretch the stall time for
    /// slow senders (an encoder without input sends a still frame about
    /// every 1.5 s).
    interval_ns: u64,
}

impl Resume {
    pub fn new(cfg: Config, now: u64) -> Resume {
        Resume {
            cfg,
            state: State::Connecting,
            since: now,
            stalled_at: None,
            next_probe: 0,
            last_reconnect: None,
            backoff_ns: cfg.probe_every_ns,
            seen_down: false,
            warned: false,
            reconnects: 0,
            interval_ns: 0,
        }
    }

    /// How long without a frame counts as a stall: `stall_after`, or 2.5
    /// frame intervals of a slower sender.
    pub fn stall_after_ns(&self) -> u64 {
        self.cfg.stall_after_ns.max(self.interval_ns * 5 / 2)
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// A video or audio frame arrived.
    pub fn on_frame(&mut self, now: u64) -> Option<Change> {
        let after_ns = now.saturating_sub(self.since);
        // A frame interval: between frames while live, or a stall that ended
        // within 10 s without a re-connect and with the sender reachable
        // (a slow sender, not a cut).
        let slow_frame = self.state == State::Stalled
            && self.reconnects == 0
            && !self.seen_down
            && after_ns < 10_000 * NS_PER_MS;
        if self.state == State::Live || slow_frame {
            self.interval_ns = if self.interval_ns == 0 {
                after_ns
            } else {
                (self.interval_ns * 3 + after_ns) / 4
            };
        }
        let reconnects = self.reconnects;
        let change = match self.state {
            State::Connecting => Some(Change::Connected {
                after_ns,
                reconnects,
            }),
            State::Stalled => Some(Change::Resumed {
                after_ns,
                reconnects,
            }),
            State::Live => None,
        };
        self.state = State::Live;
        self.since = now;
        self.stalled_at = None;
        self.last_reconnect = None;
        self.backoff_ns = self.cfg.probe_every_ns;
        self.seen_down = false;
        self.warned = false;
        self.reconnects = 0;
        change
    }

    /// The source is hidden and the receiver takes no frames ("keep
    /// active"): that time is not a stall. A stall already found stays.
    pub fn hold(&mut self, now: u64) {
        if self.state != State::Stalled {
            self.since = now;
            self.stalled_at = None;
        }
    }

    /// Called on every pass of the receive loop.
    pub fn tick(&mut self, now: u64) -> (Action, Option<Change>) {
        if !self.cfg.enabled || now.saturating_sub(self.since) < self.stall_after_ns() {
            return (Action::Wait, None);
        }
        if self.stalled_at.is_none() {
            self.stalled_at = Some(now);
            self.next_probe = now + self.cfg.probe_every_ns;
            let change = (self.state == State::Live).then_some(Change::Stalled);
            if self.state == State::Live {
                self.state = State::Stalled;
            }
            return (Action::Probe, change);
        }
        if now >= self.next_probe {
            self.next_probe = now + self.cfg.probe_every_ns;
            return (Action::Probe, None);
        }
        (Action::Wait, None)
    }

    /// The result of the probe [`tick`](Self::tick) asked for. `true`:
    /// re-connect now.
    pub fn probed(&mut self, now: u64, probe: Probe) -> (bool, Option<Change>) {
        let stalled_at = self.stalled_at.unwrap_or(now);
        match probe {
            Probe::Unreachable => {
                self.seen_down = true;
                (false, None)
            }
            Probe::NoAddress => match self.last_reconnect {
                Some(last) if now.saturating_sub(last) < self.backoff_ns => (false, None),
                Some(_) => {
                    self.backoff_ns = (self.backoff_ns * 2).min(self.cfg.backoff_max_ns);
                    self.reconnect(now, Reason::NoAddress)
                }
                None => self.reconnect(now, Reason::NoAddress),
            },
            Probe::Reachable => {
                if self.seen_down {
                    self.seen_down = false;
                    return self.reconnect(now, Reason::NetworkBack);
                }
                let quiet_since = self.last_reconnect.unwrap_or(stalled_at);
                if now.saturating_sub(quiet_since) >= self.cfg.safety_every_ns {
                    return self.reconnect(now, Reason::Safety);
                }
                // Counted from the stall or the last re-connect, so a sender
                // that was just re-connected gets its time to send.
                if !self.warned && now.saturating_sub(quiet_since) >= self.cfg.warn_after_ns {
                    self.warned = true;
                    return (
                        false,
                        Some(Change::ReachableNoFrames {
                            after_ns: now.saturating_sub(self.since),
                        }),
                    );
                }
                (false, None)
            }
        }
    }

    fn reconnect(&mut self, now: u64, reason: Reason) -> (bool, Option<Change>) {
        self.reconnects += 1;
        self.last_reconnect = Some(now);
        (true, Some(Change::Reconnecting(reason)))
    }
}

/// Whether a sender's address accepts a connection.
pub trait Prober {
    fn reachable(&self, address: &str) -> bool;
}

/// A plain TCP connect to the sender's NDI port (`host:port` from the finder).
#[derive(Debug, Clone, Copy)]
pub struct TcpProber {
    pub timeout: Duration,
}

impl Default for TcpProber {
    fn default() -> Self {
        TcpProber {
            timeout: Duration::from_millis(300),
        }
    }
}

impl Prober for TcpProber {
    fn reachable(&self, address: &str) -> bool {
        let Ok(addrs) = address.to_socket_addrs() else {
            return false;
        };
        addrs
            .into_iter()
            .any(|a| TcpStream::connect_timeout(&a, self.timeout).is_ok())
    }
}

/// An NDI receiver with resume. The shell gives it the time and hands the
/// frames to OBS; frames are freed after the callback returns.
pub struct Receiver<'a> {
    api: &'a dyn Api,
    recv: Handle,
    name: CString,
    resume: Resume,
    prober: Box<dyn Prober + 'a>,
    /// Made with the receiver (resume on), so it knows the sender's
    /// address before a cut: a finder made during a cut finds nothing.
    finder: Option<Handle>,
    /// The last tally sent; a new receiver starts with both lights off.
    tally: Tally,
    /// When the web control URL may be asked for next (ns).
    web_poll_at: u64,
}

/// How often the sender's web control URL is asked for (DistroAV: 1 s).
pub const WEB_CONTROL_POLL_NS: u64 = 1_000_000_000;

/// How long a new finder waits for its first list.
const FINDER_FIRST_WAIT_MS: u32 = 500;

impl<'a> Receiver<'a> {
    /// `None` when the runtime refuses to create the receiver.
    ///
    /// # Safety
    /// `desc` points to valid strings for the duration of the call.
    pub unsafe fn create(
        api: &'a dyn Api,
        desc: &RecvCreate,
        cfg: Config,
        now: u64,
    ) -> Option<Receiver<'a>> {
        let recv = api.recv_create(desc);
        if recv.is_null() {
            return None;
        }
        let name = if desc.source.name.is_null() {
            CString::default()
        } else {
            std::ffi::CStr::from_ptr(desc.source.name).to_owned()
        };
        let finder = if cfg.enabled {
            let f = api.find_create(true);
            (!f.is_null()).then_some(f)
        } else {
            None
        };
        Some(Receiver {
            api,
            recv,
            name,
            resume: Resume::new(cfg, now),
            prober: Box::new(TcpProber::default()),
            finder,
            tally: Tally::default(),
            web_poll_at: now,
        })
    }

    /// Replace the TCP probe (tests).
    pub fn set_prober(&mut self, prober: Box<dyn Prober + 'a>) {
        self.prober = prober;
    }

    pub fn state(&self) -> State {
        self.resume.state()
    }

    /// Send the tally when it changed. `true` when it was sent.
    pub fn set_tally(&mut self, t: Tally) -> bool {
        if t == self.tally {
            return false;
        }
        // SAFETY: `recv` is this receiver's.
        unsafe { self.api.recv_set_tally(self.recv, t.program, t.preview) };
        self.tally = t;
        true
    }

    /// The sender's web control URL when it differs from `current` (the
    /// source's saved `web_control_url`): `Some("")` when it has none any
    /// more. Asks the runtime at most once per [`WEB_CONTROL_POLL_NS`], since
    /// a sender can publish or change it at any time after connecting.
    pub fn web_control_change(&mut self, now: u64, current: &str) -> Option<String> {
        if now < self.web_poll_at {
            return None;
        }
        self.web_poll_at = now + WEB_CONTROL_POLL_NS;
        // SAFETY: `recv` is this receiver's.
        let url = unsafe { self.api.recv_web_control(self.recv) }.unwrap_or_default();
        (url != current).then_some(url)
    }

    /// See [`Resume::hold`].
    pub fn hold(&mut self, now: u64) {
        self.resume.hold(now);
    }

    /// Look the sender up in the finder and probe its address.
    fn probe(&mut self) -> Probe {
        // SAFETY: the finder is made, used and destroyed by this receiver.
        unsafe {
            if self.finder.is_none() {
                let f = self.api.find_create(true);
                if !f.is_null() {
                    self.api.find_wait(f, FINDER_FIRST_WAIT_MS);
                    self.finder = Some(f);
                }
            }
            let name = self.name.to_string_lossy();
            match self.finder.and_then(|f| self.api.find_source_url(f, &name)) {
                None => Probe::NoAddress,
                Some(url) if self.prober.reachable(&url) => Probe::Reachable,
                Some(_) => Probe::Unreachable,
            }
        }
    }

    fn reconnect(&mut self) {
        let source = Source {
            name: self.name.as_ptr(),
            url: std::ptr::null(),
        };
        // SAFETY: `source` lives across both calls; NULL disconnects.
        unsafe {
            self.api.recv_connect(self.recv, None);
            self.api.recv_connect(self.recv, Some(&source));
            // The sender may treat the re-connected receiver as new.
            if self.tally != Tally::default() {
                self.api
                    .recv_set_tally(self.recv, self.tally.program, self.tally.preview);
            }
        }
    }

    /// Wait up to `timeout_ms` for one frame, pass it on, and probe or
    /// re-connect when the resume rule says so. Returns the state changes.
    pub fn poll(
        &mut self,
        timeout_ms: u32,
        now: &mut dyn FnMut() -> u64,
        on_video: &mut dyn FnMut(&VideoFrame),
        on_audio: &mut dyn FnMut(&AudioFrame),
    ) -> Vec<Change> {
        let mut video = VideoFrame::default();
        let mut audio = AudioFrame::default();
        // SAFETY: `recv` is this receiver's; each captured frame is freed once.
        let kind = unsafe {
            self.api
                .recv_capture(self.recv, &mut video, &mut audio, timeout_ms)
        };
        let now = now();
        let mut changes = Vec::new();
        match kind {
            frame_type::VIDEO => {
                on_video(&video);
                unsafe { self.api.free_video(self.recv, &video) };
                changes.extend(self.resume.on_frame(now));
            }
            frame_type::AUDIO => {
                on_audio(&audio);
                unsafe { self.api.free_audio(self.recv, &audio) };
                changes.extend(self.resume.on_frame(now));
            }
            _ => {}
        }
        let (action, change) = self.resume.tick(now);
        changes.extend(change);
        if action == Action::Probe {
            let probe = self.probe();
            let (reconnect, change) = self.resume.probed(now, probe);
            changes.extend(change);
            if reconnect {
                self.reconnect();
            }
        }
        changes
    }
}

impl Drop for Receiver<'_> {
    fn drop(&mut self) {
        // SAFETY: created by this api, destroyed once.
        unsafe {
            if let Some(f) = self.finder.take() {
                self.api.find_destroy(f);
            }
            self.api.recv_destroy(self.recv)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ndi::{fourcc, mock};
    use std::cell::RefCell;
    use std::collections::VecDeque;

    const MS: u64 = NS_PER_MS;

    fn resume(at: u64) -> Resume {
        Resume::new(Config::default(), at)
    }

    /// Probes every 2 s from `from` (ms) to `to`, all with the same result.
    /// Returns the re-connect times (ms) and the changes.
    fn run(r: &mut Resume, from: u64, to: u64, probe: Probe) -> (Vec<u64>, Vec<Change>) {
        let (mut at, mut changes) = (Vec::new(), Vec::new());
        let mut t = from;
        while t <= to {
            let (action, c) = r.tick(t * MS);
            changes.extend(c);
            if action == Action::Probe {
                let (re, c) = r.probed(t * MS, probe);
                changes.extend(c);
                if re {
                    at.push(t);
                }
            }
            t += 100;
        }
        (at, changes)
    }

    #[test]
    fn the_first_frame_connects() {
        let mut r = resume(0);
        assert_eq!(r.state(), State::Connecting);
        assert_eq!(r.tick(100 * MS), (Action::Wait, None));
        assert_eq!(
            r.on_frame(700 * MS),
            Some(Change::Connected {
                after_ns: 700 * MS,
                reconnects: 0
            })
        );
        assert_eq!(r.state(), State::Live);
        assert_eq!(r.on_frame(740 * MS), None);
    }

    /// A network cut: unreachable while it lasts, one re-connect when the
    /// sender answers again, however long the cut was.
    #[test]
    fn a_network_cut_reconnects_once_when_the_sender_is_back() {
        let mut r = resume(0);
        r.on_frame(10 * MS);
        assert_eq!(r.tick(1_500 * MS), (Action::Wait, None));
        assert_eq!(r.tick(1_510 * MS), (Action::Probe, Some(Change::Stalled)));
        assert_eq!(r.state(), State::Stalled);
        assert_eq!(r.probed(1_510 * MS, Probe::Unreachable), (false, None));
        let (at, _) = run(&mut r, 1_600, 45_000, Probe::Unreachable);
        assert!(at.is_empty(), "no re-connect while the sender is gone");
        let (at, changes) = run(&mut r, 45_100, 47_600, Probe::Reachable);
        assert_eq!(at.len(), 1);
        assert_eq!(changes, vec![Change::Reconnecting(Reason::NetworkBack)]);
        assert_eq!(
            r.on_frame(48_000 * MS),
            Some(Change::Resumed {
                after_ns: 47_990 * MS,
                reconnects: 1
            })
        );
        assert_eq!(r.tick(48_100 * MS), (Action::Wait, None));
    }

    /// An encoder that restarts, lost its input or changes format: the sender
    /// stays reachable, so no re-connect for 30 s, one warning after 10 s.
    #[test]
    fn a_reachable_sender_is_left_alone() {
        let mut r = resume(0);
        r.on_frame(0);
        let (at, changes) = run(&mut r, 100, 61_600, Probe::Reachable);
        // Stalled at 1.5 s; safety re-connects 30 s apart.
        assert_eq!(at, vec![31_500, 61_500]);
        assert_eq!(
            changes,
            vec![
                Change::Stalled,
                Change::ReachableNoFrames {
                    after_ns: 11_500 * MS
                },
                Change::Reconnecting(Reason::Safety),
                Change::Reconnecting(Reason::Safety),
            ]
        );
    }

    /// An encoder without input sends a still frame about every 1.53 s: after a
    /// stall or two the stall time follows its frame interval, so it does not
    /// flap (and is never re-connected).
    #[test]
    fn a_slow_sender_stops_flapping() {
        let mut r = resume(0);
        r.on_frame(0);
        let mut stalls = Vec::new();
        let mut reconnects = 0;
        let mut next_frame = 1_530;
        for t in (10..60_000).step_by(10) {
            if t == next_frame {
                r.on_frame(t * MS);
                next_frame += 1_530;
            }
            let (action, c) = r.tick(t * MS);
            if c == Some(Change::Stalled) {
                stalls.push(t);
            }
            if action == Action::Probe && r.probed(t * MS, Probe::Reachable).0 {
                reconnects += 1;
            }
        }
        assert!(stalls.len() <= 2, "flapped: {stalls:?}");
        assert_eq!(reconnects, 0);
        assert!(r.stall_after_ns() > 1_530 * MS);
    }

    /// A 25 fps sender keeps the 1.5 s stall time.
    #[test]
    fn a_normal_sender_keeps_the_default_stall_time() {
        let mut r = resume(0);
        for t in (0..2_000).step_by(40) {
            r.on_frame(t * MS);
        }
        assert_eq!(r.stall_after_ns(), 1_500 * MS);
    }

    #[test]
    fn an_unknown_address_backs_off() {
        let mut r = resume(0);
        r.on_frame(0);
        let (at, _) = run(&mut r, 100, 62_000, Probe::NoAddress);
        assert_eq!(at, vec![1_500, 3_500, 7_500, 15_500, 31_500, 61_500]);
    }

    /// A hidden "keep active" source takes no frames; when it is shown
    /// again, the stall time counts from then, not from the last frame.
    #[test]
    fn time_on_hold_is_not_a_stall() {
        let mut r = resume(0);
        r.on_frame(10 * MS);
        for t in (100..10_000).step_by(100) {
            r.hold(t * MS);
        }
        r.hold(10_000 * MS);
        assert_eq!(r.state(), State::Live);
        assert_eq!(r.tick(10_100 * MS), (Action::Wait, None));
        assert_eq!(r.tick(11_490 * MS), (Action::Wait, None));
        assert_eq!(r.tick(11_510 * MS), (Action::Probe, Some(Change::Stalled)));
        // A stall stays a stall through a hold (the probe timing goes on).
        r.hold(12_000 * MS);
        assert_eq!(r.state(), State::Stalled);
        assert_eq!(r.tick(13_510 * MS), (Action::Probe, None));
    }

    #[test]
    fn hold_before_the_first_frame_keeps_connecting() {
        let mut r = resume(0);
        r.hold(5_000 * MS);
        assert_eq!(r.state(), State::Connecting);
        assert_eq!(r.tick(6_000 * MS), (Action::Wait, None));
        assert_eq!(
            r.on_frame(6_200 * MS),
            Some(Change::Connected {
                after_ns: 1_200 * MS,
                reconnects: 0
            })
        );
    }

    #[test]
    fn a_sender_offline_at_start_is_reconnected_when_it_appears() {
        let mut r = resume(0);
        assert_eq!(r.tick(1_500 * MS), (Action::Probe, None));
        assert_eq!(r.probed(1_500 * MS, Probe::Unreachable), (false, None));
        assert_eq!(r.state(), State::Connecting);
        assert_eq!(r.tick(3_500 * MS), (Action::Probe, None));
        assert_eq!(
            r.probed(3_500 * MS, Probe::Reachable),
            (true, Some(Change::Reconnecting(Reason::NetworkBack)))
        );
        assert_eq!(
            r.on_frame(4_000 * MS),
            Some(Change::Connected {
                after_ns: 4_000 * MS,
                reconnects: 1
            })
        );
    }

    #[test]
    fn a_second_cut_starts_fresh() {
        let mut r = resume(0);
        r.on_frame(0);
        r.tick(1_500 * MS);
        r.probed(1_500 * MS, Probe::Unreachable);
        r.on_frame(2_000 * MS);
        assert_eq!(r.tick(3_500 * MS), (Action::Probe, Some(Change::Stalled)));
        // The first cut's "unreachable" does not carry over.
        assert_eq!(r.probed(3_500 * MS, Probe::Reachable), (false, None));
        r.probed(5_500 * MS, Probe::Unreachable);
        assert!(r.probed(7_500 * MS, Probe::Reachable).0);
        assert_eq!(
            r.on_frame(7_600 * MS),
            Some(Change::Resumed {
                after_ns: 5_600 * MS,
                reconnects: 1
            })
        );
    }

    #[test]
    fn disabled_never_probes_or_reconnects() {
        let mut r = Resume::new(
            Config {
                enabled: false,
                ..Config::default()
            },
            0,
        );
        r.on_frame(0);
        assert_eq!(r.tick(60_000 * MS), (Action::Wait, None));
        assert_eq!(r.state(), State::Live);
    }

    #[test]
    fn the_tcp_probe_reaches_a_listener_and_not_a_closed_port() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        assert!(TcpProber::default().reachable(&addr));
        drop(l);
        assert!(!TcpProber::default().reachable(&addr));
        assert!(!TcpProber::default().reachable("not an address"));
    }

    /// Probe results in order, then "reachable".
    struct Scripted(RefCell<VecDeque<bool>>);
    impl Prober for Scripted {
        fn reachable(&self, _address: &str) -> bool {
            self.0.borrow_mut().pop_front().unwrap_or(true)
        }
    }

    fn desc_for(name: &CString) -> RecvCreate {
        RecvCreate {
            source: Source {
                name: name.as_ptr(),
                url: std::ptr::null(),
            },
            color_format: 1,
            bandwidth: 100,
            allow_video_fields: false,
            recv_name: std::ptr::null(),
        }
    }

    /// A tally goes out once per change, and again after a re-connect, as the
    /// sender may see the re-connected receiver as a new one.
    #[test]
    fn the_receiver_sends_tally_changes_and_repeats_after_a_reconnect() {
        let api = mock::Mock::new(std::iter::repeat_n(mock::Step::Nothing, 20));
        let name = CString::new("CAM (1)").unwrap();
        let program = Tally {
            program: true,
            preview: false,
        };
        let mut t = 0u64;
        {
            let mut rx =
                unsafe { Receiver::create(&api, &desc_for(&name), Config::default(), 0) }.unwrap();
            assert!(!rx.set_tally(Tally::default())); // off, as the new receiver already is
            assert!(rx.set_tally(program));
            assert!(!rx.set_tally(program));
            for _ in 0..16 {
                let mut now = || {
                    t += 100 * MS;
                    t
                };
                rx.poll(100, &mut now, &mut |_| {}, &mut |_| {});
            }
        }
        let tallies: Vec<_> = api
            .calls()
            .into_iter()
            .filter(|c| matches!(c, mock::Call::Tally { .. } | mock::Call::Connect(Some(_))))
            .collect();
        // Sent once; the first re-connect (no address known: at once) sends it again.
        assert_eq!(
            tallies,
            vec![
                mock::Call::Tally {
                    program: true,
                    preview: false
                },
                mock::Call::Connect(Some("CAM (1)".into())),
                mock::Call::Tally {
                    program: true,
                    preview: false
                },
            ]
        );
    }

    /// The receiver against the mock: a scripted cut with fake time, 100 ms
    /// per poll; the sender is unreachable at the first probe, then back.
    #[test]
    fn the_receiver_reconnects_once_through_the_api() {
        let video = mock::Step::Video {
            fourcc: fourcc(b"UYVY"),
            width: 2,
            height: 2,
            timestamp: 1,
            timecode: 1,
        };
        let audio = mock::Step::Audio {
            fourcc: fourcc(b"FLTp"),
            channels: 2,
            samples: 4,
            rate: 48000,
            timestamp: 1,
            timecode: 1,
        };
        // 3 frames, a cut of 40 polls (4 s), then video again.
        let mut script = vec![video.clone(), audio, video.clone()];
        script.extend(std::iter::repeat_n(mock::Step::Nothing, 40));
        script.push(video);
        let api = mock::Mock::new(script);
        api.set_url("CAM (1)", "192.0.2.10:5961");
        let name = CString::new("CAM (1)").unwrap();
        let mut t = 0u64;
        let mut changes = Vec::new();
        let (mut videos, mut audios) = (0, 0);
        {
            let mut rx =
                unsafe { Receiver::create(&api, &desc_for(&name), Config::default(), t) }.unwrap();
            rx.set_prober(Box::new(Scripted(RefCell::new(VecDeque::from([false])))));
            for _ in 0..44 {
                let mut now = || {
                    t += 100 * MS;
                    t
                };
                changes.extend(rx.poll(100, &mut now, &mut |_| videos += 1, &mut |_| audios += 1));
            }
            assert_eq!(rx.state(), State::Live);
        }
        assert_eq!((videos, audios), (3, 1));
        assert_eq!(
            changes,
            vec![
                Change::Connected {
                    after_ns: 100 * MS,
                    reconnects: 0
                },
                Change::Stalled,
                // Last frame at 0.3 s; unreachable at 1.8 s, reachable at 3.8 s; back at 4.4 s.
                Change::Reconnecting(Reason::NetworkBack),
                Change::Resumed {
                    after_ns: 4_100 * MS,
                    reconnects: 1
                },
            ]
        );
        let calls = api.calls();
        let connects: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, mock::Call::Connect(_)))
            .collect();
        // One re-connect: a disconnect, then a connect to the same source.
        assert_eq!(
            connects,
            vec![
                &mock::Call::Connect(None),
                &mock::Call::Connect(Some("CAM (1)".into())),
            ]
        );
        assert_eq!(
            calls
                .iter()
                .filter(|c| **c == mock::Call::FreeVideo)
                .count(),
            3
        );
        assert_eq!(
            calls
                .iter()
                .filter(|c| **c == mock::Call::FreeAudio)
                .count(),
            1
        );
        // The finder made with the receiver is destroyed with it.
        assert_eq!(
            calls
                .iter()
                .filter(|c| **c == mock::Call::FindCreate)
                .count(),
            1
        );
        assert!(calls.contains(&mock::Call::FindDestroy));
        assert_eq!(calls.last(), Some(&mock::Call::Destroy));
        assert_eq!(api.live_receivers(), 0);
    }

    #[test]
    fn web_control_url_is_reported_when_it_changes_at_most_once_a_second() {
        let mock = crate::ndi::mock::Mock::new([]);
        let name = c"cam";
        let desc = RecvCreate {
            source: crate::ndi::Source {
                name: name.as_ptr(),
                url: std::ptr::null(),
            },
            color_format: 1,
            bandwidth: 100,
            allow_video_fields: false,
            recv_name: c"t".as_ptr(),
        };
        let mut rx = unsafe { Receiver::create(&mock, &desc, Config::default(), 0) }.unwrap();
        // No URL yet and none saved: nothing to change.
        assert_eq!(rx.web_control_change(0, ""), None);
        mock.set_web_control(Some("http://10.0.0.5/"));
        // Within the second: not asked again.
        assert_eq!(rx.web_control_change(WEB_CONTROL_POLL_NS / 2, ""), None);
        assert_eq!(
            rx.web_control_change(WEB_CONTROL_POLL_NS, ""),
            Some("http://10.0.0.5/".to_string())
        );
        // Saved now: no change.
        assert_eq!(
            rx.web_control_change(2 * WEB_CONTROL_POLL_NS, "http://10.0.0.5/"),
            None
        );
        // The sender drops it: an empty URL clears the saved one.
        mock.set_web_control(None);
        assert_eq!(
            rx.web_control_change(3 * WEB_CONTROL_POLL_NS, "http://10.0.0.5/"),
            Some(String::new())
        );
    }
}
