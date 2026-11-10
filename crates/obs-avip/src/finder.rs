//! Discovery of NDI senders, for the source list in the properties.
//!
//! Like DistroAV (`ndi-finder.cpp`), a finder runs only while someone looks at
//! the list: at most every `REFRESH_EVERY_NS`, in a background thread, and it
//! is destroyed when the list is complete. No finder runs in the background
//! otherwise, so the plugin adds no discovery traffic to the network while
//! the properties are closed.

use crate::ndi::Api;

pub const REFRESH_EVERY_NS: u64 = 5_000_000_000;
/// One wait of the finder, as DistroAV.
pub const WAIT_MS: u32 = 1000;

/// Find the senders on the network: wait until the list stops growing, then
/// destroy the finder. Sorted (ignoring case), without duplicates.
/// `cancelled` is asked before each wait: when it says yes (the plugin is
/// unloading), the finder is destroyed and the list so far is returned.
pub fn find_all(api: &dyn Api, wait_ms: u32, cancelled: &dyn Fn() -> bool) -> Vec<String> {
    // SAFETY: the finder is created, used and destroyed here, on one thread.
    unsafe {
        let find = api.find_create(true);
        if find.is_null() {
            return Vec::new();
        }
        let mut list = Vec::new();
        while !cancelled() {
            api.find_wait(find, wait_ms);
            let before = list.len();
            list = api.find_sources(find);
            if list.len() <= before {
                break;
            }
        }
        api.find_destroy(find);
        list.sort_by_key(|n| n.to_lowercase());
        list.dedup();
        list
    }
}

/// The last list, and when to look again.
#[derive(Debug, Default)]
pub struct Cache {
    list: Vec<String>,
    last: Option<u64>,
    refreshing: bool,
}

impl Cache {
    pub const fn new() -> Cache {
        Cache {
            list: Vec::new(),
            last: None,
            refreshing: false,
        }
    }

    pub fn list(&self) -> &[String] {
        &self.list
    }

    /// `true`: start a refresh now (and it counts as running until `finish`).
    pub fn start_refresh(&mut self, now: u64) -> bool {
        let due = self
            .last
            .is_none_or(|t| now.saturating_sub(t) >= REFRESH_EVERY_NS);
        if self.refreshing || !due {
            return false;
        }
        self.refreshing = true;
        true
    }

    pub fn finish(&mut self, list: Vec<String>, now: u64) {
        self.list = list;
        self.last = Some(now);
        self.refreshing = false;
    }

    /// The refresh that `start_refresh` allowed did not run (its thread did
    /// not start): keep the list, allow the next refresh.
    pub fn cancel(&mut self) {
        self.refreshing = false;
    }
}

/// One entry of the source list: what the user sees, and the saved value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub value: String,
}

/// The list for the properties. The current source stays in the list when
/// the sender is not found (offline, or not discovered yet), marked as such,
/// so opening the properties never changes the selection.
pub fn source_items(found: &[String], current: &str) -> Vec<Item> {
    let mut items = Vec::new();
    if !current.is_empty() && !found.iter().any(|f| f == current) {
        items.push(Item {
            label: format!("{current} (not found)"),
            value: current.to_string(),
        });
    }
    items.extend(found.iter().map(|f| Item {
        label: f.clone(),
        value: f.clone(),
    }));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ndi::mock::{Call, Mock};

    #[test]
    fn find_all_waits_until_the_list_stops_growing() {
        let api = Mock::default();
        api.set_discovery(vec![
            vec![],
            vec!["CAM-B (Stream1)"],
            vec!["CAM-B (Stream1)", "cam-a (Stream1)", "CAM-B (Stream1)"],
            vec!["CAM-B (Stream1)", "cam-a (Stream1)", "CAM-B (Stream1)"],
        ]);
        assert_eq!(
            find_all(&api, 1, &|| false),
            vec!["cam-a (Stream1)", "CAM-B (Stream1)"]
        );
        assert_eq!(api.calls(), vec![Call::FindCreate, Call::FindDestroy]);
        assert_eq!(api.live_finders(), 0);
    }

    /// At unload the discovery thread must end soon: no new wait once cancelled.
    #[test]
    fn a_cancelled_find_stops_waiting_and_destroys_the_finder() {
        let api = Mock::default();
        api.set_discovery(vec![vec!["A (1)"], vec!["A (1)", "B (1)"]]);
        assert_eq!(find_all(&api, 1, &|| true), Vec::<String>::new());
        assert_eq!(api.live_finders(), 0);
    }

    #[test]
    fn find_all_with_nothing_on_the_network() {
        let api = Mock::default();
        assert_eq!(find_all(&api, 1, &|| false), Vec::<String>::new());
        assert_eq!(api.live_finders(), 0);
    }

    #[test]
    fn the_cache_refreshes_at_most_every_5_s_and_never_twice_at_once() {
        let s = 1_000_000_000;
        let mut c = Cache::default();
        assert!(c.start_refresh(0), "the first look starts a refresh");
        assert!(!c.start_refresh(s), "one is running");
        c.finish(vec!["A".into()], 2 * s);
        assert_eq!(c.list(), ["A"]);
        assert!(!c.start_refresh(6 * s), "4 s after the last one");
        assert!(c.start_refresh(7 * s));
    }

    #[test]
    fn the_current_source_stays_in_the_list_when_it_is_not_found() {
        let found = vec!["A (1)".to_string(), "B (1)".to_string()];
        assert_eq!(
            source_items(&found, "B (1)"),
            vec![
                Item {
                    label: "A (1)".into(),
                    value: "A (1)".into()
                },
                Item {
                    label: "B (1)".into(),
                    value: "B (1)".into()
                },
            ]
        );
        assert_eq!(
            source_items(&found, "CAM (2)"),
            vec![
                Item {
                    label: "CAM (2) (not found)".into(),
                    value: "CAM (2)".into()
                },
                Item {
                    label: "A (1)".into(),
                    value: "A (1)".into()
                },
                Item {
                    label: "B (1)".into(),
                    value: "B (1)".into()
                },
            ]
        );
        assert_eq!(source_items(&[], ""), vec![]);
    }

    #[test]
    fn a_refresh_that_did_not_run_does_not_block_the_next() {
        let mut c = Cache::new();
        assert!(c.start_refresh(0));
        assert!(!c.start_refresh(1));
        c.cancel();
        assert!(c.start_refresh(2));
        assert!(c.list().is_empty());
    }
}
