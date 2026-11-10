//! Source settings, compatible with DistroAV's `ndi_source`.
//!
//! A drop-in replacement must read a scene collection that DistroAV saved and
//! behave the same. DistroAV saves only the values the user changed; the rest
//! come from its defaults (`ndi-source.cpp`, `ndi_source_getdefaults`). So the
//! keys, the integer values, the defaults and the handling of unknown values
//! are all part of the contract. Reference: DistroAV fa41b38, `src/ndi-source.cpp`.

/// The settings keys, as DistroAV names them.
pub mod keys {
    pub const SOURCE: &str = "ndi_source_name";
    pub const BEHAVIOR: &str = "ndi_behavior";
    pub const TIMEOUT: &str = "ndi_behavior_timeout";
    pub const BANDWIDTH: &str = "ndi_bw_mode";
    pub const SYNC: &str = "ndi_sync";
    pub const FRAMESYNC: &str = "ndi_framesync";
    pub const HW_ACCEL: &str = "ndi_recv_hw_accel";
    pub const FIX_ALPHA: &str = "ndi_fix_alpha_blending";
    pub const YUV_RANGE: &str = "yuv_range";
    pub const YUV_COLORSPACE: &str = "yuv_colorspace";
    pub const LATENCY: &str = "latency";
    pub const AUDIO: &str = "ndi_audio";
    pub const PTZ: &str = "ndi_ptz";
    pub const PAN: &str = "ndi_pan";
    pub const TILT: &str = "ndi_tilt";
    pub const ZOOM: &str = "ndi_zoom";
    pub const WEB_CONTROL_URL: &str = "web_control_url";
}

/// What the receiver asks the sender for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    Highest,
    Lowest,
    AudioOnly,
}

/// What the source does while it is not shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behavior {
    KeepActive,
    StopResumeBlank,
    StopResumeLastFrame,
}

/// What the picture does when the sender stops sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeout {
    ClearContent,
    KeepContent,
}

/// Which NDI time the OBS frame timestamp comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sync {
    /// The time the sender sent the frame (`timestamp`).
    SenderTimestamp,
    /// The sender's timecode (`timecode`). DistroAV's default ("Source Timing").
    SenderTimecode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvRange {
    Partial,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    Bt601,
    Bt709,
    /// BT.2100 with the HLG transfer, as DistroAV maps it (`VIDEO_CS_2100_HLG`).
    Bt2100Hlg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Latency {
    Normal,
    Low,
    Lowest,
}

/// The pixel formats the receiver asks the SDK for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorFormat {
    /// `NDIlib_recv_color_format_UYVY_BGRA`
    UyvyBgra,
    /// `NDIlib_recv_color_format_fastest`
    Fastest,
}

/// A source's settings, after DistroAV's rules for unknown values.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSettings {
    pub name: String,
    pub bandwidth: Bandwidth,
    pub behavior: Behavior,
    pub timeout: Timeout,
    pub sync: Sync,
    pub framesync: bool,
    pub hw_accel: bool,
    pub yuv_range: YuvRange,
    pub colorspace: ColorSpace,
    pub latency: Latency,
    pub audio: bool,
}

/// A value of a settings key.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Str(&'static str),
}

/// Read access to a source's settings. `obs_data_get_*` returns the default
/// when a key is not set, so the shell can pass it straight through.
pub trait Read {
    fn int(&self, key: &str) -> i64;
    fn bool(&self, key: &str) -> bool;
    fn string(&self, key: &str) -> String;
}

/// The defaults, as `ndi_source_getdefaults` sets them.
pub fn defaults() -> Vec<(&'static str, Value)> {
    vec![
        (keys::BANDWIDTH, Value::Int(0)),      // highest
        (keys::BEHAVIOR, Value::Int(2)),       // stop, resume with the last frame
        (keys::TIMEOUT, Value::Int(1)),        // keep content
        (keys::SYNC, Value::Int(2)),           // sender timecode
        (keys::YUV_RANGE, Value::Int(1)),      // partial
        (keys::YUV_COLORSPACE, Value::Int(2)), // BT.709
        (keys::LATENCY, Value::Int(0)),        // normal
        (keys::AUDIO, Value::Bool(true)),
        (keys::WEB_CONTROL_URL, Value::Str("")),
    ]
}

/// Read the settings. The second value lists the keys to write back, as
/// DistroAV does in `ndi_source_update`: it corrects an invalid behavior and
/// the removed "Internal" sync mode, and never keeps the alpha fix set.
pub fn read(r: &impl Read) -> (SourceSettings, Vec<(&'static str, Value)>) {
    // The alpha fix is an action: DistroAV always writes it back as false.
    let mut rewrite = vec![(keys::FIX_ALPHA, Value::Bool(false))];
    let behavior = match r.int(keys::BEHAVIOR) {
        0 => Behavior::KeepActive,
        1 => Behavior::StopResumeBlank,
        2 => Behavior::StopResumeLastFrame,
        _ => {
            rewrite.push((keys::BEHAVIOR, Value::Int(0)));
            Behavior::KeepActive
        }
    };
    let sync = match r.int(keys::SYNC) {
        1 => Sync::SenderTimestamp,
        2 => Sync::SenderTimecode,
        0 => {
            rewrite.push((keys::SYNC, Value::Int(2)));
            Sync::SenderTimecode
        }
        _ => Sync::SenderTimecode,
    };
    let settings = SourceSettings {
        name: r.string(keys::SOURCE),
        bandwidth: match r.int(keys::BANDWIDTH) {
            1 => Bandwidth::Lowest,
            2 => Bandwidth::AudioOnly,
            _ => Bandwidth::Highest,
        },
        behavior,
        timeout: match r.int(keys::TIMEOUT) {
            1 => Timeout::KeepContent,
            _ => Timeout::ClearContent,
        },
        sync,
        framesync: r.bool(keys::FRAMESYNC),
        hw_accel: r.bool(keys::HW_ACCEL),
        yuv_range: match r.int(keys::YUV_RANGE) {
            2 => YuvRange::Full,
            _ => YuvRange::Partial,
        },
        colorspace: match r.int(keys::YUV_COLORSPACE) {
            1 => ColorSpace::Bt601,
            3 => ColorSpace::Bt2100Hlg,
            _ => ColorSpace::Bt709,
        },
        latency: match r.int(keys::LATENCY) {
            0 => Latency::Normal,
            2 => Latency::Lowest,
            _ => Latency::Low,
        },
        audio: r.bool(keys::AUDIO),
    };
    (settings, rewrite)
}

impl Latency {
    /// The color format the receiver asks for.
    pub fn color_format(self) -> ColorFormat {
        match self {
            Latency::Normal => ColorFormat::UyvyBgra,
            Latency::Low | Latency::Lowest => ColorFormat::Fastest,
        }
    }

    /// Whether OBS's async buffering is off for the source.
    pub fn unbuffered(self) -> bool {
        self == Latency::Lowest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Settings as OBS gives them: saved values over the defaults.
    struct Data(HashMap<&'static str, Value>);

    impl Data {
        fn saved(values: &[(&'static str, Value)]) -> Self {
            let mut map: HashMap<_, _> = defaults().into_iter().collect();
            for (k, v) in values {
                map.insert(*k, v.clone());
            }
            Data(map)
        }
    }

    impl Read for Data {
        fn int(&self, key: &str) -> i64 {
            match self.0.get(key) {
                Some(Value::Int(v)) => *v,
                _ => 0,
            }
        }
        fn bool(&self, key: &str) -> bool {
            matches!(self.0.get(key), Some(Value::Bool(true)))
        }
        fn string(&self, key: &str) -> String {
            match self.0.get(key) {
                Some(Value::Str(s)) => s.to_string(),
                _ => String::new(),
            }
        }
    }

    fn with(key: &'static str, v: i64) -> SourceSettings {
        read(&Data::saved(&[(key, Value::Int(v))])).0
    }

    /// DistroAV `ndi_source_getdefaults`, `ndi-source.cpp:337-345`.
    #[test]
    fn defaults_are_distroavs() {
        let d: HashMap<_, _> = defaults().into_iter().collect();
        assert_eq!(d[keys::BANDWIDTH], Value::Int(0));
        assert_eq!(d[keys::BEHAVIOR], Value::Int(2));
        assert_eq!(d[keys::TIMEOUT], Value::Int(1));
        assert_eq!(d[keys::SYNC], Value::Int(2));
        assert_eq!(d[keys::YUV_RANGE], Value::Int(1));
        assert_eq!(d[keys::YUV_COLORSPACE], Value::Int(2));
        assert_eq!(d[keys::LATENCY], Value::Int(0));
        assert_eq!(d[keys::AUDIO], Value::Bool(true));
        assert_eq!(d[keys::WEB_CONTROL_URL], Value::Str(""));
        assert_eq!(d.len(), 9, "DistroAV sets exactly these defaults");
    }

    /// The settings DistroAV saved for a camera source: only the name, the alpha fix
    /// and the web control URL. Everything else is a default.
    #[test]
    fn a_source_saved_by_distroav_reads_its_defaults() {
        let data = Data::saved(&[
            (keys::SOURCE, Value::Str("CAMERA-01 (Stream1)")),
            (keys::FIX_ALPHA, Value::Bool(false)),
            (keys::WEB_CONTROL_URL, Value::Str("http://camera/")),
        ]);
        let (s, rewrite) = read(&data);
        assert_eq!(
            s,
            SourceSettings {
                name: "CAMERA-01 (Stream1)".into(),
                bandwidth: Bandwidth::Highest,
                behavior: Behavior::StopResumeLastFrame,
                timeout: Timeout::KeepContent,
                sync: Sync::SenderTimecode,
                framesync: false,
                hw_accel: false,
                yuv_range: YuvRange::Partial,
                colorspace: ColorSpace::Bt709,
                latency: Latency::Normal,
                audio: true,
            }
        );
        assert_eq!(rewrite, vec![(keys::FIX_ALPHA, Value::Bool(false))]);
    }

    #[test]
    fn bandwidth_values() {
        assert_eq!(with(keys::BANDWIDTH, 0).bandwidth, Bandwidth::Highest);
        assert_eq!(with(keys::BANDWIDTH, 1).bandwidth, Bandwidth::Lowest);
        assert_eq!(with(keys::BANDWIDTH, 2).bandwidth, Bandwidth::AudioOnly);
        // The `default:` of DistroAV's switch: highest (also PROP_BW_UNDEFINED, -1).
        assert_eq!(with(keys::BANDWIDTH, -1).bandwidth, Bandwidth::Highest);
        assert_eq!(with(keys::BANDWIDTH, 7).bandwidth, Bandwidth::Highest);
    }

    #[test]
    fn behavior_values_and_the_invalid_value_rewrite() {
        assert_eq!(with(keys::BEHAVIOR, 0).behavior, Behavior::KeepActive);
        assert_eq!(with(keys::BEHAVIOR, 1).behavior, Behavior::StopResumeBlank);
        assert_eq!(
            with(keys::BEHAVIOR, 2).behavior,
            Behavior::StopResumeLastFrame
        );
        // An invalid value (from DistroAV 4.14.x) is forced to "keep active"
        // and written back (`ndi-source.cpp:1069-1079`).
        let (s, rewrite) = read(&Data::saved(&[(keys::BEHAVIOR, Value::Int(5))]));
        assert_eq!(s.behavior, Behavior::KeepActive);
        assert!(rewrite.contains(&(keys::BEHAVIOR, Value::Int(0))));
    }

    #[test]
    fn timeout_values() {
        assert_eq!(with(keys::TIMEOUT, 0).timeout, Timeout::ClearContent);
        assert_eq!(with(keys::TIMEOUT, 1).timeout, Timeout::KeepContent);
        // DistroAV keeps the content only for exactly 1 (`ndi-source.cpp:367`).
        assert_eq!(with(keys::TIMEOUT, 3).timeout, Timeout::ClearContent);
    }

    #[test]
    fn sync_values_and_the_internal_rewrite() {
        assert_eq!(with(keys::SYNC, 1).sync, Sync::SenderTimestamp);
        assert_eq!(with(keys::SYNC, 2).sync, Sync::SenderTimecode);
        // "Internal" (0) is no longer supported: it becomes source timing and
        // is written back (`ndi-source.cpp:1098-1104`).
        let (s, rewrite) = read(&Data::saved(&[(keys::SYNC, Value::Int(0))]));
        assert_eq!(s.sync, Sync::SenderTimecode);
        assert!(rewrite.contains(&(keys::SYNC, Value::Int(2))));
        // DistroAV leaves the timestamp unset for any other value (its switch
        // has no default). We use the default mode instead, and do not rewrite.
        let (s, rewrite) = read(&Data::saved(&[(keys::SYNC, Value::Int(9))]));
        assert_eq!(s.sync, Sync::SenderTimecode);
        assert!(!rewrite.iter().any(|(k, _)| *k == keys::SYNC));
    }

    #[test]
    fn color_values() {
        assert_eq!(with(keys::YUV_RANGE, 1).yuv_range, YuvRange::Partial);
        assert_eq!(with(keys::YUV_RANGE, 2).yuv_range, YuvRange::Full);
        assert_eq!(with(keys::YUV_RANGE, 0).yuv_range, YuvRange::Partial);
        assert_eq!(with(keys::YUV_COLORSPACE, 1).colorspace, ColorSpace::Bt601);
        assert_eq!(with(keys::YUV_COLORSPACE, 2).colorspace, ColorSpace::Bt709);
        assert_eq!(
            with(keys::YUV_COLORSPACE, 3).colorspace,
            ColorSpace::Bt2100Hlg
        );
        assert_eq!(with(keys::YUV_COLORSPACE, 0).colorspace, ColorSpace::Bt709);
    }

    #[test]
    fn latency_values_and_what_they_do() {
        assert_eq!(with(keys::LATENCY, 0).latency, Latency::Normal);
        assert_eq!(with(keys::LATENCY, 1).latency, Latency::Low);
        assert_eq!(with(keys::LATENCY, 2).latency, Latency::Lowest);
        // DistroAV tests only "== normal" and "== lowest", so any other value
        // (PROP_LATENCY_UNDEFINED, -1) behaves like low.
        assert_eq!(with(keys::LATENCY, -1).latency, Latency::Low);

        assert_eq!(Latency::Normal.color_format(), ColorFormat::UyvyBgra);
        assert_eq!(Latency::Low.color_format(), ColorFormat::Fastest);
        assert_eq!(Latency::Lowest.color_format(), ColorFormat::Fastest);
        // Only lowest turns OBS buffering off (`ndi-source.cpp:1120-1122`).
        assert!(!Latency::Normal.unbuffered());
        assert!(!Latency::Low.unbuffered());
        assert!(Latency::Lowest.unbuffered());
    }

    #[test]
    fn booleans() {
        let s = read(&Data::saved(&[
            (keys::AUDIO, Value::Bool(false)),
            (keys::FRAMESYNC, Value::Bool(true)),
            (keys::HW_ACCEL, Value::Bool(true)),
        ]))
        .0;
        assert!(!s.audio && s.framesync && s.hw_accel);
    }

    /// The alpha fix is an action, not a state: DistroAV adds its filter and
    /// sets the key back to false (`ndi-source.cpp:1106-1108`).
    #[test]
    fn the_alpha_fix_is_always_written_back_false() {
        let (_, rewrite) = read(&Data::saved(&[(keys::FIX_ALPHA, Value::Bool(true))]));
        assert!(rewrite.contains(&(keys::FIX_ALPHA, Value::Bool(false))));
    }
}
