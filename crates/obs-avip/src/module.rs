//! Whether the plugin registers: a drop-in for DistroAV uses DistroAV's ids,
//! so scene collections and profiles load with no change. Two modules cannot
//! register one id (libobs keeps the first and logs an error for the second),
//! and two NDI senders would announce the same name. So the plugin decides in
//! `obs_module_post_load`, after every module loaded: when another module
//! (DistroAV, or a plugin built on the NDI Advanced SDK) already provides an
//! id, this plugin registers nothing and does not load the NDI runtime.

use crate::filter;

pub const SOURCE_ID: &str = "ndi_source";
pub const OUTPUT_ID: &str = "ndi_output";

/// Every id the plugin registers, in the order of a log line.
pub const IDS: [&str; 4] = [SOURCE_ID, OUTPUT_ID, filter::VIDEO_ID, filter::AUDIO_ID];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    Register,
    /// Another module provides these ids.
    Refuse {
        taken: Vec<&'static str>,
    },
}

/// `taken` holds the ids of [`IDS`] that another module provides.
pub fn decide(taken: &[&'static str]) -> Registration {
    let _ = taken;
    todo!()
}

impl Registration {
    /// One log line for a refusal; `None` when the plugin registers.
    pub fn log_line(&self) -> Option<String> {
        let Registration::Refuse { taken } = self else {
            return None;
        };
        let ids: Vec<String> = taken.iter().map(|id| format!("'{id}'")).collect();
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
    fn registers_only_when_every_id_is_free() {
        assert_eq!(decide(&[]), Registration::Register);
        assert_eq!(decide(&[]).log_line(), None);
        for id in IDS {
            assert_eq!(
                decide(&[id]),
                Registration::Refuse { taken: vec![id] },
                "{id}"
            );
        }
        // A filter id alone is enough: two modules cannot register one id.
        assert_eq!(
            decide(&[filter::AUDIO_ID]),
            Registration::Refuse {
                taken: vec!["ndi_audiofilter"]
            }
        );
    }

    #[test]
    fn a_refusal_names_the_taken_ids_in_ids_order() {
        let line = decide(&[OUTPUT_ID, SOURCE_ID]).log_line().unwrap();
        assert!(!line.contains('\n'));
        assert!(line.contains("'ndi_source', 'ndi_output'"), "{line}");
        assert!(line.contains("not registered"), "{line}");
        let line = decide(&[filter::VIDEO_ID]).log_line().unwrap();
        assert!(
            line.contains("'ndi_filter'") && !line.contains("'ndi_source'"),
            "{line}"
        );
    }
}
