//! Tally: tell the sender (a camera's red or green light) whether the source
//! is on program or on preview. DistroAV's rule (`ndi-source.cpp:939-947` and
//! the activate/deactivate/show/hide callbacks), with its two switches from
//! `[NDIPlugin]` in the user config: `TallyProgramEnabled`,
//! `TallyPreviewEnabled`, both on by default.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    pub program: bool,
    pub preview: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TallyConfig {
    pub program: bool,
    pub preview: bool,
}

impl Default for TallyConfig {
    fn default() -> Self {
        TallyConfig {
            program: true,
            preview: true,
        }
    }
}

/// The OBS callback that changed the source's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Update,
    Activate,
    Deactivate,
    Show,
    Hide,
}

impl TallyConfig {
    /// The tally after `event`, with the source's state at that moment.
    /// Program: on program (active). Preview: showing but not on program.
    pub fn after(&self, event: Event, prev: Tally, active: bool, showing: bool) -> Tally {
        // In deactivate the source is leaving program, whatever the state
        // reads at that moment (DistroAV forces program off there).
        let active = active && event != Event::Deactivate;
        let preview = self.preview && showing && !active;
        match event {
            Event::Update | Event::Activate | Event::Deactivate => Tally {
                program: self.program && active,
                preview,
            },
            Event::Show => Tally {
                program: prev.program,
                preview,
            },
            Event::Hide => Tally {
                program: prev.program,
                preview: false,
            },
        }
    }
}

/// For an `AtomicU8` between the OBS callbacks and the receive thread.
pub fn encode(t: Tally) -> u8 {
    u8::from(t.program) | (u8::from(t.preview) << 1)
}

pub fn decode(v: u8) -> Tally {
    Tally {
        program: v & 1 != 0,
        preview: v & 2 != 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFF: Tally = Tally {
        program: false,
        preview: false,
    };
    const PROGRAM: Tally = Tally {
        program: true,
        preview: false,
    };
    const PREVIEW: Tally = Tally {
        program: false,
        preview: true,
    };

    #[test]
    fn on_program_is_program_only() {
        let c = TallyConfig::default();
        assert_eq!(c.after(Event::Activate, OFF, true, true), PROGRAM);
    }

    #[test]
    fn showing_but_not_active_is_preview() {
        let c = TallyConfig::default();
        assert_eq!(c.after(Event::Show, OFF, false, true), PREVIEW);
        assert_eq!(c.after(Event::Update, OFF, false, true), PREVIEW);
    }

    /// DistroAV forces program off in deactivate and preview off in hide,
    /// whatever the state reads at that moment.
    #[test]
    fn deactivate_and_hide_clear_their_light() {
        let c = TallyConfig::default();
        assert_eq!(c.after(Event::Deactivate, PROGRAM, true, true), PREVIEW);
        assert_eq!(c.after(Event::Deactivate, PROGRAM, false, false), OFF);
        assert_eq!(c.after(Event::Hide, PREVIEW, false, true), OFF);
        // Hide keeps program as it was: show does not fire for a source in
        // preview that is also in program, so hide leaves program alone.
        assert_eq!(c.after(Event::Hide, PROGRAM, true, true), PROGRAM);
    }

    #[test]
    fn the_switches_turn_each_light_off() {
        let no_program = TallyConfig {
            program: false,
            preview: true,
        };
        assert_eq!(no_program.after(Event::Activate, OFF, true, true), OFF);
        assert_eq!(no_program.after(Event::Show, OFF, false, true), PREVIEW);
        let no_preview = TallyConfig {
            program: true,
            preview: false,
        };
        assert_eq!(no_preview.after(Event::Show, OFF, false, true), OFF);
        assert_eq!(no_preview.after(Event::Activate, OFF, true, true), PROGRAM);
    }

    #[test]
    fn encode_round_trips() {
        for t in [
            OFF,
            PROGRAM,
            PREVIEW,
            Tally {
                program: true,
                preview: true,
            },
        ] {
            assert_eq!(decode(encode(t)), t);
        }
    }
}
