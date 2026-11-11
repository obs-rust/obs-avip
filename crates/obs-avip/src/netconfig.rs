//! NDI network settings: the NDI runtime's own config file
//! (`ndi-config.v1.json`), written from obs-avip's settings window.
//!
//! The Standard NDI SDK has no API for transport, discovery or adapter
//! settings. It reads one config file per process, from `$HOME/.ndi/` or from
//! the folder in `NDI_CONFIG_DIR`, once, at the first `NDIlib_initialize`.
//! Checked with NDI 6.0.1 on macOS: a changed file, a changed
//! `NDI_CONFIG_DIR` and `NDIlib_destroy` + `NDIlib_initialize` all keep the
//! first settings. So a change applies at the next OBS start.
//!
//! The user picks where the settings come from:
//! - the system settings (NDI Access Manager, `~/.ndi`), the default;
//! - obs-avip's settings: obs-avip writes the file into its own config folder
//!   and points `NDI_CONFIG_DIR` there before it loads the runtime.
//!
//! A `NDI_CONFIG_DIR` that is already set (by the user or a launcher) always
//! wins: obs-avip does not override it.
//!
//! Only the keys the user set are written. A transport left on "Default"
//! writes nothing, so the runtime keeps its own default.

use std::collections::BTreeMap;

/// The config file's name.
pub const FILE_NAME: &str = "ndi-config.v1.json";

/// The user.ini section of the window.
pub const SECTION: &str = "AVIPNetwork";

/// The key that selects where the settings come from, and its values.
pub const SOURCE_KEY: &str = "SettingsSource";
pub const SOURCE_SYSTEM: &str = "system";
pub const SOURCE_OWN: &str = "obs-avip";

/// A transport switch.
pub const TRI_DEFAULT: &str = "default";
pub const TRI_ON: &str = "on";
pub const TRI_OFF: &str = "off";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Free text; empty writes nothing.
    Text,
    /// `default`, `on` or `off`; `default` writes nothing.
    Tri,
}

/// One setting: its user.ini key and its place in the config file.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub key: &'static str,
    pub kind: Kind,
    pub path: &'static [&'static str],
}

/// Every setting of the window, in window order. Key names follow the
/// config file of the NDI SDK documentation and of NDI Access Manager.
pub const FIELDS: &[Field] = &[
    Field {
        key: "Discovery",
        kind: Kind::Text,
        path: &["networks", "discovery"],
    },
    Field {
        key: "ExtraIps",
        kind: Kind::Text,
        path: &["networks", "ips"],
    },
    Field {
        key: "RecvGroups",
        kind: Kind::Text,
        path: &["groups", "recv"],
    },
    Field {
        key: "SendGroups",
        kind: Kind::Text,
        path: &["groups", "send"],
    },
    Field {
        key: "SourceFilter",
        kind: Kind::Text,
        path: &["sourcefilter", "regex"],
    },
    Field {
        key: "RudpRecv",
        kind: Kind::Tri,
        path: &["rudp", "recv", "enable"],
    },
    Field {
        key: "RudpSend",
        kind: Kind::Tri,
        path: &["rudp", "send", "enable"],
    },
    Field {
        key: "TcpRecv",
        kind: Kind::Tri,
        path: &["tcp", "recv", "enable"],
    },
    Field {
        key: "TcpSend",
        kind: Kind::Tri,
        path: &["tcp", "send", "enable"],
    },
    Field {
        key: "UdpRecv",
        kind: Kind::Tri,
        path: &["unicast", "recv", "enable"],
    },
    Field {
        key: "UdpSend",
        kind: Kind::Tri,
        path: &["unicast", "send", "enable"],
    },
    Field {
        key: "MulticastRecv",
        kind: Kind::Tri,
        path: &["multicast", "recv", "enable"],
    },
    Field {
        key: "MulticastSubnets",
        kind: Kind::Text,
        path: &["multicast", "recv", "subnets"],
    },
    Field {
        key: "Adapters",
        kind: Kind::Text,
        path: &["adapters", "allowed"],
    },
];

enum Node {
    Obj(BTreeMap<&'static str, Node>),
    Str(String),
    Bool(bool),
}

fn insert(root: &mut BTreeMap<&'static str, Node>, path: &[&'static str], value: Node) {
    let (last, parents) = path.split_last().expect("a field has a path");
    let mut map = root;
    for p in parents {
        let entry = map.entry(p).or_insert_with(|| Node::Obj(BTreeMap::new()));
        map = match entry {
            Node::Obj(m) => m,
            // FIELDS never puts a value where an object is.
            _ => unreachable!("config path {p} is a value"),
        };
    }
    map.insert(last, value);
}

fn escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write(node: &Node, indent: usize, out: &mut String) {
    match node {
        Node::Str(s) => escape(s, out),
        Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Node::Obj(map) => {
            out.push('{');
            let mut first = true;
            for (k, v) in map {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push('\n');
                out.push_str(&"  ".repeat(indent + 1));
                escape(k, out);
                out.push_str(": ");
                write(v, indent + 1, out);
            }
            if !map.is_empty() {
                out.push('\n');
                out.push_str(&"  ".repeat(indent));
            }
            out.push('}');
        }
    }
}

/// The entries of the adapter list: (label, value). "All adapters" (empty
/// value: nothing written, the runtime uses every adapter) comes first, then
/// one entry per address in `found` (interface name, IP), sorted by interface
/// and without repeats. A saved value that is not among them (an adapter that
/// is gone, or a comma list from an older or hand-edited setting) stays
/// selectable, marked "(not found)".
pub fn adapter_choices(found: &[(String, String)], saved: &str) -> Vec<(String, String)> {
    let mut list = found.to_vec();
    list.sort();
    list.dedup_by(|a, b| a.1 == b.1);
    let mut out = vec![("All adapters (default)".to_string(), String::new())];
    out.extend(
        list.iter()
            .map(|(name, ip)| (format!("{ip} ({name})"), ip.clone())),
    );
    let saved = saved.trim();
    if !saved.is_empty() && !list.iter().any(|(_, ip)| ip == saved) {
        out.push((format!("{saved} (not found)"), saved.to_string()));
    }
    out
}

/// The config file for the window's values. `get` returns a field's value by
/// its user.ini key (empty when not set). Text is trimmed; empty text and
/// "default" are left out.
pub fn to_json(get: impl Fn(&str) -> String) -> String {
    let mut ndi = BTreeMap::new();
    for f in FIELDS {
        let v = get(f.key);
        let v = v.trim();
        let node = match f.kind {
            Kind::Text if v.is_empty() => continue,
            Kind::Text => Node::Str(v.to_string()),
            Kind::Tri => match v {
                TRI_ON => Node::Bool(true),
                TRI_OFF => Node::Bool(false),
                _ => continue,
            },
        };
        insert(&mut ndi, f.path, node);
    }
    let mut root = BTreeMap::new();
    root.insert("ndi", Node::Obj(ndi));
    let mut out = String::new();
    write(&Node::Obj(root), 0, &mut out);
    out.push('\n');
    out
}

/// Where the NDI runtime takes its settings from in this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigDir {
    /// `$HOME/.ndi` (NDI Access Manager): nothing to do.
    System,
    /// obs-avip's own folder: write the file, set `NDI_CONFIG_DIR`.
    Own(String),
    /// `NDI_CONFIG_DIR` was already set; obs-avip leaves it.
    UserEnv(String),
}

/// Decide before the runtime loads. `env` is `NDI_CONFIG_DIR`.
pub fn choose(use_own: bool, env: Option<&str>, own_dir: &str) -> ConfigDir {
    match env.map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) => ConfigDir::UserEnv(e.to_string()),
        None if use_own => ConfigDir::Own(own_dir.to_string()),
        None => ConfigDir::System,
    }
}

impl ConfigDir {
    /// The log line, and the "in effect" line of the window.
    pub fn describe(&self, use_own: bool) -> String {
        match self {
            ConfigDir::System => {
                "NDI settings: the system settings (NDI Access Manager, ~/.ndi)".to_string()
            }
            ConfigDir::Own(d) => format!("NDI settings: obs-avip's network settings ({d})"),
            ConfigDir::UserEnv(d) if use_own => format!(
                "NDI settings: NDI_CONFIG_DIR ({d}); obs-avip's network settings are not used while it is set"
            ),
            ConfigDir::UserEnv(d) => format!("NDI settings: NDI_CONFIG_DIR ({d})"),
        }
    }
}

/// What the runtime took at this start, to tell when the window's values
/// differ from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    System,
    /// obs-avip's settings, with the file as it was written at the start.
    Own(String),
    /// `NDI_CONFIG_DIR` was set: the window's values are not used at all.
    UserEnv,
}

/// Whether the window's values (`now_own`: obs-avip's settings selected;
/// `now_json`: `to_json` of the values) differ from what the runtime took
/// at this start, so that they apply only after a restart.
pub fn restart_needed(started: &Started, now_own: bool, now_json: &str) -> bool {
    match started {
        Started::UserEnv => false,
        Started::System => now_own,
        Started::Own(json) => !now_own || json != now_json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn json(pairs: &[(&str, &str)]) -> String {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        to_json(|k| m.get(k).cloned().unwrap_or_default())
    }

    #[test]
    fn nothing_set_writes_an_empty_ndi_object() {
        assert_eq!(json(&[]), "{\n  \"ndi\": {}\n}\n");
    }

    #[test]
    fn default_and_empty_values_are_left_out() {
        let j = json(&[
            ("RudpRecv", "default"),
            ("Discovery", "   "),
            ("TcpSend", ""),
        ]);
        assert_eq!(j, "{\n  \"ndi\": {}\n}\n");
    }

    #[test]
    fn values_go_to_the_sdk_paths() {
        let j = json(&[
            ("Discovery", " 10.0.0.5:5959, 10.0.0.6 "),
            ("ExtraIps", "10.1.0.20"),
            ("RecvGroups", "Studio"),
            ("RudpRecv", "off"),
            ("TcpRecv", "on"),
            ("MulticastRecv", "on"),
            ("MulticastSubnets", "10.2.0.0"),
            ("Adapters", "10.0.0.2"),
        ]);
        let want = r#"{
  "ndi": {
    "adapters": {
      "allowed": "10.0.0.2"
    },
    "groups": {
      "recv": "Studio"
    },
    "multicast": {
      "recv": {
        "enable": true,
        "subnets": "10.2.0.0"
      }
    },
    "networks": {
      "discovery": "10.0.0.5:5959, 10.0.0.6",
      "ips": "10.1.0.20"
    },
    "rudp": {
      "recv": {
        "enable": false
      }
    },
    "tcp": {
      "recv": {
        "enable": true
      }
    }
  }
}
"#;
        assert_eq!(j, want);
    }

    #[test]
    fn send_and_recv_are_separate() {
        let j = json(&[("UdpSend", "off"), ("UdpRecv", "on")]);
        assert!(j.contains("\"unicast\": {\n      \"recv\": {\n        \"enable\": true\n      },\n      \"send\": {\n        \"enable\": false\n      }"));
    }

    #[test]
    fn text_is_escaped() {
        let j = json(&[("SourceFilter", "^Cam \"A\"\\d$")]);
        assert!(j.contains(r#""regex": "^Cam \"A\"\\d$""#), "{j}");
    }

    #[test]
    fn every_field_has_a_unique_key_and_path() {
        let mut keys: Vec<_> = FIELDS.iter().map(|f| f.key).collect();
        let mut paths: Vec<_> = FIELDS.iter().map(|f| f.path.join(".")).collect();
        keys.sort();
        keys.dedup();
        paths.sort();
        paths.dedup();
        assert_eq!(keys.len(), FIELDS.len());
        assert_eq!(paths.len(), FIELDS.len());
        // A value never sits where another field needs an object.
        for p in &paths {
            assert!(
                !paths.iter().any(|q| q.starts_with(&format!("{p}."))),
                "{p}"
            );
        }
    }

    fn found(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(n, i)| (n.to_string(), i.to_string()))
            .collect()
    }

    #[test]
    fn adapters_start_with_all_and_follow_the_interfaces() {
        let c = adapter_choices(&found(&[("en1", "10.0.0.9"), ("en0", "10.0.0.23")]), "");
        assert_eq!(
            c,
            vec![
                ("All adapters (default)".into(), "".into()),
                ("10.0.0.23 (en0)".into(), "10.0.0.23".into()),
                ("10.0.0.9 (en1)".into(), "10.0.0.9".into()),
            ]
        );
    }

    #[test]
    fn a_saved_adapter_that_is_gone_stays_selectable() {
        let c = adapter_choices(&found(&[("en0", "10.0.0.23")]), "10.9.9.9");
        assert_eq!(
            c.last().unwrap(),
            &("10.9.9.9 (not found)".into(), "10.9.9.9".into())
        );
        // A present one is not repeated.
        let c = adapter_choices(&found(&[("en0", "10.0.0.23")]), "10.0.0.23");
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn an_address_on_two_interfaces_is_listed_once() {
        let c = adapter_choices(&found(&[("en0", "10.0.0.1"), ("bridge0", "10.0.0.1")]), "");
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn an_existing_env_always_wins() {
        assert_eq!(
            choose(true, Some("/x"), "/own"),
            ConfigDir::UserEnv("/x".into())
        );
        assert_eq!(
            choose(false, Some("/x"), "/own"),
            ConfigDir::UserEnv("/x".into())
        );
    }

    #[test]
    fn own_settings_only_when_chosen() {
        assert_eq!(choose(true, None, "/own"), ConfigDir::Own("/own".into()));
        assert_eq!(choose(false, None, "/own"), ConfigDir::System);
        // An empty env counts as not set.
        assert_eq!(
            choose(true, Some(" "), "/own"),
            ConfigDir::Own("/own".into())
        );
    }

    #[test]
    fn the_line_says_when_own_settings_are_ignored() {
        let d = choose(true, Some("/x"), "/own");
        assert!(d.describe(true).contains("not used"));
        assert!(!d.describe(false).contains("not used"));
    }

    #[test]
    fn restart_is_needed_only_when_values_differ_from_the_start() {
        let started_json = json(&[("RecvGroups", "Studio")]);
        let own = Started::Own(started_json.clone());
        assert!(!restart_needed(&own, true, &started_json));
        assert!(restart_needed(
            &own,
            true,
            &json(&[("RecvGroups", "Other")])
        ));
        assert!(restart_needed(&own, false, &started_json));
        // On the system settings, only switching to obs-avip needs a restart.
        assert!(!restart_needed(
            &Started::System,
            false,
            &json(&[("RecvGroups", "X")])
        ));
        assert!(restart_needed(&Started::System, true, &json(&[])));
        // NDI_CONFIG_DIR wins: nothing in the window applies.
        assert!(!restart_needed(
            &Started::UserEnv,
            true,
            &json(&[("RecvGroups", "X")])
        ));
    }
}
