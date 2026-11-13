//! One description for every NDI output (#6): what it sends and how.
//!
//! The main and preview outputs, the per-source outputs (#5) and the outputs
//! of the outputs window (#7) all run on the same `ndi_output` type. This
//! module holds what they share and what can be tested without OBS: the output
//! spec, its checks, the frame-rate divisor and the mapping of DistroAV's
//! `[NDIPlugin]` keys to the main and preview outputs.

/// What an output sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// OBS's program (the main mix).
    Program,
    /// The studio-mode preview scene, or the program outside studio mode.
    Preview,
    /// One scene, by UUID.
    Scene(String),
    /// One source, by UUID.
    Source(String),
}

/// The audio an output sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audio {
    None,
    /// An OBS mix track, 0 to 5 (track 1 to 6 in the UI).
    Track(u8),
    /// The source's own audio. libobs gives a plug-in the audio of one source
    /// only for async sources (`source_signal_audio_data` is called from
    /// `source_output_audio_data` only), so this is valid for `Content::Source`
    /// only; a scene takes a track.
    Source,
}

/// The video an output sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Video {
    /// `None`: the canvas size; otherwise the scaled size.
    pub size: Option<(u32, u32)>,
    /// Send every nth frame of OBS's frame rate; 1 sends all.
    pub fps_divisor: u32,
}

impl Default for Video {
    fn default() -> Self {
        Video {
            size: None,
            fps_divisor: 1,
        }
    }
}

/// One NDI output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSpec {
    /// The NDI name of the stream (the machine name is put in front by NDI).
    pub name: String,
    /// The NDI groups, comma separated; empty for the default group.
    pub groups: String,
    pub content: Content,
    pub video: Video,
    pub audio: Audio,
    pub enabled: bool,
}

/// Why a spec cannot run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecError {
    NoName,
    /// The frame-rate divisor is 0 or larger than `MAX_FPS_DIVISOR`.
    FpsDivisor(u32),
    /// A scaled size that is zero or odd (the NDI formats subsample chroma).
    Size(u32, u32),
    /// A mix track outside 0 to 5.
    Track(u8),
    /// `Audio::Source` on content other than a source.
    SourceAudio,
    /// `Content::Scene` or `Content::Source` without a UUID.
    NoUuid,
}

/// The largest frame-rate divisor: 60 fps down to 1 fps.
pub const MAX_FPS_DIVISOR: u32 = 60;
/// OBS has 6 mix tracks.
pub const TRACKS: u8 = 6;

impl OutputSpec {
    /// Check the spec before an output is created from it.
    pub fn check(&self) -> Result<(), SpecError> {
        if self.name.is_empty() {
            return Err(SpecError::NoName);
        }
        if let Content::Scene(uuid) | Content::Source(uuid) = &self.content {
            if uuid.is_empty() {
                return Err(SpecError::NoUuid);
            }
        }
        let d = self.video.fps_divisor;
        if d == 0 || d > MAX_FPS_DIVISOR {
            return Err(SpecError::FpsDivisor(d));
        }
        if let Some((w, h)) = self.video.size {
            if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
                return Err(SpecError::Size(w, h));
            }
        }
        match self.audio {
            Audio::Track(t) if t >= TRACKS => Err(SpecError::Track(t)),
            Audio::Source if !matches!(self.content, Content::Source(_)) => {
                Err(SpecError::SourceAudio)
            }
            _ => Ok(()),
        }
    }
}

/// The NDI frame rate of an output: OBS's rate divided by the divisor.
pub fn ndi_fps(fps_num: u32, fps_den: u32, divisor: u32) -> (u32, u32) {
    (fps_num, fps_den.saturating_mul(divisor.max(1)))
}

/// Lets every nth frame through, starting with the first.
#[derive(Debug, Clone)]
pub struct FrameGate {
    divisor: u32,
    count: u32,
}

impl FrameGate {
    pub fn new(divisor: u32) -> Self {
        FrameGate {
            divisor: divisor.max(1),
            count: 0,
        }
    }

    /// `true` for the frames the output sends.
    pub fn pass(&mut self) -> bool {
        let pass = self.count == 0;
        self.count = (self.count + 1) % self.divisor;
        pass
    }
}

/// The two outputs DistroAV keeps in `[NDIPlugin]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Legacy {
    Main,
    Preview,
}

impl Legacy {
    pub const ALL: [Legacy; 2] = [Legacy::Main, Legacy::Preview];

    /// The `[NDIPlugin]` keys: enabled, name, groups.
    pub fn keys(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Legacy::Main => ("MainOutputEnabled", "MainOutputName", "MainOutputGroups"),
            Legacy::Preview => (
                "PreviewOutputEnabled",
                "PreviewOutputName",
                "PreviewOutputGroups",
            ),
        }
    }

    /// DistroAV's default names.
    pub fn default_name(self) -> &'static str {
        match self {
            Legacy::Main => "OBS PGM",
            Legacy::Preview => "OBS Preview",
        }
    }

    /// The spec of this output from its `[NDIPlugin]` values (`None`: the key
    /// is not set). The main output sends track 1, the preview no audio, as
    /// DistroAV does.
    pub fn spec(
        self,
        enabled: Option<bool>,
        name: Option<&str>,
        groups: Option<&str>,
    ) -> OutputSpec {
        let (content, audio) = match self {
            Legacy::Main => (Content::Program, Audio::Track(0)),
            Legacy::Preview => (Content::Preview, Audio::None),
        };
        OutputSpec {
            name: name.unwrap_or(self.default_name()).to_string(),
            groups: groups.unwrap_or_default().to_string(),
            content,
            video: Video::default(),
            audio,
            enabled: enabled.unwrap_or(false),
        }
    }
}

/// The `ndi_output` settings keys. `ndi_name`, `ndi_groups`, `uses_video` and
/// `uses_audio` are DistroAV's (obs-websocket clients set them); the others
/// are obs-avip's and default to what DistroAV does.
pub mod keys {
    pub const NAME: &str = "ndi_name";
    pub const GROUPS: &str = "ndi_groups";
    pub const USES_VIDEO: &str = "uses_video";
    pub const USES_AUDIO: &str = "uses_audio";
    /// Scaled width and height; 0 for the canvas size.
    pub const WIDTH: &str = "avip_width";
    pub const HEIGHT: &str = "avip_height";
    pub const FPS_DIVISOR: &str = "avip_fps_divisor";
    /// The mix track, 0 to 5.
    pub const TRACK: &str = "avip_track";
}

/// The `ndi_output` settings of a spec whose content the shell maps to the
/// output's media (Program, Preview): the values for `keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSettings {
    pub name: String,
    pub groups: String,
    pub uses_video: bool,
    pub uses_audio: bool,
    pub width: u32,
    pub height: u32,
    pub fps_divisor: u32,
    pub track: u8,
}

impl OutputSpec {
    pub fn settings(&self) -> OutputSettings {
        let (width, height) = self.video.size.unwrap_or((0, 0));
        OutputSettings {
            name: self.name.clone(),
            groups: self.groups.clone(),
            uses_video: true,
            uses_audio: self.audio != Audio::None,
            width,
            height,
            fps_divisor: self.video.fps_divisor.max(1),
            track: match self.audio {
                Audio::Track(t) => t,
                _ => 0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(name: &str) -> OutputSpec {
        OutputSpec {
            name: name.into(),
            groups: String::new(),
            content: Content::Program,
            video: Video::default(),
            audio: Audio::Track(0),
            enabled: true,
        }
    }

    #[test]
    fn a_plain_program_output_is_valid() {
        assert_eq!(program("OBS PGM").check(), Ok(()));
    }

    #[test]
    fn checks_name_divisor_size_track_and_uuid() {
        let mut s = program("");
        assert_eq!(s.check(), Err(SpecError::NoName));
        s.name = "x".into();
        s.video.fps_divisor = 0;
        assert_eq!(s.check(), Err(SpecError::FpsDivisor(0)));
        s.video.fps_divisor = MAX_FPS_DIVISOR + 1;
        assert_eq!(s.check(), Err(SpecError::FpsDivisor(61)));
        s.video.fps_divisor = 2;
        s.video.size = Some((1919, 1080));
        assert_eq!(s.check(), Err(SpecError::Size(1919, 1080)));
        s.video.size = Some((0, 1080));
        assert_eq!(s.check(), Err(SpecError::Size(0, 1080)));
        s.video.size = Some((1920, 1080));
        s.audio = Audio::Track(TRACKS);
        assert_eq!(s.check(), Err(SpecError::Track(6)));
        s.audio = Audio::Track(5);
        assert_eq!(s.check(), Ok(()));
        s.content = Content::Scene(String::new());
        assert_eq!(s.check(), Err(SpecError::NoUuid));
    }

    #[test]
    fn source_audio_only_for_a_source() {
        let mut s = program("x");
        s.audio = Audio::Source;
        assert_eq!(s.check(), Err(SpecError::SourceAudio));
        s.content = Content::Scene("u-1".into());
        assert_eq!(s.check(), Err(SpecError::SourceAudio));
        s.content = Content::Source("u-2".into());
        assert_eq!(s.check(), Ok(()));
    }

    #[test]
    fn ndi_fps_divides_the_rate() {
        assert_eq!(ndi_fps(50, 1, 1), (50, 1));
        assert_eq!(ndi_fps(50, 1, 2), (50, 2));
        assert_eq!(ndi_fps(60000, 1001, 2), (60000, 2002));
        // A divisor of 0 is treated as 1: never a zero denominator.
        assert_eq!(ndi_fps(30, 1, 0), (30, 1));
    }

    #[test]
    fn frame_gate_passes_every_nth_frame_from_the_first() {
        let mut all = FrameGate::new(1);
        assert!((0..5).all(|_| all.pass()));
        let mut half = FrameGate::new(2);
        let got: Vec<bool> = (0..6).map(|_| half.pass()).collect();
        assert_eq!(got, [true, false, true, false, true, false]);
        let mut third = FrameGate::new(3);
        let got: Vec<bool> = (0..7).map(|_| third.pass()).collect();
        assert_eq!(got, [true, false, false, true, false, false, true]);
        // 0 behaves as 1.
        let mut zero = FrameGate::new(0);
        assert!((0..3).all(|_| zero.pass()));
    }

    #[test]
    fn legacy_outputs_match_distroav() {
        let main = Legacy::Main.spec(None, None, None);
        assert_eq!(main.name, "OBS PGM");
        assert_eq!(main.content, Content::Program);
        assert_eq!(main.audio, Audio::Track(0));
        assert!(!main.enabled);
        assert_eq!(main.video, Video::default());
        let preview = Legacy::Preview.spec(Some(true), Some("Studio"), Some("a,b"));
        assert_eq!(preview.name, "Studio");
        assert_eq!(preview.groups, "a,b");
        assert_eq!(preview.content, Content::Preview);
        assert_eq!(preview.audio, Audio::None);
        assert!(preview.enabled);
        // An empty name stays empty (the output is then not started).
        assert_eq!(Legacy::Main.spec(Some(true), Some(""), None).name, "");
    }

    #[test]
    fn settings_of_the_legacy_outputs_are_distroavs() {
        let main = Legacy::Main
            .spec(Some(true), Some("PGM"), Some("g"))
            .settings();
        assert_eq!(
            main,
            OutputSettings {
                name: "PGM".into(),
                groups: "g".into(),
                uses_video: true,
                uses_audio: true,
                width: 0,
                height: 0,
                fps_divisor: 1,
                track: 0,
            }
        );
        let preview = Legacy::Preview.spec(None, None, None).settings();
        assert!(preview.uses_video);
        assert!(!preview.uses_audio);
    }

    #[test]
    fn settings_carry_size_divisor_and_track() {
        let mut s = program("x");
        s.video = Video {
            size: Some((1280, 720)),
            fps_divisor: 2,
        };
        s.audio = Audio::Track(3);
        let o = s.settings();
        assert_eq!(
            (o.width, o.height, o.fps_divisor, o.track),
            (1280, 720, 2, 3)
        );
        assert!(o.uses_audio);
        s.audio = Audio::None;
        assert!(!s.settings().uses_audio);
    }
}
