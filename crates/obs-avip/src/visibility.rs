//! When the receiver runs and when the picture is cleared: DistroAV's
//! "behavior" and "timeout" settings (`ndi-source.cpp`, `ndi_source_update`,
//! `ndi_source_shown/hidden/activated`, `process_empty_frame`).
//!
//! - Show and activate start the receiver when it is not running.
//! - Hide stops it, except with "keep active"; "stop, resume blank" also
//!   clears the picture.
//! - An update with changed settings restarts the receiver and clears the
//!   picture (DistroAV resets the receiver and clears). It starts again when
//!   the source is active, or with "keep active". DistroAV asks only
//!   `obs_source_active`; we also start when the source is showing (a source
//!   shown only in preview would stay black until it is shown again).
//! - A receiver that runs while the source is hidden ("keep active") takes no
//!   frames: they wait in the SDK, so a sender that sends one frame per
//!   connection (a static test pattern) still shows when the source is shown.
//! - Timeout "clear content": clear the picture after 3 s without video.
//!
//! Connecting while hidden is what broke the first run in OBS (2026-11-07):
//! the one frame of NDI Test Patterns arrived while the source was hidden,
//! and OBS never showed it.

use crate::settings::{Behavior, Timeout};
pub use crate::tally::Event;

/// No video for this long clears the picture with timeout "clear content".
pub const TIMEOUT_NS: u64 = 3_000_000_000;

/// The source's state when an OBS callback runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct State {
    /// A receiver thread is running.
    pub running: bool,
    pub active: bool,
    pub showing: bool,
    /// A source name is set.
    pub has_source: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Action {
    pub start: bool,
    pub stop: bool,
    /// Clear the picture (`obs_source_output_video(NULL)`) once the receiver
    /// has stopped.
    pub clear: bool,
}

/// What to do after `event`. For `Update`, the shell calls this only when the
/// settings changed, after stopping the old receiver.
pub fn action(behavior: Behavior, event: Event, s: State) -> Action {
    let keep = behavior == Behavior::KeepActive;
    match event {
        Event::Update => Action {
            start: s.has_source && (s.active || s.showing || keep),
            stop: false,
            clear: true,
        },
        Event::Show | Event::Activate => Action {
            start: s.has_source && !s.running,
            ..Action::default()
        },
        Event::Hide => {
            let stop = s.running && !keep;
            Action {
                start: false,
                stop,
                clear: stop && behavior == Behavior::StopResumeBlank,
            }
        }
        Event::Deactivate => Action::default(),
    }
}

/// Clear the picture now? `last_video` is the time of the last video frame
/// (`None` before the first); `cleared` is true once cleared since then.
pub fn clear_after_timeout(
    timeout: Timeout,
    last_video: Option<u64>,
    now: u64,
    cleared: bool,
) -> bool {
    timeout == Timeout::ClearContent
        && !cleared
        && last_video.is_some_and(|t| now.saturating_sub(t) > TIMEOUT_NS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Behavior::*;
    use Event::*;

    const ALL: [Behavior; 3] = [KeepActive, StopResumeBlank, StopResumeLastFrame];

    fn st(running: bool, active: bool, showing: bool) -> State {
        State {
            running,
            active,
            showing,
            has_source: true,
        }
    }

    const START: Action = Action {
        start: true,
        stop: false,
        clear: false,
    };
    const NONE: Action = Action {
        start: false,
        stop: false,
        clear: false,
    };

    #[test]
    fn show_and_activate_start_a_stopped_receiver() {
        for b in ALL {
            assert_eq!(action(b, Show, st(false, false, true)), START, "{b:?}");
            assert_eq!(action(b, Activate, st(false, true, true)), START, "{b:?}");
            assert_eq!(
                action(b, Show, st(true, false, true)),
                NONE,
                "{b:?} running"
            );
            assert_eq!(
                action(b, Activate, st(true, true, true)),
                NONE,
                "{b:?} running"
            );
        }
    }

    #[test]
    fn nothing_starts_without_a_source_name() {
        let s = State {
            has_source: false,
            ..st(false, true, true)
        };
        for b in ALL {
            for e in [Update, Show, Activate] {
                assert!(!action(b, e, s).start, "{b:?} {e:?}");
            }
        }
    }

    #[test]
    fn hide_stops_unless_keep_active_and_blank_clears() {
        assert_eq!(action(KeepActive, Hide, st(true, false, false)), NONE);
        assert_eq!(
            action(StopResumeLastFrame, Hide, st(true, false, false)),
            Action { stop: true, ..NONE }
        );
        assert_eq!(
            action(StopResumeBlank, Hide, st(true, false, false)),
            Action {
                stop: true,
                clear: true,
                ..NONE
            }
        );
        // Nothing to stop.
        assert_eq!(action(StopResumeBlank, Hide, st(false, false, false)), NONE);
    }

    #[test]
    fn deactivate_changes_only_the_tally() {
        for b in ALL {
            assert_eq!(action(b, Deactivate, st(true, false, true)), NONE, "{b:?}");
        }
    }

    #[test]
    fn update_clears_and_starts_only_when_visible_or_keep_active() {
        let clear = Action {
            clear: true,
            ..NONE
        };
        let clear_start = Action {
            start: true,
            clear: true,
            ..NONE
        };
        for b in [StopResumeBlank, StopResumeLastFrame] {
            // Hidden: the first-run bug. Connect on show, not now.
            assert_eq!(action(b, Update, st(false, false, false)), clear, "{b:?}");
            assert_eq!(
                action(b, Update, st(false, true, true)),
                clear_start,
                "{b:?}"
            );
            // Shown in preview only (studio mode): DistroAV would not start.
            assert_eq!(
                action(b, Update, st(false, false, true)),
                clear_start,
                "{b:?}"
            );
        }
        assert_eq!(
            action(KeepActive, Update, st(false, false, false)),
            clear_start
        );
    }

    #[test]
    fn timeout_clears_once_after_three_seconds_without_video() {
        let t0 = 10 * TIMEOUT_NS;
        let c = Timeout::ClearContent;
        assert!(!clear_after_timeout(c, None, t0, false), "no frame yet");
        assert!(
            !clear_after_timeout(c, Some(t0), t0 + TIMEOUT_NS, false),
            "exactly 3 s"
        );
        assert!(clear_after_timeout(c, Some(t0), t0 + TIMEOUT_NS + 1, false));
        assert!(
            !clear_after_timeout(c, Some(t0), t0 + 2 * TIMEOUT_NS, true),
            "once"
        );
        assert!(!clear_after_timeout(
            Timeout::KeepContent,
            Some(t0),
            t0 + 9 * TIMEOUT_NS,
            false
        ));
    }
}
