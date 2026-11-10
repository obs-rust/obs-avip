//! NDI frames to what `obs_source_output_video` / `obs_source_output_audio`
//! need, without OBS types: the shell copies these fields into
//! `obs_source_frame` and `obs_source_audio`.

use crate::ndi::{fourcc, AudioFrame, VideoFrame, TIMESTAMP_UNDEFINED};
use crate::settings::SourceSettings;

/// `enum video_format` in libobs/media-io/video-io.h
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    I420 = 1,
    Nv12 = 2,
    Uyvy = 5,
    Rgba = 6,
    Bgra = 7,
    Bgrx = 8,
}

/// One plane of an OBS frame: where it starts in the NDI frame's data, and
/// its line size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plane {
    pub offset: usize,
    pub linesize: u32,
}

/// `VIDEO_TRC_DEFAULT` in libobs/media-io/video-io.h
pub const TRC_DEFAULT: u8 = 0;
/// `VIDEO_TRC_HLG`
pub const TRC_HLG: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObsVideo {
    pub format: VideoFormat,
    pub width: u32,
    pub height: u32,
    /// The planes OBS reads (`data[i]`, `linesize[i]`), `plane_count` of them.
    pub planes: [Plane; 3],
    pub plane_count: usize,
    /// `obs_source_frame.full_range`
    pub full_range: bool,
    /// `obs_source_frame.trc` (`enum video_trc`)
    pub trc: u8,
    pub timestamp: u64,
}

/// The planes of an NDI frame, as the SDK lays them out in one buffer: NV12
/// is the Y plane, then the interleaved UV plane, both with the frame's
/// stride; I420 is Y, then U, then V, the chroma planes with half the stride
/// and half the rows. The packed formats have one plane.
pub fn planes(format: VideoFormat, stride: u32, height: u32) -> ([Plane; 3], usize) {
    let y = stride as usize * height as usize;
    let half = stride / 2;
    let p = |offset, linesize| Plane { offset, linesize };
    match format {
        VideoFormat::Nv12 => ([p(0, stride), p(y, stride), Plane::default()], 2),
        VideoFormat::I420 => {
            let u = half as usize * (height as usize / 2);
            ([p(0, stride), p(y, half), p(y + u, half)], 3)
        }
        VideoFormat::Uyvy | VideoFormat::Rgba | VideoFormat::Bgra | VideoFormat::Bgrx => {
            ([p(0, stride), Plane::default(), Plane::default()], 1)
        }
    }
}

/// Whether OBS reads the frame as full range. NDI's RGB formats are always
/// full range (for them libobs's "limited" means a range compression the
/// sender never applied); the YUV formats follow the source's YUV range
/// setting.
pub fn full_range(format: VideoFormat, s: &SourceSettings) -> bool {
    match format {
        VideoFormat::Rgba | VideoFormat::Bgra | VideoFormat::Bgrx => true,
        VideoFormat::Uyvy | VideoFormat::I420 | VideoFormat::Nv12 => {
            s.yuv_range == crate::settings::YuvRange::Full
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObsAudio {
    /// `enum speaker_layout` in libobs/media-io/audio-io.h
    pub speakers: i32,
    /// Planes to pass: plane `i` starts at `data + i * stride`.
    pub planes: usize,
    pub stride: usize,
    pub frames: u32,
    pub rate: u32,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    /// A pixel or sample format OBS cannot take (the FourCC).
    Format(u32),
    /// A frame with no channels or no samples.
    Empty,
    /// A single field of a planar format (only packed fields are woven).
    PlanarField,
}

/// `NDIlib_frame_format_type_field_0`: the even lines of an interlaced frame.
pub const FRAME_FORMAT_FIELD_0: i32 = 2;
/// `NDIlib_frame_format_type_field_1`: the odd lines.
pub const FRAME_FORMAT_FIELD_1: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// Field 0: lines 0, 2, 4, ...
    First,
    /// Field 1: lines 1, 3, 5, ...
    Second,
}

/// The field of a video frame, or `None` for a whole frame (progressive or
/// interleaved). The SDK sends single fields from an interlaced sender with
/// the "fastest" color format, also when the receiver was created with
/// `allow_video_fields = false` (seen 2026-11-10 with SDK 6.0.1 and 6.3.2).
pub fn field(format_type: i32) -> Option<Field> {
    match format_type {
        FRAME_FORMAT_FIELD_0 => Some(Field::First),
        FRAME_FORMAT_FIELD_1 => Some(Field::Second),
        _ => None,
    }
}

/// Weaves field 0 and the field 1 after it into one full frame, as the SDK
/// itself does with the UYVY_BGRA color format. OBS's own deinterlace
/// setting then works on the woven frame. Packed formats only (one plane).
#[derive(Debug, Default)]
pub struct Weaver {
    buf: Vec<u8>,
    stride: usize,
    rows: usize,
    first_timestamp: Option<u64>,
}

impl Weaver {
    /// Adds one field (`rows` lines of `stride` bytes). Returns the woven
    /// frame (`2 * rows` lines, same stride) and field 0's timestamp when a
    /// field 1 completes a pair. A field 1 without a field 0 before it, or
    /// with another size, is dropped.
    pub fn push(
        &mut self,
        field: Field,
        data: &[u8],
        stride: usize,
        rows: usize,
        timestamp: u64,
    ) -> Option<(&[u8], u64)> {
        if stride == 0 || rows == 0 || data.len() < stride * rows {
            return None;
        }
        let line = |i: usize| &data[i * stride..(i + 1) * stride];
        match field {
            Field::First => {
                self.buf.resize(stride * rows * 2, 0);
                self.stride = stride;
                self.rows = rows;
                for i in 0..rows {
                    self.buf[2 * i * stride..(2 * i + 1) * stride].copy_from_slice(line(i));
                }
                self.first_timestamp = Some(timestamp);
                None
            }
            Field::Second => {
                let first = self.first_timestamp.take()?;
                if stride != self.stride || rows != self.rows {
                    return None;
                }
                for i in 0..rows {
                    self.buf[(2 * i + 1) * stride..(2 * i + 2) * stride].copy_from_slice(line(i));
                }
                Some((&self.buf, first))
            }
        }
    }
}

/// The OBS timestamp of a frame. A sender without timestamps gives
/// `NDIlib_recv_timestamp_undefined`; then the frame gets the arrival time.
pub fn frame_time(s: &SourceSettings, timestamp: i64, timecode: i64, now_ns: u64) -> u64 {
    let t = match s.sync {
        crate::settings::Sync::SenderTimestamp => timestamp,
        crate::settings::Sync::SenderTimecode => timecode,
    };
    if t == TIMESTAMP_UNDEFINED {
        now_ns
    } else {
        (t as u64).wrapping_mul(100)
    }
}

/// DistroAV's mapping (`ndi_source_thread_process_video2`): UYVA is shown as
/// UYVY (the alpha plane is dropped), RGBX as RGBA.
pub fn video(v: &VideoFrame, s: &SourceSettings, now_ns: u64) -> Result<ObsVideo, Skip> {
    let format = match v.fourcc {
        x if x == fourcc(b"UYVY") || x == fourcc(b"UYVA") => VideoFormat::Uyvy,
        x if x == fourcc(b"BGRA") => VideoFormat::Bgra,
        x if x == fourcc(b"BGRX") => VideoFormat::Bgrx,
        x if x == fourcc(b"RGBA") || x == fourcc(b"RGBX") => VideoFormat::Rgba,
        x if x == fourcc(b"I420") => VideoFormat::I420,
        x if x == fourcc(b"NV12") => VideoFormat::Nv12,
        other => return Err(Skip::Format(other)),
    };
    if v.xres <= 0 || v.yres <= 0 {
        return Err(Skip::Empty);
    }
    let (planes, plane_count) = planes(format, v.stride.max(0) as u32, v.yres as u32);
    Ok(ObsVideo {
        format,
        width: v.xres as u32,
        height: v.yres as u32,
        planes,
        plane_count,
        full_range: full_range(format, s),
        trc: if s.colorspace == crate::settings::ColorSpace::Bt2100Hlg {
            TRC_HLG
        } else {
            TRC_DEFAULT
        },
        timestamp: frame_time(s, v.timestamp, v.timecode, now_ns),
    })
}

/// Planar float only. More than 8 channels: the first 8. 7 channels: the first
/// 6 as 5.1, because libobs has no 7-channel layout and reads one plane per
/// channel of the layout (DistroAV gives SPEAKERS_UNKNOWN, which libobs refuses).
pub fn audio(a: &AudioFrame, s: &SourceSettings, now_ns: u64) -> Result<ObsAudio, Skip> {
    if a.fourcc != fourcc(b"FLTp") {
        return Err(Skip::Format(a.fourcc));
    }
    if a.channels <= 0 || a.samples <= 0 {
        return Err(Skip::Empty);
    }
    // enum speaker_layout: MONO 1 .. 5POINT1 6, 7POINT1 8.
    let (speakers, planes) = match a.channels {
        c @ 1..=6 => (c, c as usize),
        7 => (6, 6),
        _ => (8, 8),
    };
    Ok(ObsAudio {
        speakers,
        planes,
        stride: a.stride.max(0) as usize,
        frames: a.samples as u32,
        rate: a.sample_rate.max(0) as u32,
        timestamp: frame_time(s, a.timestamp, a.timecode, now_ns),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{self, Read, Sync};

    struct Defaults;
    impl Read for Defaults {
        fn int(&self, key: &str) -> i64 {
            settings::defaults()
                .into_iter()
                .find_map(|(k, v)| match (k == key, v) {
                    (true, settings::Value::Int(i)) => Some(i),
                    _ => None,
                })
                .unwrap_or(0)
        }
        fn bool(&self, key: &str) -> bool {
            key == settings::keys::AUDIO
        }
        fn string(&self, _: &str) -> String {
            String::new()
        }
    }

    fn defaults() -> SourceSettings {
        settings::read(&Defaults).0
    }

    fn vframe(code: &[u8; 4]) -> VideoFrame {
        VideoFrame {
            xres: 1920,
            yres: 1080,
            fourcc: fourcc(code),
            stride: 3840,
            timestamp: 5,
            timecode: 9,
            ..Default::default()
        }
    }

    fn aframe(channels: i32) -> AudioFrame {
        AudioFrame {
            sample_rate: 48000,
            channels,
            samples: 1024,
            fourcc: fourcc(b"FLTp"),
            stride: 4096,
            timestamp: 5,
            timecode: 9,
            ..Default::default()
        }
    }

    #[test]
    fn video_formats() {
        let s = defaults();
        let f = |c: &[u8; 4]| video(&vframe(c), &s, 0).map(|o| o.format);
        assert_eq!(f(b"UYVY"), Ok(VideoFormat::Uyvy));
        assert_eq!(f(b"UYVA"), Ok(VideoFormat::Uyvy));
        assert_eq!(f(b"BGRA"), Ok(VideoFormat::Bgra));
        assert_eq!(f(b"BGRX"), Ok(VideoFormat::Bgrx));
        assert_eq!(f(b"RGBA"), Ok(VideoFormat::Rgba));
        assert_eq!(f(b"RGBX"), Ok(VideoFormat::Rgba));
        assert_eq!(f(b"I420"), Ok(VideoFormat::I420));
        assert_eq!(f(b"NV12"), Ok(VideoFormat::Nv12));
        assert_eq!(f(b"P216"), Err(Skip::Format(fourcc(b"P216"))));
    }

    #[test]
    fn video_fields_and_time() {
        let o = video(&vframe(b"UYVY"), &defaults(), 0).unwrap();
        // The default sync is the sender timecode: 9 x 100 ns.
        assert_eq!(
            o,
            ObsVideo {
                format: VideoFormat::Uyvy,
                width: 1920,
                height: 1080,
                planes: [
                    Plane {
                        offset: 0,
                        linesize: 3840
                    },
                    Plane::default(),
                    Plane::default()
                ],
                plane_count: 1,
                full_range: false,
                trc: TRC_DEFAULT,
                timestamp: 900
            }
        );
    }

    /// An NV12 or I420 frame is one buffer; OBS needs a pointer per plane.
    /// With only the first, OBS reads the chroma from a null `data[1]`.
    #[test]
    fn planar_frames_give_every_plane() {
        // 1920x1080, stride 1920 (NDI's stride for the Y plane).
        let p = |offset, linesize| Plane { offset, linesize };
        assert_eq!(
            planes(VideoFormat::Nv12, 1920, 1080),
            ([p(0, 1920), p(1920 * 1080, 1920), Plane::default()], 2)
        );
        assert_eq!(
            planes(VideoFormat::I420, 1920, 1080),
            (
                [
                    p(0, 1920),
                    p(1920 * 1080, 960),
                    p(1920 * 1080 + 960 * 540, 960)
                ],
                3
            )
        );
        // A padded stride moves the chroma planes with it.
        assert_eq!(
            planes(VideoFormat::Nv12, 2048, 1080).0[1],
            p(2048 * 1080, 2048)
        );
        assert_eq!(planes(VideoFormat::Uyvy, 3840, 1080).1, 1);
        let mut f = vframe(b"I420");
        f.stride = 1920;
        let o = video(&f, &defaults(), 0).unwrap();
        assert_eq!(o.plane_count, 3);
        assert_eq!(o.planes[2].offset, 1920 * 1080 + 960 * 540);
    }

    /// RGB from NDI is full range; with `full_range = false` libobs would
    /// apply its "RGB limited" conversion and stretch the levels.
    #[test]
    fn rgb_is_full_range_and_yuv_follows_the_setting() {
        let mut s = defaults();
        assert!(full_range(VideoFormat::Bgra, &s));
        assert!(full_range(VideoFormat::Bgrx, &s));
        assert!(full_range(VideoFormat::Rgba, &s));
        assert!(!full_range(VideoFormat::Uyvy, &s));
        assert!(!full_range(VideoFormat::Nv12, &s));
        s.yuv_range = crate::settings::YuvRange::Full;
        assert!(full_range(VideoFormat::Uyvy, &s));
        assert!(full_range(VideoFormat::I420, &s));
    }

    #[test]
    fn hlg_color_space_sets_the_hlg_transfer() {
        let mut s = defaults();
        assert_eq!(video(&vframe(b"UYVY"), &s, 0).unwrap().trc, TRC_DEFAULT);
        s.colorspace = crate::settings::ColorSpace::Bt2100Hlg;
        assert_eq!(video(&vframe(b"UYVY"), &s, 0).unwrap().trc, TRC_HLG);
    }

    #[test]
    fn an_undefined_time_becomes_the_arrival_time() {
        let mut s = defaults();
        s.sync = Sync::SenderTimestamp;
        assert_eq!(frame_time(&s, TIMESTAMP_UNDEFINED, 9, 42), 42);
        assert_eq!(frame_time(&s, 5, 9, 42), 500);
        s.sync = Sync::SenderTimecode;
        assert_eq!(frame_time(&s, 5, TIMESTAMP_UNDEFINED, 42), 42);
        assert_eq!(frame_time(&s, 5, 9, 42), 900);
    }

    #[test]
    fn audio_layouts() {
        let s = defaults();
        let layout = |ch: i32| audio(&aframe(ch), &s, 0).map(|o| (o.speakers, o.planes));
        assert_eq!(layout(1), Ok((1, 1)));
        assert_eq!(layout(2), Ok((2, 2)));
        assert_eq!(layout(3), Ok((3, 3)));
        assert_eq!(layout(4), Ok((4, 4)));
        assert_eq!(layout(5), Ok((5, 5)));
        assert_eq!(layout(6), Ok((6, 6)));
        assert_eq!(layout(7), Ok((6, 6)));
        assert_eq!(layout(8), Ok((8, 8)));
        assert_eq!(layout(16), Ok((8, 8)));
        assert_eq!(layout(0), Err(Skip::Empty));
    }

    #[test]
    fn audio_fields_and_format_check() {
        let s = defaults();
        assert_eq!(
            audio(&aframe(2), &s, 0),
            Ok(ObsAudio {
                speakers: 2,
                planes: 2,
                stride: 4096,
                frames: 1024,
                rate: 48000,
                timestamp: 900
            })
        );
        let mut interleaved = aframe(2);
        interleaved.fourcc = fourcc(b"FLT ");
        assert_eq!(
            audio(&interleaved, &s, 0),
            Err(Skip::Format(fourcc(b"FLT ")))
        );
        let mut empty = aframe(2);
        empty.samples = 0;
        assert_eq!(audio(&empty, &s, 0), Err(Skip::Empty));
    }

    #[test]
    fn field_types() {
        assert_eq!(field(0), None); // interleaved
        assert_eq!(field(1), None); // progressive
        assert_eq!(field(FRAME_FORMAT_FIELD_0), Some(Field::First));
        assert_eq!(field(FRAME_FORMAT_FIELD_1), Some(Field::Second));
    }

    #[test]
    fn weave_two_fields() {
        // 3 lines of 4 bytes per field: field 0 lines 0,2,4; field 1 lines 1,3,5.
        let f0: Vec<u8> = [[0u8; 4], [2; 4], [4; 4]].concat();
        let f1: Vec<u8> = [[1u8; 4], [3; 4], [5; 4]].concat();
        let mut w = Weaver::default();
        assert_eq!(w.push(Field::First, &f0, 4, 3, 100), None);
        let (frame, ts) = w.push(Field::Second, &f1, 4, 3, 200).unwrap();
        let lines: Vec<u8> = frame.chunks(4).map(|l| l[0]).collect();
        assert_eq!(lines, [0, 1, 2, 3, 4, 5]);
        assert_eq!(ts, 100, "the woven frame has field 0's time");
        // The pair is used up: another field 1 alone gives nothing.
        assert_eq!(w.push(Field::Second, &f1, 4, 3, 300), None);
    }

    #[test]
    fn weave_drops_unpaired_and_changed_fields() {
        let f = vec![7u8; 12];
        let mut w = Weaver::default();
        // Field 1 first (stream started on field 1): dropped.
        assert_eq!(w.push(Field::Second, &f, 4, 3, 0), None);
        // Field 0 twice: the second one starts the pair.
        assert_eq!(w.push(Field::First, &[1u8; 12], 4, 3, 10), None);
        assert_eq!(w.push(Field::First, &f, 4, 3, 20), None);
        assert_eq!(
            w.push(Field::Second, &f, 4, 3, 30).map(|(_, t)| t),
            Some(20)
        );
        // A field 1 with another size than its field 0: dropped.
        assert_eq!(w.push(Field::First, &f, 4, 3, 40), None);
        assert_eq!(w.push(Field::Second, &[7u8; 16], 4, 4, 50), None);
        // Too little data for the size: dropped.
        assert_eq!(w.push(Field::First, &[0u8; 8], 4, 3, 60), None);
    }
}
