//! The NDI API as a function table, the runtime loader, and a mock.
//!
//! The structs follow the NDI SDK's public headers (MIT licence,
//! `Processing.NDI.structs.h`, `Processing.NDI.Recv.h`); no header is copied.
//! Everything that talks to NDI goes through [`Api`], so the receive logic can
//! run against [`mock::Mock`] in tests and in CI, where no runtime is installed.

use std::ffi::{c_char, c_int, c_void, CStr};

pub type Handle = *mut c_void;

/// `NDIlib_source_t`
#[repr(C)]
pub struct Source {
    pub name: *const c_char,
    pub url: *const c_char,
}

/// `NDIlib_recv_create_v3_t`
#[repr(C)]
pub struct RecvCreate {
    pub source: Source,
    pub color_format: c_int,
    pub bandwidth: c_int,
    pub allow_video_fields: bool,
    pub recv_name: *const c_char,
}

/// `NDIlib_video_frame_v2_t`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VideoFrame {
    pub xres: c_int,
    pub yres: c_int,
    pub fourcc: u32,
    pub rate_n: c_int,
    pub rate_d: c_int,
    pub aspect: f32,
    pub format_type: c_int,
    pub timecode: i64,
    pub data: *mut u8,
    pub stride: c_int,
    pub metadata: *const c_char,
    pub timestamp: i64,
}

/// `NDIlib_audio_frame_v3_t`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioFrame {
    pub sample_rate: c_int,
    pub channels: c_int,
    pub samples: c_int,
    pub timecode: i64,
    pub fourcc: u32,
    pub data: *mut u8,
    pub stride: c_int,
    pub metadata: *const c_char,
    pub timestamp: i64,
}

impl Default for VideoFrame {
    fn default() -> Self {
        // SAFETY: plain C data; null pointers and zero are valid.
        unsafe { std::mem::zeroed() }
    }
}

impl Default for AudioFrame {
    fn default() -> Self {
        // SAFETY: plain C data; null pointers and zero are valid.
        unsafe { std::mem::zeroed() }
    }
}

/// `NDIlib_frame_type_e`
pub mod frame_type {
    pub const NONE: i32 = 0;
    pub const VIDEO: i32 = 1;
    pub const AUDIO: i32 = 2;
    pub const METADATA: i32 = 3;
    pub const ERROR: i32 = 4;
    pub const STATUS_CHANGE: i32 = 100;
    pub const SOURCE_CHANGE: i32 = 101;
}

/// `NDIlib_recv_timestamp_undefined`: the sender gave no timestamp.
pub const TIMESTAMP_UNDEFINED: i64 = i64::MAX;

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*s)
}

/// The NDI calls the plugin uses. `Lib` is the runtime, `mock::Mock` the test double.
///
/// # Safety
/// The handles are the SDK's; a caller passes only handles this `Api` created
/// and not yet destroyed, and frames this `Api` filled and not yet freed.
#[allow(clippy::missing_safety_doc)] // the rule above holds for every method
pub unsafe trait Api: Send + Sync {
    fn version(&self) -> String;
    unsafe fn recv_create(&self, desc: &RecvCreate) -> Handle;
    /// `None` disconnects.
    unsafe fn recv_connect(&self, recv: Handle, source: Option<&Source>);
    unsafe fn recv_capture(
        &self,
        recv: Handle,
        video: &mut VideoFrame,
        audio: &mut AudioFrame,
        timeout_ms: u32,
    ) -> i32;
    unsafe fn free_video(&self, recv: Handle, video: &VideoFrame);
    unsafe fn free_audio(&self, recv: Handle, audio: &AudioFrame);
    unsafe fn recv_destroy(&self, recv: Handle);
    unsafe fn recv_connections(&self, recv: Handle) -> i32;
    unsafe fn recv_set_tally(&self, recv: Handle, program: bool, preview: bool) -> bool;
    /// The sender's web control URL, if it publishes one.
    unsafe fn recv_web_control(&self, recv: Handle) -> Option<String>;
    unsafe fn find_create(&self, show_local: bool) -> Handle;
    /// `true` when the list changed within `timeout_ms`.
    unsafe fn find_wait(&self, find: Handle, timeout_ms: u32) -> bool;
    /// The names of the sources found so far.
    unsafe fn find_sources(&self, find: Handle) -> Vec<String>;
    /// The address (`host:port`) of the source `name`, if this finder has it.
    unsafe fn find_source_url(&self, find: Handle, name: &str) -> Option<String>;
    unsafe fn find_destroy(&self, find: Handle);
    unsafe fn send_create(&self, name: &CStr, groups: Option<&CStr>) -> Handle;
    unsafe fn send_destroy(&self, send: Handle);
    /// The SDK may read `frame`'s data until the next call; `None` waits
    /// until it is done with the last frame.
    unsafe fn send_video_async(&self, send: Handle, frame: Option<&VideoFrame>);
    /// The SDK copies the data before it returns.
    unsafe fn send_audio(&self, send: Handle, frame: &AudioFrame);
    unsafe fn send_connections(&self, send: Handle, timeout_ms: u32) -> i32;
}

/// `NDIlib_send_create_t`
#[repr(C)]
pub struct SendCreate {
    pub name: *const c_char,
    pub groups: *const c_char,
    pub clock_video: bool,
    pub clock_audio: bool,
}

/// `NDIlib_tally_t`
#[repr(C)]
pub struct TallyFrame {
    pub on_program: bool,
    pub on_preview: bool,
}

/// `NDIlib_find_create_t`
#[repr(C)]
pub struct FindCreate {
    pub show_local_sources: bool,
    pub groups: *const c_char,
    pub extra_ips: *const c_char,
}

/// The operating systems the loader knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// The NDI 6 runtime's file name on Windows (x64 only).
pub const WINDOWS_RUNTIME: &str = "Processing.NDI.Lib.x64.dll";

/// The NDI 6 runtime major version the struct layouts here follow.
pub const RUNTIME_MAJOR: u32 = 6;

/// Where to look for the runtime, in order. `own_dir` is the plugin's own
/// folder (a runtime shipped with the plugin comes first); `env_dir` is
/// `NDI_RUNTIME_DIR_V6`; `list_dir` gives the file names in a directory
/// (empty when it does not exist).
///
/// After the plugin's folder the search follows DistroAV's (`load_ndilib`):
/// the environment directory, then `/usr/lib`, `/usr/lib64` and
/// `/usr/local/lib`. On Linux it takes only `libndi.so.6*` (DistroAV takes the
/// highest major, which could load a runtime whose structs differ), newest
/// name first, and ends with the bare `libndi.so.6` for the loader's own
/// search path. On macOS it takes `libndi.dylib` in each directory, then the
/// bare name. On Windows it takes `Processing.NDI.Lib.x64.dll` in the plugin's
/// folder and in `NDI_RUNTIME_DIR_V6` (the NDI runtime installer sets it), and
/// never a bare name: that would search the working directory and `PATH`.
pub fn candidates(
    os: Os,
    own_dir: Option<&str>,
    env_dir: Option<&str>,
    list_dir: &dyn Fn(&str) -> Vec<String>,
) -> Vec<String> {
    let own_dir = own_dir.filter(|d| !d.is_empty());
    let env_dir = env_dir.filter(|d| !d.is_empty());
    if os == Os::Windows {
        return [own_dir, env_dir]
            .into_iter()
            .flatten()
            .map(|d| d.trim_end_matches(['\\', '/']))
            .filter(|d| {
                Some(*d) == env_dir.map(|e| e.trim_end_matches(['\\', '/']))
                    || list_dir(d)
                        .iter()
                        .any(|f| f.eq_ignore_ascii_case(WINDOWS_RUNTIME))
            })
            .map(|d| format!("{d}\\{WINDOWS_RUNTIME}"))
            .collect();
    }
    let mut dirs: Vec<&str> = own_dir.into_iter().chain(env_dir).collect();
    dirs.extend(["/usr/lib", "/usr/lib64", "/usr/local/lib"]);
    let mut out = Vec::new();
    for dir in dirs {
        let dir = dir.trim_end_matches('/');
        match os {
            Os::Linux => {
                // libndi.so.6, libndi.so.6.3.2, ...: major 6 only, newest first.
                let mut found: Vec<(Vec<u32>, String)> = list_dir(dir)
                    .into_iter()
                    .filter_map(|name| {
                        let version: Vec<u32> = name
                            .strip_prefix("libndi.so.")?
                            .split('.')
                            .map(|p| p.parse().ok())
                            .collect::<Option<_>>()?;
                        (version.first() == Some(&RUNTIME_MAJOR)).then_some((version, name))
                    })
                    .collect();
                found.sort_by(|a, b| b.0.cmp(&a.0));
                out.extend(found.into_iter().map(|(_, name)| format!("{dir}/{name}")));
            }
            Os::MacOs => {
                if list_dir(dir).iter().any(|f| f == "libndi.dylib") || env_dir == Some(dir) {
                    out.push(format!("{dir}/libndi.dylib"));
                }
            }
            Os::Windows => unreachable!("handled above"),
        }
    }
    out.push(match os {
        Os::Linux => format!("libndi.so.{RUNTIME_MAJOR}"),
        _ => "libndi.dylib".to_string(),
    });
    out
}

/// Opening a shared library and looking up a symbol, per platform.
#[cfg(unix)]
mod dl {
    use std::ffi::{c_char, c_int, c_void, CStr, CString};

    extern "C" {
        fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    pub unsafe fn open(path: &str) -> Result<*mut c_void, String> {
        let cpath = CString::new(path).map_err(|_| "NUL in path".to_string())?;
        let lib = dlopen(cpath.as_ptr(), 2 /* RTLD_NOW */);
        if lib.is_null() {
            Err("cannot open".into())
        } else {
            Ok(lib)
        }
    }

    pub unsafe fn sym(lib: *mut c_void, name: &CStr) -> *mut c_void {
        dlsym(lib, name.as_ptr())
    }
}

#[cfg(windows)]
mod dl {
    use std::ffi::{c_char, c_void, CStr};

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn GetLastError() -> u32;
    }

    /// `LOAD_WITH_ALTERED_SEARCH_PATH`: the runtime's own dependencies are
    /// looked up next to it, not next to obs64.exe.
    const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;

    pub unsafe fn open(path: &str) -> Result<*mut c_void, String> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let lib = LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            LOAD_WITH_ALTERED_SEARCH_PATH,
        );
        if lib.is_null() {
            Err(format!("cannot open (error {})", GetLastError()))
        } else {
            Ok(lib)
        }
    }

    pub unsafe fn sym(lib: *mut c_void, name: &CStr) -> *mut c_void {
        GetProcAddress(lib, name.as_ptr())
    }
}

type RecvCreateFn = unsafe extern "C" fn(*const RecvCreate) -> Handle;
type RecvConnectFn = unsafe extern "C" fn(Handle, *const Source);
type RecvCaptureFn =
    unsafe extern "C" fn(Handle, *mut VideoFrame, *mut AudioFrame, *mut c_void, u32) -> c_int;
type FreeVideoFn = unsafe extern "C" fn(Handle, *const VideoFrame);
type FreeAudioFn = unsafe extern "C" fn(Handle, *const AudioFrame);
type HandleFn = unsafe extern "C" fn(Handle);
type ConnectionsFn = unsafe extern "C" fn(Handle) -> c_int;
type VersionFn = unsafe extern "C" fn() -> *const c_char;
type BoolFn = unsafe extern "C" fn() -> bool;
type VoidFn = unsafe extern "C" fn();
type SetTallyFn = unsafe extern "C" fn(Handle, *const TallyFrame) -> bool;
type WebControlFn = unsafe extern "C" fn(Handle) -> *const c_char;
type FreeStringFn = unsafe extern "C" fn(Handle, *const c_char);
type SendCreateFn = unsafe extern "C" fn(*const SendCreate) -> Handle;
type SendVideoFn = unsafe extern "C" fn(Handle, *const VideoFrame);
type SendAudioFn = unsafe extern "C" fn(Handle, *const AudioFrame);
type SendConnectionsFn = unsafe extern "C" fn(Handle, u32) -> c_int;
type FindCreateFn = unsafe extern "C" fn(*const FindCreate) -> Handle;
type FindWaitFn = unsafe extern "C" fn(Handle, u32) -> bool;
type FindSourcesFn = unsafe extern "C" fn(Handle, *mut u32) -> *const Source;

/// The NDI runtime, loaded with `dlopen` (`LoadLibraryExW` on Windows).
pub struct Lib {
    pub path: String,
    version: VersionFn,
    destroy: VoidFn,
    recv_create: RecvCreateFn,
    recv_connect: RecvConnectFn,
    recv_capture: RecvCaptureFn,
    free_video: FreeVideoFn,
    free_audio: FreeAudioFn,
    recv_destroy: HandleFn,
    recv_connections: ConnectionsFn,
    recv_set_tally: SetTallyFn,
    recv_web_control: WebControlFn,
    recv_free_string: FreeStringFn,
    find_create: FindCreateFn,
    find_wait: FindWaitFn,
    find_sources: FindSourcesFn,
    find_destroy: HandleFn,
    send_create: SendCreateFn,
    send_destroy: HandleFn,
    send_video_async: SendVideoFn,
    send_audio: SendAudioFn,
    send_connections: SendConnectionsFn,
}

impl Lib {
    /// Load the first candidate that opens and has every symbol, and initialize it.
    /// The error lists what was tried.
    pub fn load(candidates: &[String]) -> Result<Lib, String> {
        let mut tried = Vec::new();
        for path in candidates {
            // SAFETY: dlopen/dlsym on a path; the symbols are cast to the
            // signatures in the SDK headers.
            match unsafe { Self::open(path) } {
                Ok(lib) => return Ok(lib),
                Err(e) => tried.push(format!("{path}: {e}")),
            }
        }
        Err(if tried.is_empty() {
            "no candidate".into()
        } else {
            tried.join("; ")
        })
    }

    unsafe fn open(path: &str) -> Result<Lib, String> {
        let lib = dl::open(path)?;
        let sym = |name: &CStr| -> Result<*mut c_void, String> {
            let p = dl::sym(lib, name);
            if p.is_null() {
                Err(format!("missing {}", name.to_string_lossy()))
            } else {
                Ok(p)
            }
        };
        let init: BoolFn = std::mem::transmute::<*mut c_void, BoolFn>(sym(c"NDIlib_initialize")?);
        let lib = Lib {
            path: path.to_string(),
            version: std::mem::transmute::<*mut c_void, VersionFn>(sym(c"NDIlib_version")?),
            destroy: std::mem::transmute::<*mut c_void, VoidFn>(sym(c"NDIlib_destroy")?),
            recv_create: std::mem::transmute::<*mut c_void, RecvCreateFn>(sym(
                c"NDIlib_recv_create_v3",
            )?),
            recv_connect: std::mem::transmute::<*mut c_void, RecvConnectFn>(sym(
                c"NDIlib_recv_connect",
            )?),
            recv_capture: std::mem::transmute::<*mut c_void, RecvCaptureFn>(sym(
                c"NDIlib_recv_capture_v3",
            )?),
            free_video: std::mem::transmute::<*mut c_void, FreeVideoFn>(sym(
                c"NDIlib_recv_free_video_v2",
            )?),
            free_audio: std::mem::transmute::<*mut c_void, FreeAudioFn>(sym(
                c"NDIlib_recv_free_audio_v3",
            )?),
            recv_destroy: std::mem::transmute::<*mut c_void, HandleFn>(sym(
                c"NDIlib_recv_destroy",
            )?),
            recv_connections: std::mem::transmute::<*mut c_void, ConnectionsFn>(sym(
                c"NDIlib_recv_get_no_connections",
            )?),
            recv_set_tally: std::mem::transmute::<*mut c_void, SetTallyFn>(sym(
                c"NDIlib_recv_set_tally",
            )?),
            recv_web_control: std::mem::transmute::<*mut c_void, WebControlFn>(sym(
                c"NDIlib_recv_get_web_control",
            )?),
            recv_free_string: std::mem::transmute::<*mut c_void, FreeStringFn>(sym(
                c"NDIlib_recv_free_string",
            )?),
            find_create: std::mem::transmute::<*mut c_void, FindCreateFn>(sym(
                c"NDIlib_find_create_v2",
            )?),
            find_wait: std::mem::transmute::<*mut c_void, FindWaitFn>(sym(
                c"NDIlib_find_wait_for_sources",
            )?),
            find_sources: std::mem::transmute::<*mut c_void, FindSourcesFn>(sym(
                c"NDIlib_find_get_current_sources",
            )?),
            find_destroy: std::mem::transmute::<*mut c_void, HandleFn>(sym(
                c"NDIlib_find_destroy",
            )?),
            send_create: std::mem::transmute::<*mut c_void, SendCreateFn>(sym(
                c"NDIlib_send_create",
            )?),
            send_destroy: std::mem::transmute::<*mut c_void, HandleFn>(sym(
                c"NDIlib_send_destroy",
            )?),
            send_video_async: std::mem::transmute::<*mut c_void, SendVideoFn>(sym(
                c"NDIlib_send_send_video_async_v2",
            )?),
            send_audio: std::mem::transmute::<*mut c_void, SendAudioFn>(sym(
                c"NDIlib_send_send_audio_v3",
            )?),
            send_connections: std::mem::transmute::<*mut c_void, SendConnectionsFn>(sym(
                c"NDIlib_send_get_no_connections",
            )?),
        };
        if !init() {
            return Err("NDIlib_initialize failed (CPU not supported?)".into());
        }
        Ok(lib)
    }

    /// `NDIlib_destroy`, at module unload.
    ///
    /// # Safety
    /// No receiver of this runtime may be alive.
    pub unsafe fn shutdown(&self) {
        (self.destroy)()
    }
}

// SAFETY: the SDK functions are thread-safe per the SDK documentation; the
// table itself is immutable.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

unsafe impl Api for Lib {
    fn version(&self) -> String {
        // SAFETY: NDIlib_version returns a static string.
        unsafe { CStr::from_ptr((self.version)()) }
            .to_string_lossy()
            .into_owned()
    }
    unsafe fn recv_create(&self, desc: &RecvCreate) -> Handle {
        (self.recv_create)(desc)
    }
    unsafe fn recv_connect(&self, recv: Handle, source: Option<&Source>) {
        (self.recv_connect)(
            recv,
            source.map_or(std::ptr::null(), |s| s as *const Source),
        )
    }
    unsafe fn recv_capture(
        &self,
        recv: Handle,
        video: &mut VideoFrame,
        audio: &mut AudioFrame,
        timeout_ms: u32,
    ) -> i32 {
        (self.recv_capture)(recv, video, audio, std::ptr::null_mut(), timeout_ms)
    }
    unsafe fn free_video(&self, recv: Handle, video: &VideoFrame) {
        (self.free_video)(recv, video)
    }
    unsafe fn free_audio(&self, recv: Handle, audio: &AudioFrame) {
        (self.free_audio)(recv, audio)
    }
    unsafe fn recv_destroy(&self, recv: Handle) {
        (self.recv_destroy)(recv)
    }
    unsafe fn recv_connections(&self, recv: Handle) -> i32 {
        (self.recv_connections)(recv)
    }
    unsafe fn recv_set_tally(&self, recv: Handle, program: bool, preview: bool) -> bool {
        (self.recv_set_tally)(
            recv,
            &TallyFrame {
                on_program: program,
                on_preview: preview,
            },
        )
    }
    unsafe fn recv_web_control(&self, recv: Handle) -> Option<String> {
        let p = (self.recv_web_control)(recv);
        if p.is_null() {
            return None;
        }
        let url = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
        (self.recv_free_string)(recv, p);
        (!url.is_empty()).then_some(url)
    }
    unsafe fn find_create(&self, show_local: bool) -> Handle {
        let desc = FindCreate {
            show_local_sources: show_local,
            groups: std::ptr::null(),
            extra_ips: std::ptr::null(),
        };
        (self.find_create)(&desc)
    }
    unsafe fn find_wait(&self, find: Handle, timeout_ms: u32) -> bool {
        (self.find_wait)(find, timeout_ms)
    }
    unsafe fn find_sources(&self, find: Handle) -> Vec<String> {
        let mut n = 0u32;
        let list = (self.find_sources)(find, &mut n);
        if list.is_null() {
            return Vec::new();
        }
        // The array stays valid until the next call on this finder.
        std::slice::from_raw_parts(list, n as usize)
            .iter()
            .filter(|s| !s.name.is_null())
            .map(|s| CStr::from_ptr(s.name).to_string_lossy().into_owned())
            .collect()
    }
    unsafe fn find_source_url(&self, find: Handle, name: &str) -> Option<String> {
        let mut n = 0u32;
        let list = (self.find_sources)(find, &mut n);
        if list.is_null() {
            return None;
        }
        std::slice::from_raw_parts(list, n as usize)
            .iter()
            .filter(|s| !s.name.is_null() && !s.url.is_null())
            .find(|s| CStr::from_ptr(s.name).to_bytes() == name.as_bytes())
            .map(|s| CStr::from_ptr(s.url).to_string_lossy().into_owned())
            .filter(|u| !u.is_empty())
    }
    unsafe fn find_destroy(&self, find: Handle) {
        (self.find_destroy)(find)
    }
    unsafe fn send_create(&self, name: &CStr, groups: Option<&CStr>) -> Handle {
        let desc = SendCreate {
            name: name.as_ptr(),
            groups: groups.map_or(std::ptr::null(), |g| g.as_ptr()),
            clock_video: false,
            clock_audio: false,
        };
        (self.send_create)(&desc)
    }
    unsafe fn send_destroy(&self, send: Handle) {
        (self.send_destroy)(send)
    }
    unsafe fn send_video_async(&self, send: Handle, frame: Option<&VideoFrame>) {
        (self.send_video_async)(
            send,
            frame.map_or(std::ptr::null(), |f| f as *const VideoFrame),
        )
    }
    unsafe fn send_audio(&self, send: Handle, frame: &AudioFrame) {
        (self.send_audio)(send, frame)
    }
    unsafe fn send_connections(&self, send: Handle, timeout_ms: u32) -> i32 {
        (self.send_connections)(send, timeout_ms)
    }
}

/// A scripted NDI runtime for tests: `recv_capture` plays the script one step
/// per call, and every call is recorded.
pub mod mock {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq)]
    pub enum Step {
        Video {
            fourcc: u32,
            width: i32,
            height: i32,
            timestamp: i64,
            timecode: i64,
        },
        Audio {
            fourcc: u32,
            channels: i32,
            samples: i32,
            rate: i32,
            timestamp: i64,
            timecode: i64,
        },
        /// `recv_capture` times out with no frame.
        Nothing,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum Call {
        Create {
            name: String,
            bandwidth: i32,
            color_format: i32,
        },
        Connect(Option<String>),
        FreeVideo,
        FreeAudio,
        Destroy,
        FindCreate,
        FindDestroy,
        Tally {
            program: bool,
            preview: bool,
        },
        SendCreate {
            name: String,
            groups: Option<String>,
        },
        /// `prev_intact`: whether the previous async frame's data was still
        /// what it was when it was handed over (`None`: no previous frame).
        SendVideo {
            fourcc: u32,
            width: i32,
            height: i32,
            stride: i32,
            rate_n: i32,
            rate_d: i32,
            timestamp: i64,
            prev_intact: Option<bool>,
        },
        SendFlush {
            prev_intact: Option<bool>,
        },
        SendAudio {
            channels: i32,
            samples: i32,
            stride: i32,
            timestamp: i64,
            data: Vec<f32>,
        },
        SendDestroy,
    }

    #[derive(Default)]
    struct State {
        calls: Vec<Call>,
        script: VecDeque<Step>,
        video: Vec<u8>,
        audio: Vec<f32>,
        receivers: usize,
        connections: i32,
        web_control: Option<String>,
        /// Source addresses a finder reports, by name.
        urls: std::collections::HashMap<String, String>,
        /// What the network shows after each `find_wait`: round 0 before the first wait.
        discovery: Vec<Vec<String>>,
        round: usize,
        finders: usize,
        /// The async frame the SDK may still read: its data and length, and
        /// a copy of the bytes when it was handed over.
        pending: Option<(*const u8, usize, Vec<u8>)>,
        senders: usize,
        /// The bytes of each video frame, as the SDK saw them when handed over.
        sent_video: Vec<Vec<u8>>,
    }

    // SAFETY: `pending` is only read under the mutex, while the sender keeps
    // the buffer alive (its buffers live until it is dropped).
    unsafe impl Send for State {}

    #[derive(Default)]
    pub struct Mock {
        state: Mutex<State>,
    }

    impl Mock {
        pub fn new(script: impl IntoIterator<Item = Step>) -> Mock {
            let m = Mock::default();
            m.state.lock().unwrap().script = script.into_iter().collect();
            m
        }
        pub fn push(&self, step: Step) {
            self.state.lock().unwrap().script.push_back(step);
        }
        pub fn calls(&self) -> Vec<Call> {
            self.state.lock().unwrap().calls.clone()
        }
        /// Receivers created and not destroyed.
        pub fn live_receivers(&self) -> usize {
            self.state.lock().unwrap().receivers
        }
        pub fn set_connections(&self, n: i32) {
            self.state.lock().unwrap().connections = n;
        }
        pub fn set_web_control(&self, url: Option<&str>) {
            self.state.lock().unwrap().web_control = url.map(String::from);
        }
        pub fn set_url(&self, name: &str, url: &str) {
            self.state
                .lock()
                .unwrap()
                .urls
                .insert(name.into(), url.into());
        }
        /// Sources seen by a finder: `rounds[i]` after the i-th wait (round 0 before any).
        pub fn set_discovery(&self, rounds: Vec<Vec<&str>>) {
            self.state.lock().unwrap().discovery = rounds
                .into_iter()
                .map(|r| r.into_iter().map(String::from).collect())
                .collect();
        }
        /// Finders created and not destroyed.
        pub fn live_finders(&self) -> usize {
            self.state.lock().unwrap().finders
        }
        pub fn live_senders(&self) -> usize {
            self.state.lock().unwrap().senders
        }
        /// The data of each sent video frame, as handed over.
        pub fn sent_video(&self) -> Vec<Vec<u8>> {
            self.state.lock().unwrap().sent_video.clone()
        }
    }

    unsafe fn text(p: *const c_char) -> Option<String> {
        (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
    }

    unsafe impl Api for Mock {
        fn version(&self) -> String {
            "NDI SDK MOCK 6.0.0".into()
        }
        unsafe fn recv_create(&self, desc: &RecvCreate) -> Handle {
            let mut s = self.state.lock().unwrap();
            s.receivers += 1;
            s.calls.push(Call::Create {
                name: text(desc.source.name).unwrap_or_default(),
                bandwidth: desc.bandwidth,
                color_format: desc.color_format,
            });
            // Any non-null value: the mock never dereferences it.
            std::ptr::NonNull::<c_void>::dangling().as_ptr()
        }
        unsafe fn recv_connect(&self, _recv: Handle, source: Option<&Source>) {
            let name = source.and_then(|s| text(s.name));
            self.state.lock().unwrap().calls.push(Call::Connect(name));
        }
        unsafe fn recv_capture(
            &self,
            _recv: Handle,
            video: &mut VideoFrame,
            audio: &mut AudioFrame,
            _timeout_ms: u32,
        ) -> i32 {
            let mut s = self.state.lock().unwrap();
            match s.script.pop_front().unwrap_or(Step::Nothing) {
                Step::Video {
                    fourcc,
                    width,
                    height,
                    timestamp,
                    timecode,
                } => {
                    let stride = width * 4;
                    s.video.resize((stride * height) as usize, 0x80);
                    *video = VideoFrame {
                        xres: width,
                        yres: height,
                        fourcc,
                        rate_n: 30000,
                        rate_d: 1001,
                        aspect: 0.0,
                        format_type: 1, // progressive
                        timecode,
                        data: s.video.as_mut_ptr(),
                        stride,
                        metadata: std::ptr::null(),
                        timestamp,
                    };
                    frame_type::VIDEO
                }
                Step::Audio {
                    fourcc,
                    channels,
                    samples,
                    rate,
                    timestamp,
                    timecode,
                } => {
                    s.audio.resize((channels * samples).max(0) as usize, 0.25);
                    *audio = AudioFrame {
                        sample_rate: rate,
                        channels,
                        samples,
                        timecode,
                        fourcc,
                        data: s.audio.as_mut_ptr().cast(),
                        stride: samples * 4,
                        metadata: std::ptr::null(),
                        timestamp,
                    };
                    frame_type::AUDIO
                }
                Step::Nothing => frame_type::NONE,
            }
        }
        unsafe fn free_video(&self, _recv: Handle, _video: &VideoFrame) {
            self.state.lock().unwrap().calls.push(Call::FreeVideo);
        }
        unsafe fn free_audio(&self, _recv: Handle, _audio: &AudioFrame) {
            self.state.lock().unwrap().calls.push(Call::FreeAudio);
        }
        unsafe fn recv_destroy(&self, _recv: Handle) {
            let mut s = self.state.lock().unwrap();
            s.receivers -= 1;
            s.calls.push(Call::Destroy);
        }
        unsafe fn recv_connections(&self, _recv: Handle) -> i32 {
            self.state.lock().unwrap().connections
        }
        unsafe fn recv_set_tally(&self, _recv: Handle, program: bool, preview: bool) -> bool {
            self.state
                .lock()
                .unwrap()
                .calls
                .push(Call::Tally { program, preview });
            true
        }
        unsafe fn recv_web_control(&self, _recv: Handle) -> Option<String> {
            self.state.lock().unwrap().web_control.clone()
        }
        unsafe fn find_create(&self, _show_local: bool) -> Handle {
            let mut s = self.state.lock().unwrap();
            s.finders += 1;
            s.round = 0;
            s.calls.push(Call::FindCreate);
            std::ptr::NonNull::<c_void>::dangling().as_ptr()
        }
        unsafe fn find_wait(&self, _find: Handle, _timeout_ms: u32) -> bool {
            let mut s = self.state.lock().unwrap();
            let before = s.discovery.get(s.round).cloned().unwrap_or_default();
            if s.round + 1 < s.discovery.len() {
                s.round += 1;
            }
            s.discovery.get(s.round).cloned().unwrap_or_default() != before
        }
        unsafe fn find_sources(&self, _find: Handle) -> Vec<String> {
            let s = self.state.lock().unwrap();
            s.discovery.get(s.round).cloned().unwrap_or_default()
        }
        unsafe fn find_source_url(&self, _find: Handle, name: &str) -> Option<String> {
            self.state.lock().unwrap().urls.get(name).cloned()
        }
        unsafe fn find_destroy(&self, _find: Handle) {
            let mut s = self.state.lock().unwrap();
            s.finders -= 1;
            s.calls.push(Call::FindDestroy);
        }
        unsafe fn send_create(&self, name: &CStr, groups: Option<&CStr>) -> Handle {
            let mut s = self.state.lock().unwrap();
            s.senders += 1;
            s.calls.push(Call::SendCreate {
                name: name.to_string_lossy().into_owned(),
                groups: groups.map(|g| g.to_string_lossy().into_owned()),
            });
            std::ptr::NonNull::<c_void>::dangling().as_ptr()
        }
        unsafe fn send_destroy(&self, _send: Handle) {
            let mut s = self.state.lock().unwrap();
            s.senders -= 1;
            s.calls.push(Call::SendDestroy);
        }
        unsafe fn send_video_async(&self, _send: Handle, frame: Option<&VideoFrame>) {
            let mut s = self.state.lock().unwrap();
            // The SDK reads the previous frame up to now: was it still intact?
            let prev_intact = s
                .pending
                .take()
                .map(|(p, len, copy)| std::slice::from_raw_parts(p, len) == copy.as_slice());
            match frame {
                Some(f) => {
                    let len = video_len(f);
                    let bytes = std::slice::from_raw_parts(f.data, len).to_vec();
                    s.sent_video.push(bytes.clone());
                    s.pending = Some((f.data, len, bytes));
                    s.calls.push(Call::SendVideo {
                        fourcc: f.fourcc,
                        width: f.xres,
                        height: f.yres,
                        stride: f.stride,
                        rate_n: f.rate_n,
                        rate_d: f.rate_d,
                        timestamp: f.timestamp,
                        prev_intact,
                    });
                }
                None => s.calls.push(Call::SendFlush { prev_intact }),
            }
        }
        unsafe fn send_audio(&self, _send: Handle, f: &AudioFrame) {
            let n = (f.stride / 4 * f.channels).max(0) as usize;
            let data = std::slice::from_raw_parts(f.data.cast::<f32>(), n).to_vec();
            self.state.lock().unwrap().calls.push(Call::SendAudio {
                channels: f.channels,
                samples: f.samples,
                stride: f.stride,
                timestamp: f.timestamp,
                data,
            });
        }
        unsafe fn send_connections(&self, _send: Handle, _timeout_ms: u32) -> i32 {
            self.state.lock().unwrap().connections
        }
    }

    /// The bytes an NDI video frame covers, by its FourCC.
    fn video_len(f: &VideoFrame) -> usize {
        let (stride, h) = (f.stride.max(0) as usize, f.yres.max(0) as usize);
        if f.fourcc == fourcc(b"NV12") || f.fourcc == fourcc(b"I420") {
            stride * h * 3 / 2
        } else {
            stride * h
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn dirs(
        listing: &'static [(&'static str, &'static [&'static str])],
    ) -> impl Fn(&str) -> Vec<String> {
        move |dir: &str| {
            listing
                .iter()
                .find(|(d, _)| *d == dir)
                .map(|(_, files)| files.iter().map(|f| f.to_string()).collect())
                .unwrap_or_default()
        }
    }

    #[test]
    fn linux_takes_only_major_6_newest_first_then_the_bare_name() {
        let list = dirs(&[
            (
                "/opt/ndi",
                &["libndi.so.6", "libndi.so.6.3.2", "libndi.so.5"],
            ),
            (
                "/usr/local/lib",
                &["libndi.so.7", "libndi.so.6.2.0", "libfoo.so.6"],
            ),
        ]);
        assert_eq!(
            candidates(Os::Linux, None, Some("/opt/ndi"), &list),
            vec![
                "/opt/ndi/libndi.so.6.3.2",
                "/opt/ndi/libndi.so.6",
                "/usr/local/lib/libndi.so.6.2.0",
                "libndi.so.6",
            ]
        );
    }

    #[test]
    fn linux_without_a_runtime_still_tries_the_loader_search() {
        assert_eq!(
            candidates(Os::Linux, None, None, &dirs(&[])),
            vec!["libndi.so.6"]
        );
    }

    #[test]
    fn macos_takes_the_dylib_in_each_directory_then_the_bare_name() {
        let list = dirs(&[("/usr/local/lib", &["libndi.dylib"])]);
        assert_eq!(
            candidates(Os::MacOs, None, Some("/Library/NDI"), &list),
            vec![
                "/Library/NDI/libndi.dylib",
                "/usr/local/lib/libndi.dylib",
                "libndi.dylib"
            ]
        );
    }

    #[test]
    fn a_runtime_in_the_plugin_folder_comes_first() {
        let list = dirs(&[
            ("/opt/obs/plugins/bin", &["libndi.so.6.3.2"]),
            ("/usr/lib", &["libndi.so.6.2.0"]),
        ]);
        assert_eq!(
            candidates(Os::Linux, Some("/opt/obs/plugins/bin"), None, &list),
            vec![
                "/opt/obs/plugins/bin/libndi.so.6.3.2",
                "/usr/lib/libndi.so.6.2.0",
                "libndi.so.6",
            ]
        );
        let list = dirs(&[("/Plugins/obs-avip.plugin/Contents/MacOS", &["libndi.dylib"])]);
        assert_eq!(
            candidates(
                Os::MacOs,
                Some("/Plugins/obs-avip.plugin/Contents/MacOS"),
                None,
                &list
            )[0],
            "/Plugins/obs-avip.plugin/Contents/MacOS/libndi.dylib"
        );
    }

    #[test]
    fn windows_takes_the_plugin_folder_then_the_runtime_folder_never_a_bare_name() {
        const OWN: &str = r"C:\ProgramData\obs-studio\plugins\obs-avip\bin\64bit";
        const LISTING: &[(&str, &[&str])] =
            &[(OWN, &["obs-avip.dll", "processing.ndi.lib.x64.dll"])];
        let own = OWN;
        let env = r"C:\Program Files\NDI\NDI 6 Runtime\v6\";
        let list = dirs(LISTING);
        assert_eq!(
            candidates(Os::Windows, Some(own), Some(env), &list),
            vec![
                format!(r"{own}\Processing.NDI.Lib.x64.dll"),
                r"C:\Program Files\NDI\NDI 6 Runtime\v6\Processing.NDI.Lib.x64.dll".to_string(),
            ]
        );
        // No runtime next to the plugin: only the runtime folder.
        assert_eq!(
            candidates(Os::Windows, Some(own), Some(env), &dirs(&[])).len(),
            1
        );
        // No runtime anywhere: nothing to try, not a search of PATH.
        assert!(candidates(Os::Windows, Some(own), None, &dirs(&[])).is_empty());
    }

    #[test]
    fn a_missing_runtime_says_what_was_tried() {
        let err = Lib::load(&["/nonexistent/libndi.so.6".to_string()])
            .err()
            .unwrap();
        assert!(
            err.contains("/nonexistent/libndi.so.6: cannot open"),
            "{err}"
        );
    }

    #[test]
    fn the_mock_plays_its_script_and_records_calls() {
        let m = mock::Mock::new([
            mock::Step::Video {
                fourcc: fourcc(b"UYVY"),
                width: 4,
                height: 2,
                timestamp: 10,
                timecode: 20,
            },
            mock::Step::Nothing,
        ]);
        let name = CString::new("CAM (1)").unwrap();
        let desc = RecvCreate {
            source: Source {
                name: name.as_ptr(),
                url: std::ptr::null(),
            },
            color_format: 1,
            bandwidth: 100,
            allow_video_fields: false,
            recv_name: std::ptr::null(),
        };
        unsafe {
            let r = m.recv_create(&desc);
            let (mut v, mut a) = (VideoFrame::default(), AudioFrame::default());
            assert_eq!(m.recv_capture(r, &mut v, &mut a, 0), frame_type::VIDEO);
            assert_eq!((v.xres, v.yres, v.stride, v.timecode), (4, 2, 16, 20));
            m.free_video(r, &v);
            assert_eq!(m.recv_capture(r, &mut v, &mut a, 0), frame_type::NONE);
            m.recv_connect(r, None);
            m.recv_destroy(r);
        }
        assert_eq!(m.live_receivers(), 0);
        assert_eq!(
            m.calls(),
            vec![
                mock::Call::Create {
                    name: "CAM (1)".into(),
                    bandwidth: 100,
                    color_format: 1
                },
                mock::Call::FreeVideo,
                mock::Call::Connect(None),
                mock::Call::Destroy,
            ]
        );
    }
}
