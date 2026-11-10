//! NDI send for the output: OBS raw frames to NDI frames.
//!
//! Three points where DistroAV's output (`ndi-output.cpp`, fa41b38) is wrong,
//! and this one is not:
//! - Buffer lifetime. `NDIlib_send_send_video_async_v2` may read a frame's data
//!   until the next call. DistroAV passes OBS's own frame buffer, which OBS
//!   reuses after `raw_video` returns. Here every frame is copied into one of
//!   two owned buffers, used in turn: frame N+1 goes to the other buffer, and
//!   handing it over releases frame N's.
//! - Planes. For NV12 and I420 DistroAV passes only `data[0]` and assumes the
//!   planes follow without padding. Here they are packed tightly, as NDI reads
//!   them (stride = width; the chroma planes follow the Y plane).
//! - Frame rate. DistroAV sends `fps * 100 / 100`, which is wrong for
//!   29.97 or 59.94. Here it is OBS's `fps_num / fps_den`.

use std::ffi::CStr;

use crate::ndi::{fourcc, Api, AudioFrame, Handle, VideoFrame};

/// `NDIlib_send_timecode_synthesize`
pub const TIMECODE_SYNTHESIZE: i64 = i64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutFormat {
    Nv12,
    I420,
    Rgba,
    Bgra,
    Bgrx,
    Uyvy,
    /// OBS I444, sent as UYVY (the chroma of every second pixel is dropped,
    /// as DistroAV does).
    I444ToUyvy,
}

impl OutFormat {
    pub fn fourcc(self) -> u32 {
        match self {
            OutFormat::Nv12 => fourcc(b"NV12"),
            OutFormat::I420 => fourcc(b"I420"),
            OutFormat::Rgba => fourcc(b"RGBA"),
            OutFormat::Bgra => fourcc(b"BGRA"),
            OutFormat::Bgrx => fourcc(b"BGRX"),
            OutFormat::Uyvy | OutFormat::I444ToUyvy => fourcc(b"UYVY"),
        }
    }

    /// The bytes of one packed NDI frame, and its stride.
    pub fn packed(self, width: u32, height: u32) -> (usize, usize) {
        let (w, h) = (width as usize, height as usize);
        match self {
            OutFormat::Nv12 | OutFormat::I420 => (w * h * 3 / 2, w),
            OutFormat::Rgba | OutFormat::Bgra | OutFormat::Bgrx => (w * h * 4, w * 4),
            OutFormat::Uyvy | OutFormat::I444ToUyvy => (w * h * 2, w * 2),
        }
    }
}

/// The NDI format for an OBS output format (`enum video_format`), or the OBS
/// value when NDI has none (P010, I010, P216, P416, ...).
pub fn out_format(obs: i32) -> Result<OutFormat, i32> {
    match obs {
        1 => Ok(OutFormat::I420),
        2 => Ok(OutFormat::Nv12),
        5 => Ok(OutFormat::Uyvy),
        6 => Ok(OutFormat::Rgba),
        7 => Ok(OutFormat::Bgra),
        8 => Ok(OutFormat::Bgrx),
        10 => Ok(OutFormat::I444ToUyvy),
        other => Err(other),
    }
}

/// The rows of each OBS plane for `format`, to size the plane slices.
pub fn plane_rows(format: OutFormat, height: u32) -> [usize; 3] {
    let h = height as usize;
    match format {
        OutFormat::Nv12 => [h, h / 2, 0],
        OutFormat::I420 => [h, h / 2, h / 2],
        OutFormat::I444ToUyvy => [h, h, h],
        OutFormat::Rgba | OutFormat::Bgra | OutFormat::Bgrx | OutFormat::Uyvy => [h, 0, 0],
    }
}

/// One raw OBS video frame (`struct video_data`), as slices.
pub struct VideoIn<'a> {
    pub planes: [&'a [u8]; 3],
    pub linesize: [u32; 3],
    pub timestamp_ns: u64,
}

/// Copy `v` into `out`, packed as NDI reads `format`.
pub fn pack(format: OutFormat, width: u32, height: u32, v: &VideoIn, out: &mut [u8]) {
    let (w, h) = (width as usize, height as usize);
    // Copy `rows` rows of `row` bytes from a plane with `stride`, to `at`.
    let copy = |out: &mut [u8], at: usize, plane: &[u8], stride: u32, row: usize, rows: usize| {
        for r in 0..rows {
            let src = &plane[r * stride as usize..][..row];
            out[at + r * row..][..row].copy_from_slice(src);
        }
        at + rows * row
    };
    match format {
        OutFormat::Nv12 => {
            let at = copy(out, 0, v.planes[0], v.linesize[0], w, h);
            copy(out, at, v.planes[1], v.linesize[1], w, h / 2);
        }
        OutFormat::I420 => {
            let at = copy(out, 0, v.planes[0], v.linesize[0], w, h);
            let at = copy(out, at, v.planes[1], v.linesize[1], w / 2, h / 2);
            copy(out, at, v.planes[2], v.linesize[2], w / 2, h / 2);
        }
        OutFormat::Rgba | OutFormat::Bgra | OutFormat::Bgrx => {
            copy(out, 0, v.planes[0], v.linesize[0], w * 4, h);
        }
        OutFormat::Uyvy => {
            copy(out, 0, v.planes[0], v.linesize[0], w * 2, h);
        }
        OutFormat::I444ToUyvy => {
            for r in 0..h {
                let [y, u, vv] = [0, 1, 2].map(|i| &v.planes[i][r * v.linesize[i] as usize..]);
                let row = &mut out[r * w * 2..][..w * 2];
                for x in (0..w.saturating_sub(1)).step_by(2) {
                    row[x * 2..x * 2 + 4].copy_from_slice(&[u[x], y[x], vv[x], y[x + 1]]);
                }
            }
        }
    }
}

/// Logs the number of receivers, checked at most once a second.
#[derive(Debug, Default)]
pub struct Connections {
    last_check: Option<u64>,
    count: Option<i32>,
}

impl Connections {
    pub const EVERY_NS: u64 = 1_000_000_000;

    /// Whether to ask the SDK now.
    pub fn due(&mut self, now: u64) -> bool {
        if self
            .last_check
            .is_some_and(|t| now.saturating_sub(t) < Self::EVERY_NS)
        {
            return false;
        }
        self.last_check = Some(now);
        true
    }

    /// The new count, when it changed.
    pub fn observe(&mut self, n: i32) -> Option<i32> {
        if self.count == Some(n) {
            return None;
        }
        self.count = Some(n);
        Some(n)
    }
}

pub struct Sender<'a> {
    api: &'a dyn Api,
    send: Handle,
    format: OutFormat,
    width: u32,
    height: u32,
    rate: (i32, i32),
    bufs: [Vec<u8>; 2],
    next: usize,
    audio: Vec<f32>,
    connections: Connections,
}

impl<'a> Sender<'a> {
    /// # Safety
    /// `name` and `groups` are valid C strings for the call.
    pub unsafe fn create(
        api: &'a dyn Api,
        name: &CStr,
        groups: Option<&CStr>,
        format: OutFormat,
        width: u32,
        height: u32,
        fps: (u32, u32),
    ) -> Option<Sender<'a>> {
        let send = api.send_create(name, groups);
        if send.is_null() {
            return None;
        }
        let (len, _) = format.packed(width, height);
        Some(Sender {
            api,
            send,
            format,
            width,
            height,
            rate: (fps.0 as i32, fps.1.max(1) as i32),
            bufs: [vec![0; len], vec![0; len]],
            next: 0,
            audio: Vec::new(),
            connections: Connections::default(),
        })
    }

    pub fn video(&mut self, v: &VideoIn) {
        let (_, stride) = self.format.packed(self.width, self.height);
        // The other buffer than the frame the SDK may still read.
        let buf = &mut self.bufs[self.next];
        pack(self.format, self.width, self.height, v, buf);
        let frame = VideoFrame {
            xres: self.width as i32,
            yres: self.height as i32,
            fourcc: self.format.fourcc(),
            rate_n: self.rate.0,
            rate_d: self.rate.1,
            aspect: 0.0,    // width / height
            format_type: 1, // progressive
            timecode: TIMECODE_SYNTHESIZE,
            data: buf.as_mut_ptr(),
            stride: stride as i32,
            metadata: std::ptr::null(),
            timestamp: (v.timestamp_ns / 100) as i64,
        };
        // SAFETY: `buf` lives until this sender is dropped, and is not written
        // again before the next frame (in the other buffer) is handed over.
        unsafe { self.api.send_video_async(self.send, Some(&frame)) };
        self.next ^= 1;
    }

    /// Planar float audio: one slice of `frames` samples per channel.
    pub fn audio(&mut self, planes: &[&[f32]], frames: u32, rate: u32, timestamp_ns: u64) {
        let n = frames as usize;
        self.audio.clear();
        for plane in planes {
            self.audio.extend_from_slice(&plane[..n]);
        }
        let frame = AudioFrame {
            sample_rate: rate as i32,
            channels: planes.len() as i32,
            samples: frames as i32,
            timecode: TIMECODE_SYNTHESIZE,
            fourcc: fourcc(b"FLTp"),
            data: self.audio.as_mut_ptr().cast(),
            stride: (n * 4) as i32,
            metadata: std::ptr::null(),
            timestamp: (timestamp_ns / 100) as i64,
        };
        // SAFETY: the SDK copies the audio before it returns.
        unsafe { self.api.send_audio(self.send, &frame) };
    }

    /// The receiver count when it changed, checked at most once a second.
    pub fn connections(&mut self, now: u64) -> Option<i32> {
        if !self.connections.due(now) {
            return None;
        }
        // SAFETY: `send` is this sender's.
        let n = unsafe { self.api.send_connections(self.send, 0) };
        self.connections.observe(n)
    }
}

impl Drop for Sender<'_> {
    fn drop(&mut self) {
        // SAFETY: wait until the SDK is done with the last frame, then destroy;
        // the buffers are freed after this.
        unsafe {
            self.api.send_video_async(self.send, None);
            self.api.send_destroy(self.send);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ndi::mock::{Call, Mock};
    use std::ffi::CString;

    fn sender(api: &Mock, format: OutFormat, w: u32, h: u32, fps: (u32, u32)) -> Sender<'_> {
        let name = CString::new("OBS").unwrap();
        unsafe { Sender::create(api, &name, None, format, w, h, fps) }.unwrap()
    }

    /// NV12 4x2 with OBS rows padded to 8 bytes.
    fn nv12_in(y: &mut [u8; 16], uv: &mut [u8; 8], seed: u8) {
        for (i, b) in y.iter_mut().enumerate() {
            *b = if i % 8 < 4 { seed + i as u8 } else { 0xEE };
        }
        for (i, b) in uv.iter_mut().enumerate() {
            *b = if i < 4 { 100 + seed + i as u8 } else { 0xEE };
        }
    }

    #[test]
    fn plane_rows_per_format() {
        assert_eq!(plane_rows(OutFormat::Nv12, 1080), [1080, 540, 0]);
        assert_eq!(plane_rows(OutFormat::I420, 1080), [1080, 540, 540]);
        assert_eq!(plane_rows(OutFormat::I444ToUyvy, 1080), [1080, 1080, 1080]);
        assert_eq!(plane_rows(OutFormat::Bgra, 1080), [1080, 0, 0]);
    }

    #[test]
    fn output_formats() {
        assert_eq!(out_format(1), Ok(OutFormat::I420));
        assert_eq!(out_format(2), Ok(OutFormat::Nv12));
        assert_eq!(out_format(5), Ok(OutFormat::Uyvy));
        assert_eq!(out_format(6), Ok(OutFormat::Rgba));
        assert_eq!(out_format(7), Ok(OutFormat::Bgra));
        assert_eq!(out_format(8), Ok(OutFormat::Bgrx));
        assert_eq!(out_format(10), Ok(OutFormat::I444ToUyvy));
        assert_eq!(out_format(4), Err(4)); // YUY2
        assert_eq!(out_format(13), Err(13));
    }

    #[test]
    fn nv12_planes_are_packed_without_the_obs_padding() {
        let (mut y, mut uv) = ([0u8; 16], [0u8; 8]);
        nv12_in(&mut y, &mut uv, 1);
        let v = VideoIn {
            planes: [&y, &uv, &[]],
            linesize: [8, 8, 0],
            timestamp_ns: 0,
        };
        let mut out = [0u8; 12];
        pack(OutFormat::Nv12, 4, 2, &v, &mut out);
        assert_eq!(out, [1, 2, 3, 4, 9, 10, 11, 12, 101, 102, 103, 104]);
    }

    #[test]
    fn i420_planes_follow_each_other() {
        let y: Vec<u8> = (0..8).collect(); // 4x2, no padding
        let u = [50u8, 51, 0xEE, 0xEE]; // 2x1, padded to 4
        let vv = [60u8, 61, 0xEE, 0xEE];
        let v = VideoIn {
            planes: [&y, &u, &vv],
            linesize: [4, 4, 4],
            timestamp_ns: 0,
        };
        let mut out = [0u8; 12];
        pack(OutFormat::I420, 4, 2, &v, &mut out);
        assert_eq!(out, [0, 1, 2, 3, 4, 5, 6, 7, 50, 51, 60, 61]);
    }

    #[test]
    fn i444_becomes_uyvy() {
        let (y, u, vv) = ([1u8, 2, 3, 4], [10u8, 11, 12, 13], [20u8, 21, 22, 23]);
        let v = VideoIn {
            planes: [&y, &u, &vv],
            linesize: [4, 4, 4],
            timestamp_ns: 0,
        };
        let mut out = [0u8; 8];
        pack(OutFormat::I444ToUyvy, 4, 1, &v, &mut out);
        assert_eq!(out, [10, 1, 20, 2, 12, 3, 22, 4]);
    }

    /// The point of the two buffers: OBS reuses its frame buffer after
    /// `raw_video` returns, while the SDK still reads the last async frame.
    #[test]
    fn a_frame_stays_intact_until_the_next_one_is_handed_over() {
        let api = Mock::default();
        let (mut y, mut uv) = ([0u8; 16], [0u8; 8]);
        {
            let mut s = sender(&api, OutFormat::Nv12, 4, 2, (30000, 1001));
            for seed in [1u8, 2, 3] {
                nv12_in(&mut y, &mut uv, seed);
                s.video(&VideoIn {
                    planes: [&y, &uv, &[]],
                    linesize: [8, 8, 0],
                    timestamp_ns: 1_000_000,
                });
                // OBS writes the next frame into the same buffer.
                y.fill(0xAB);
                uv.fill(0xAB);
            }
        }
        let calls = api.calls();
        let intact: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                Call::SendVideo { prev_intact, .. } | Call::SendFlush { prev_intact } => {
                    Some(*prev_intact)
                }
                _ => None,
            })
            .collect();
        assert_eq!(intact, vec![None, Some(true), Some(true), Some(true)]);
        assert_eq!(
            api.sent_video()[2],
            vec![3, 4, 5, 6, 11, 12, 13, 14, 103, 104, 105, 106]
        );
        assert_eq!(calls.last(), Some(&Call::SendDestroy));
        assert_eq!(api.live_senders(), 0);
    }

    #[test]
    fn the_frame_rate_and_time_are_exact() {
        let api = Mock::default();
        let (y, uv) = ([0u8; 16], [0u8; 8]);
        {
            let mut s = sender(&api, OutFormat::Nv12, 4, 2, (30000, 1001));
            s.video(&VideoIn {
                planes: [&y, &uv, &[]],
                linesize: [8, 8, 0],
                timestamp_ns: 123_456_700,
            });
        }
        assert!(api.calls().contains(&Call::SendVideo {
            fourcc: fourcc(b"NV12"),
            width: 4,
            height: 2,
            stride: 4,
            rate_n: 30000,
            rate_d: 1001,
            timestamp: 1_234_567,
            prev_intact: None,
        }));
    }

    #[test]
    fn audio_planes_go_out_one_after_the_other() {
        let api = Mock::default();
        {
            let mut s = sender(&api, OutFormat::Nv12, 4, 2, (30, 1));
            s.audio(&[&[1.0, 2.0, 3.0], &[4.0, 5.0, 6.0]], 3, 48000, 2_000);
        }
        assert!(api.calls().contains(&Call::SendAudio {
            channels: 2,
            samples: 3,
            stride: 12,
            timestamp: 20,
            data: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
        }));
    }

    #[test]
    fn connections_are_checked_once_a_second_and_reported_on_change() {
        let s = 1_000_000_000;
        let mut c = Connections::default();
        assert!(c.due(0));
        assert!(!c.due(s / 2));
        assert!(c.due(s));
        assert_eq!(c.observe(0), Some(0));
        assert_eq!(c.observe(0), None);
        assert_eq!(c.observe(2), Some(2));
    }
}
