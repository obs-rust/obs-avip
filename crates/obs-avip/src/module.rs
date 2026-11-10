//! Whether the plugin registers: a drop-in for DistroAV uses DistroAV's ids,
//! so scene collections and profiles load with no change. Two modules cannot
//! register one id (libobs keeps the first and logs an error for the second),
//! and two NDI senders would announce the same name. So the plugin decides in
//! `obs_module_post_load`, after every module loaded: when another module
//! (DistroAV, or a plugin built on the NDI Advanced SDK) already provides an
//! id, this plugin registers nothing and does not load the NDI runtime.

pub const SOURCE_ID: &str = "ndi_source";
pub const OUTPUT_ID: &str = "ndi_output";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    Register,
    /// Another module provides one or both ids.
    Refuse {
        source_taken: bool,
        output_taken: bool,
    },
}

pub fn decide(source_taken: bool, output_taken: bool) -> Registration {
    if source_taken || output_taken {
        Registration::Refuse {
            source_taken,
            output_taken,
        }
    } else {
        Registration::Register
    }
}

impl Registration {
    /// One log line for a refusal; `None` when the plugin registers.
    pub fn log_line(&self) -> Option<String> {
        let Registration::Refuse {
            source_taken,
            output_taken,
        } = *self
        else {
            return None;
        };
        let ids: Vec<String> = [(source_taken, SOURCE_ID), (output_taken, OUTPUT_ID)]
            .iter()
            .filter(|(taken, _)| *taken)
            .map(|(_, id)| format!("'{id}'"))
            .collect();
        Some(format!(
            "another module (DistroAV?) provides {}: this plugin is not registered and does not load the NDI runtime",
            ids.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_only_when_both_ids_are_free() {
        assert_eq!(decide(false, false), Registration::Register);
        assert_eq!(decide(false, false).log_line(), None);
        for (s, o) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                decide(s, o),
                Registration::Refuse {
                    source_taken: s,
                    output_taken: o
                },
                "{s} {o}"
            );
        }
    }

    #[test]
    fn a_refusal_names_the_taken_ids_in_one_line() {
        let line = decide(true, true).log_line().unwrap();
        assert!(!line.contains('\n'));
        assert!(line.contains("'ndi_source', 'ndi_output'"), "{line}");
        assert!(line.contains("not registered"), "{line}");
        let line = decide(false, true).log_line().unwrap();
        assert!(
            line.contains("'ndi_output'") && !line.contains("'ndi_source'"),
            "{line}"
        );
    }
}
