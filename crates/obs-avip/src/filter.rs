//! DistroAV's dedicated output filters: `ndi_filter` sends the picture and
//! audio of the source it is on as its own NDI stream, `ndi_audiofilter` only
//! the audio. Same ids, keys, default name and naming rules as DistroAV
//! (fa41b38, `src/ndi-filter.cpp`), so saved collections keep them.
//!
//! The filter renders the source as DistroAV does: the source with the
//! filters before this one, only while the source is showing and the filter
//! is enabled, at the source's own size, as BGRA (with alpha). The read-back
//! uses two stage surfaces: a frame is staged in one frame and read in the
//! next, so the render thread does not wait for the GPU (DistroAV maps the
//! surface it just staged).

pub const VIDEO_ID: &str = "ndi_filter";
pub const AUDIO_ID: &str = "ndi_audiofilter";

/// The settings keys.
pub mod keys {
    pub const NAME: &str = "ndi_filter_ndiname";
    pub const GROUPS: &str = "ndi_filter_ndigroups";
}

/// DistroAV's default stream name (`NDIPlugin.FilterProps.NDIName.Default`).
pub const DEFAULT_NAME: &str = "${filter} (${source})";
pub const SOURCE_TOKEN: &str = "${source}";
pub const FILTER_TOKEN: &str = "${filter}";

/// Characters DistroAV replaces with a space in a stream name.
pub const FORBIDDEN: &str = "\\/:*?\"<>|";

/// Every forbidden character replaced with a space.
pub fn sanitize(s: &str) -> String {
    let _ = s;
    todo!()
}

/// The NDI name of a filter's stream, and which renames change it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamName {
    pub name: String,
    /// The template uses `${source}`: a rename of the source changes the name.
    pub watch_source: bool,
    /// The template uses `${filter}`: a rename of the filter changes the name.
    pub watch_filter: bool,
}

/// DistroAV's rule: the template is sanitized (and saved back so), the
/// tokens are replaced with the source and filter names, and the result is
/// sanitized again (a source name may hold a forbidden character). An empty
/// template gives an empty name: the SDK then names the stream itself.
pub fn stream_name(template: &str, source: &str, filter: &str) -> StreamName {
    let _ = (template, source, filter);
    todo!()
}

/// Whether the video filter renders and sends this frame: the filter is
/// enabled, its source is showing (in the program, the preview or a
/// projector) and has a size.
pub fn sends_video(enabled: bool, source_showing: bool, width: u32, height: u32) -> bool {
    let _ = (enabled, source_showing, width, height);
    todo!()
}

/// What the render thread does with the two stage surfaces in one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// The size changed (or this is the first frame): create both surfaces
    /// again at the new size.
    pub recreate: bool,
    /// Stage this frame into this surface.
    pub stage: usize,
    /// Read this surface: it holds the frame staged in the frame before.
    pub read: Option<usize>,
}

/// The order of the two stage surfaces.
#[derive(Debug, Default)]
pub struct Readback {
    size: Option<(u32, u32)>,
    staged: [bool; 2],
    next: usize,
}

impl Readback {
    /// A frame that renders at `width` x `height`.
    pub fn frame(&mut self, width: u32, height: u32) -> Step {
        let _ = (width, height, &self.size, &self.staged, &self.next);
        todo!()
    }

    /// A frame that does not render (the filter or the source is off): the
    /// staged frame is dropped, so a later frame never sends an old picture.
    pub fn idle(&mut self) {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_keys_and_default_are_distroavs() {
        assert_eq!(VIDEO_ID, "ndi_filter");
        assert_eq!(AUDIO_ID, "ndi_audiofilter");
        assert_eq!(keys::NAME, "ndi_filter_ndiname");
        assert_eq!(keys::GROUPS, "ndi_filter_ndigroups");
        assert_eq!(DEFAULT_NAME, "${filter} (${source})");
    }

    #[test]
    fn forbidden_characters_become_spaces() {
        assert_eq!(sanitize(r#"a\b/c:d*e?f"g<h>i|j"#), "a b c d e f g h i j");
        assert_eq!(sanitize("Cam 1 (Clean)"), "Cam 1 (Clean)");
        assert_eq!(sanitize("Kamera für Bühne"), "Kamera für Bühne");
        assert_eq!(sanitize(""), "");
    }

    #[test]
    fn the_default_name_takes_both_names() {
        assert_eq!(
            stream_name(DEFAULT_NAME, "Camera", "Dedicated NDI output"),
            StreamName {
                name: "Dedicated NDI output (Camera)".into(),
                watch_source: true,
                watch_filter: true,
            }
        );
    }

    #[test]
    fn tokens_decide_which_renames_matter() {
        let n = stream_name("Clean ${source}", "Cam", "F");
        assert_eq!(
            (n.name.as_str(), n.watch_source, n.watch_filter),
            ("Clean Cam", true, false)
        );
        let n = stream_name("${filter}", "Cam", "Iso 1");
        assert_eq!(
            (n.name.as_str(), n.watch_source, n.watch_filter),
            ("Iso 1", false, true)
        );
        let n = stream_name("Fixed name", "Cam", "F");
        assert_eq!(
            (n.name.as_str(), n.watch_source, n.watch_filter),
            ("Fixed name", false, false)
        );
        let n = stream_name("${source}+${source}", "A", "F");
        assert_eq!(n.name, "A+A");
    }

    #[test]
    fn names_are_sanitized_before_and_after_the_tokens() {
        // A forbidden character in a source name is replaced too.
        assert_eq!(stream_name("${source}", "Cam: wide", "F").name, "Cam  wide");
        // In the template it is replaced before the tokens: "${source}|" is
        // still a token followed by a space.
        assert_eq!(stream_name("${source}|x", "Cam", "F").name, "Cam x");
        assert_eq!(stream_name("", "Cam", "F").name, "");
    }

    #[test]
    fn video_is_sent_only_when_enabled_showing_and_sized() {
        assert!(sends_video(true, true, 1920, 1080));
        assert!(!sends_video(false, true, 1920, 1080));
        assert!(!sends_video(true, false, 1920, 1080));
        assert!(!sends_video(true, true, 0, 1080));
        assert!(!sends_video(true, true, 1920, 0));
    }

    #[test]
    fn a_frame_is_read_one_frame_after_it_is_staged() {
        let mut r = Readback::default();
        let s1 = r.frame(4, 2);
        assert_eq!(
            s1,
            Step {
                recreate: true,
                stage: 0,
                read: None
            }
        );
        let s2 = r.frame(4, 2);
        assert_eq!(
            s2,
            Step {
                recreate: false,
                stage: 1,
                read: Some(0)
            }
        );
        let s3 = r.frame(4, 2);
        assert_eq!(
            s3,
            Step {
                recreate: false,
                stage: 0,
                read: Some(1)
            }
        );
    }

    #[test]
    fn a_size_change_starts_again() {
        let mut r = Readback::default();
        r.frame(4, 2);
        r.frame(4, 2);
        let s = r.frame(8, 4);
        assert!(s.recreate);
        assert_eq!(s.read, None, "the staged frame has the old size");
        let s = r.frame(8, 4);
        assert!(!s.recreate);
        assert_eq!(s.read, Some(1 - s.stage));
    }

    #[test]
    fn a_pause_drops_the_staged_frame() {
        let mut r = Readback::default();
        r.frame(4, 2);
        r.frame(4, 2);
        r.idle();
        let s = r.frame(4, 2);
        assert!(!s.recreate, "same size: the surfaces stay");
        assert_eq!(s.read, None, "no old picture after the pause");
        assert_eq!(r.frame(4, 2).read, Some(s.stage));
    }
}
