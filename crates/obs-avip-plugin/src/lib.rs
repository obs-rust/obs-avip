//! obs-avip: an NDI® source and output for OBS, a drop-in for DistroAV: DistroAV's
//! ids (`ndi_source`, `ndi_output`), keys and defaults. It registers in
//! `obs_module_post_load`, only when no other module provides those ids
//! (`obs_avip::module`).
//! libndi is loaded at runtime. No headers are copied; the structs follow the SDK's public MIT headers.
//! Settings: DistroAV's keys and defaults (`obs_avip::settings`), plus `arrival_ts` (bool, measurement only).
#![allow(non_snake_case, non_camel_case_types, clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;

use obs_avip::convert;
use obs_avip::finder::{self, Cache};
use obs_avip::module;
use obs_avip::ndi::{self, Api, Lib, Os, RecvCreate};
use obs_avip::resume::{self, Change, Reason, Receiver};
use obs_avip::settings::keys;
use obs_avip::settings::{
    self, Bandwidth, ColorFormat, ColorSpace, SourceSettings, Value, YuvRange,
};
use obs_avip::tally::{self, Event, TallyConfig};
use obs_avip::visibility;
use std::cell::RefCell;

mod network_ui;
mod output;
mod settings_ui;
use std::sync::atomic::AtomicU8;
use std::sync::Mutex;

#[cfg_attr(windows, link(name = "obs", kind = "raw-dylib"))]
extern "C" {
    fn blog(level: c_int, fmt: *const c_char, ...);
    fn obs_get_module_binary_path(module: *mut c_void) -> *const c_char;
    fn obs_obj_get_data(obj: *mut c_void) -> *mut c_void;
    fn obs_source_get_settings(source: *mut c_void) -> *mut c_void;
    fn obs_source_update(source: *mut c_void, settings: *mut c_void);
    fn obs_data_create() -> *mut c_void;
    fn obs_data_release(data: *mut c_void);
    fn os_gettime_ns() -> u64;
    fn obs_register_source_s(info: *const SourceInfo, size: usize);
    fn obs_source_get_display_name(id: *const c_char) -> *const c_char;
    fn obs_output_get_display_name(id: *const c_char) -> *const c_char;
    fn obs_source_output_video(source: *mut c_void, frame: *const SourceFrame);
    fn obs_source_output_audio(source: *mut c_void, audio: *const SourceAudio);
    fn obs_source_set_async_unbuffered(source: *mut c_void, unbuffered: bool);
    fn obs_data_get_string(data: *mut c_void, name: *const c_char) -> *const c_char;
    fn obs_data_get_bool(data: *mut c_void, name: *const c_char) -> bool;
    fn obs_data_get_int(data: *mut c_void, name: *const c_char) -> i64;
    fn obs_data_set_int(data: *mut c_void, name: *const c_char, val: i64);
    fn obs_data_set_bool(data: *mut c_void, name: *const c_char, val: bool);
    fn obs_data_set_string(data: *mut c_void, name: *const c_char, val: *const c_char);
    fn obs_data_set_default_int(data: *mut c_void, name: *const c_char, val: i64);
    fn obs_data_set_default_bool(data: *mut c_void, name: *const c_char, val: bool);
    fn obs_data_set_default_string(data: *mut c_void, name: *const c_char, val: *const c_char);
    fn obs_source_set_audio_active(source: *mut c_void, active: bool);
    fn obs_source_update_properties(source: *mut c_void);
    fn obs_source_active(source: *mut c_void) -> bool;
    fn obs_source_showing(source: *mut c_void) -> bool;
    fn config_get_bool(config: *mut c_void, section: *const c_char, name: *const c_char) -> bool;
    fn config_has_user_value(
        config: *mut c_void,
        section: *const c_char,
        name: *const c_char,
    ) -> bool;
    fn obs_properties_create() -> *mut c_void;
    fn obs_properties_add_list(
        props: *mut c_void,
        name: *const c_char,
        desc: *const c_char,
        kind: c_int,
        format: c_int,
    ) -> *mut c_void;
    fn obs_property_list_add_string(
        p: *mut c_void,
        name: *const c_char,
        val: *const c_char,
    ) -> usize;
    fn obs_property_list_add_int(p: *mut c_void, name: *const c_char, val: i64) -> usize;
    fn obs_properties_add_text(
        props: *mut c_void,
        name: *const c_char,
        desc: *const c_char,
        kind: c_int,
    ) -> *mut c_void;
    fn obs_properties_add_bool(
        props: *mut c_void,
        name: *const c_char,
        desc: *const c_char,
    ) -> *mut c_void;
    fn video_format_get_parameters(
        cs: c_int,
        range: c_int,
        matrix: *mut f32,
        min: *mut f32,
        max: *mut f32,
    ) -> bool;
}

#[repr(C)]
pub(crate) struct SourceInfo {
    id: *const c_char,
    kind: c_int,
    output_flags: u32,
    get_name: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    create: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    get_width: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
    get_height: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
    get_defaults: Option<unsafe extern "C" fn(*mut c_void)>,
    get_properties: Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void>,
    update: unsafe extern "C" fn(*mut c_void, *mut c_void),
    activate: Option<unsafe extern "C" fn(*mut c_void)>,
    deactivate: Option<unsafe extern "C" fn(*mut c_void)>,
    show: Option<unsafe extern "C" fn(*mut c_void)>,
    hide: Option<unsafe extern "C" fn(*mut c_void)>,
}

#[repr(C)]
struct SourceFrame {
    data: [*mut u8; 8],
    linesize: [u32; 8],
    width: u32,
    height: u32,
    timestamp: u64,
    format: c_int,
    color_matrix: [f32; 16],
    full_range: bool,
    max_luminance: u16,
    color_range_min: [f32; 3],
    color_range_max: [f32; 3],
    flip: bool,
    flags: u8,
    trc: u8,
    refs: std::ffi::c_long,
    prev_frame: bool,
}

#[repr(C)]
struct SourceAudio {
    data: [*const u8; 8],
    frames: u32,
    speakers: c_int,
    format: c_int,
    samples_per_sec: u32,
    timestamp: u64,
}

pub(crate) static NDI: OnceLock<Lib> = OnceLock::new();
/// The last discovery result, shared by every source's properties.
static FINDER: Mutex<Cache> = Mutex::new(Cache::new());
/// The live sources, so a finished discovery can refresh their open
/// properties. `destroy` removes a source before it is freed.
static SOURCES: Mutex<Vec<SendPtr>> = Mutex::new(Vec::new());
/// `[NDIPlugin] TallyProgramEnabled / TallyPreviewEnabled`, read once.
static TALLY: Mutex<Option<TallyConfig>> = Mutex::new(None);
/// Set at the frontend's exit and at unload: no new background work, and
/// background work that is still running stops early.
pub(crate) static SHUTDOWN: AtomicBool = AtomicBool::new(false);
/// Background threads (discovery, the output settings' apply delay), so
/// unload can wait for them: none of them may run code of this module, or
/// call the NDI runtime, after `obs_module_unload`.
static BACKGROUND: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

/// Start a background thread, unless the module is shutting down. `false`
/// when it did not start.
pub(crate) fn spawn_background(name: &str, f: impl FnOnce() + Send + 'static) -> bool {
    if SHUTDOWN.load(Ordering::SeqCst) {
        return false;
    }
    let mut threads = BACKGROUND.lock().unwrap();
    threads.retain(|t| !t.is_finished());
    match std::thread::Builder::new().name(name.into()).spawn(f) {
        Ok(t) => {
            threads.push(t);
            true
        }
        Err(_) => false,
    }
}

/// Stop new background work and wait for the running threads.
fn join_background() {
    SHUTDOWN.store(true, Ordering::SeqCst);
    let threads: Vec<_> = BACKGROUND.lock().unwrap().drain(..).collect();
    for t in threads {
        let _ = t.join();
    }
}

/// A frontend function (`obs_frontend_*`), looked up at run time so the
/// module also loads without the frontend. Null when it is missing.
#[cfg(unix)]
pub(crate) unsafe fn frontend_sym(name: &CStr) -> *mut c_void {
    extern "C" {
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    // RTLD_DEFAULT: search every loaded image. 0 on Linux, (void *)-2 on
    // macOS, where a null handle finds nothing.
    let any = if cfg!(target_os = "macos") {
        -2isize as *mut c_void
    } else {
        null_mut()
    };
    dlsym(any, name.as_ptr())
}

/// A frontend function (`obs_frontend_*`) from `obs-frontend-api.dll`, which
/// the frontend has loaded. Null without a frontend.
#[cfg(windows)]
pub(crate) unsafe fn frontend_sym(name: &CStr) -> *mut c_void {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    let dll: Vec<u16> = "obs-frontend-api.dll"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let module = GetModuleHandleW(dll.as_ptr());
    if module.is_null() {
        return null_mut();
    }
    GetProcAddress(module, name.as_ptr())
}

/// The frontend's user config (`user.ini`), or null without a frontend. The
/// call is looked up at run time, so the module also loads without the frontend.
pub(crate) unsafe fn user_config() -> *mut c_void {
    let get = frontend_sym(c"obs_frontend_get_user_config");
    if get.is_null() {
        return null_mut();
    }
    let get: unsafe extern "C" fn() -> *mut c_void = std::mem::transmute(get);
    get()
}

/// A bool from `[NDIPlugin]`, or `default` when it is not set.
pub(crate) unsafe fn ndi_plugin_bool(config: *mut c_void, name: &CStr, default: bool) -> bool {
    if !config.is_null() && config_has_user_value(config, c"NDIPlugin".as_ptr(), name.as_ptr()) {
        config_get_bool(config, c"NDIPlugin".as_ptr(), name.as_ptr())
    } else {
        default
    }
}

/// `blog` with a Rust string (300 info, 200 warning, 100 error).
pub(crate) unsafe fn log(level: c_int, text: &str) {
    let line = CString::new(format!("[obs-avip] {text}")).unwrap_or_default();
    blog(level, c"%s".as_ptr(), line.as_ptr());
}

/// DistroAV's tally switches, `[NDIPlugin] TallyProgramEnabled /
/// TallyPreviewEnabled`, both on by default.
unsafe fn tally_config() -> TallyConfig {
    *TALLY.lock().unwrap().get_or_insert_with(|| {
        let config = user_config();
        let cfg = TallyConfig {
            program: ndi_plugin_bool(config, c"TallyProgramEnabled", true),
            preview: ndi_plugin_bool(config, c"TallyPreviewEnabled", true),
        };
        log(
            300,
            &format!(
                "tally enabled in [NDIPlugin]: program={} preview={}",
                cfg.program, cfg.preview
            ),
        );
        cfg
    })
}

/// Read the tally switches again (the settings window changed them) and
/// work out every live source's tally with them.
pub(crate) unsafe fn reload_tally() {
    TALLY.lock().unwrap().take();
    for source in SOURCES.lock().unwrap().iter() {
        let p = obs_obj_get_data(source.0).cast::<Proto>();
        if !p.is_null() {
            tally_event(&*p, Event::Update);
        }
    }
}

/// Work out the tally after an OBS callback; the receive thread sends it.
/// The callbacks can run on another thread than `update`, so the last value
/// lives only in the atomic, and the callbacks take `&Proto`.
unsafe fn tally_event(p: &Proto, event: Event) {
    let prev = tally::decode(p.tally.load(Ordering::Relaxed));
    let now = tally_config().after(
        event,
        prev,
        obs_source_active(p.source),
        obs_source_showing(p.source),
    );
    p.tally.store(tally::encode(now), Ordering::Relaxed);
}

unsafe extern "C" fn activate(data: *mut c_void) {
    let p = &*data.cast::<Proto>();
    tally_event(p, Event::Activate);
    apply(p, Event::Activate);
}

unsafe extern "C" fn deactivate(data: *mut c_void) {
    let p = &*data.cast::<Proto>();
    tally_event(p, Event::Deactivate);
    apply(p, Event::Deactivate);
}

unsafe extern "C" fn show(data: *mut c_void) {
    let p = &*data.cast::<Proto>();
    tally_event(p, Event::Show);
    apply(p, Event::Show);
}

unsafe extern "C" fn hide(data: *mut c_void) {
    let p = &*data.cast::<Proto>();
    tally_event(p, Event::Hide);
    apply(p, Event::Hide);
}

#[derive(Clone, PartialEq)]
struct Config {
    name: CString,
    s: SourceSettings,
    arrival_ts: bool,
    /// Re-connect after a cut (`ndi_resume`, ours; DistroAV has no such key).
    resume: bool,
}

fn ckey(key: &str) -> CString {
    CString::new(key).expect("settings keys have no NUL")
}

/// `obs_avip::settings::Read` over an `obs_data_t`.
struct Data(*mut c_void);

impl settings::Read for Data {
    fn int(&self, key: &str) -> i64 {
        unsafe { obs_data_get_int(self.0, ckey(key).as_ptr()) }
    }
    fn bool(&self, key: &str) -> bool {
        unsafe { obs_data_get_bool(self.0, ckey(key).as_ptr()) }
    }
    fn string(&self, key: &str) -> String {
        unsafe { CStr::from_ptr(obs_data_get_string(self.0, ckey(key).as_ptr())) }
            .to_string_lossy()
            .into_owned()
    }
}

unsafe fn write(data: *mut c_void, key: &str, value: &Value, default: bool) {
    let k = ckey(key);
    match (value, default) {
        (Value::Int(v), false) => obs_data_set_int(data, k.as_ptr(), *v),
        (Value::Int(v), true) => obs_data_set_default_int(data, k.as_ptr(), *v),
        (Value::Bool(v), false) => obs_data_set_bool(data, k.as_ptr(), *v),
        (Value::Bool(v), true) => obs_data_set_default_bool(data, k.as_ptr(), *v),
        (Value::Str(v), false) => obs_data_set_string(data, k.as_ptr(), ckey(v).as_ptr()),
        (Value::Str(v), true) => obs_data_set_default_string(data, k.as_ptr(), ckey(v).as_ptr()),
    }
}

/// `enum video_colorspace` and `enum video_range_type` in libobs/media-io/video-io.h
fn obs_color(s: &SourceSettings) -> (c_int, c_int) {
    let cs = match s.colorspace {
        ColorSpace::Bt601 => 1,
        ColorSpace::Bt709 => 2,
        ColorSpace::Bt2100Hlg => 5,
    };
    let range = match s.yuv_range {
        YuvRange::Partial => 1,
        YuvRange::Full => 2,
    };
    (cs, range)
}

struct Proto {
    source: *mut c_void,
    /// The settings and the receive thread. `update` and show/hide/activate
    /// run on OBS's video thread (libobs defers `update` of a video source
    /// to the video tick), `create` and `destroy` elsewhere: all go through
    /// this lock, and none of them waits for a receive thread while holding
    /// it, except `destroy`.
    run: Mutex<Run>,
    /// The tally for the receive thread to send (`tally::encode`).
    tally: Arc<AtomicU8>,
    /// Which receive thread may give OBS frames (see [`Gate`]).
    gate: Arc<Gate>,
}

/// The generation of the receive thread that may give OBS frames. Stopping
/// a thread moves the generation on, so the stopped thread's next frame is
/// dropped at once, and the caller can clear the picture without waiting
/// for the thread to end (the video thread must never wait for a receiver
/// to close). A thread hands a frame to OBS only while it holds the lock
/// and its generation is current, so no frame of an old thread can come
/// after a clear.
#[derive(Default)]
struct Gate {
    current: Mutex<u64>,
}

impl Gate {
    /// Make every running thread's frames stale; the new generation.
    fn advance(&self) -> u64 {
        let mut g = self.current.lock().unwrap();
        *g += 1;
        *g
    }

    /// Run `f` (a call into OBS) only if `generation` is still current.
    fn pass(&self, generation: u64, f: impl FnOnce()) {
        let g = self.current.lock().unwrap();
        if *g == generation {
            f();
        }
    }
}

struct Worker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

#[derive(Default)]
struct Run {
    config: Option<Config>,
    worker: Option<Worker>,
    /// Threads told to stop that may still be ending. Only `destroy` joins
    /// them; the others are dropped once they have finished.
    stopping: Vec<JoinHandle<()>>,
}

impl Run {
    /// Tell the running thread to stop and make its frames stale, without
    /// waiting for it.
    fn signal_stop(&mut self, gate: &Gate) {
        if let Some(w) = self.worker.take() {
            w.stop.store(true, Ordering::Relaxed);
            gate.advance();
            self.stopping.push(w.thread);
        }
        self.stopping.retain(|t| !t.is_finished());
    }

    fn join_stopping(&mut self) {
        for t in self.stopping.drain(..) {
            let _ = t.join();
        }
    }
}

/// Start the receive thread for the current settings. It may overlap with a
/// thread that is still ending; that thread's frames are already stale.
unsafe fn start(p: &Proto, run: &mut Run) {
    let Some(config) = run.config.clone() else {
        return;
    };
    if config.name.as_bytes().is_empty() {
        return;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stop_in = stop.clone();
    let tally_in = p.tally.clone();
    let gate = p.gate.clone();
    let generation = gate.advance();
    let source = SendPtr(p.source);
    let spawned = std::thread::Builder::new()
        .name("avip-recv".into())
        .spawn(move || {
            let source = source;
            receive(source.0, config, stop_in, tally_in, &gate, generation);
        });
    match spawned {
        Ok(thread) => run.worker = Some(Worker { stop, thread }),
        Err(e) => log(100, &format!("the receive thread did not start: {e}")),
    }
}

/// Start or stop the receiver after an OBS callback (DistroAV's behavior).
unsafe fn apply(p: &Proto, event: Event) {
    let mut run = p.run.lock().unwrap();
    let Some(behavior) = run.config.as_ref().map(|c| c.s.behavior) else {
        return;
    };
    let state = visibility::State {
        running: run.worker.is_some(),
        active: obs_source_active(p.source),
        showing: obs_source_showing(p.source),
        has_source: run
            .config
            .as_ref()
            .is_some_and(|c| !c.name.as_bytes().is_empty()),
    };
    let a = visibility::action(behavior, event, state);
    if a.stop {
        run.signal_stop(&p.gate);
    }
    if a.clear {
        // After signal_stop no frame of the stopped thread can follow.
        obs_source_output_video(p.source, null());
    }
    if a.start {
        start(p, &mut run);
    }
}

/// The source's saved `web_control_url`.
unsafe fn saved_web_control(source: *mut c_void) -> String {
    let settings = obs_source_get_settings(source);
    let p = obs_data_get_string(settings, c"web_control_url".as_ptr());
    let url = if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    };
    obs_data_release(settings);
    url
}

/// Save a new `web_control_url` through `obs_source_update`, so the change
/// reaches the "update" signal (DistroAV does the same).
unsafe fn set_web_control(source: *mut c_void, url: &str) {
    let update = obs_data_create();
    let curl = CString::new(url).unwrap_or_default();
    obs_data_set_string(update, c"web_control_url".as_ptr(), curl.as_ptr());
    obs_source_update(source, update);
    obs_data_release(update);
}

struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}

unsafe fn receive(
    source: *mut c_void,
    config: Config,
    stop: Arc<AtomicBool>,
    tally_in: Arc<AtomicU8>,
    gate: &Gate,
    generation: u64,
) {
    let ndi: &dyn Api = NDI.get().expect("loaded before a source exists");
    let desc = RecvCreate {
        source: ndi::Source {
            name: config.name.as_ptr(),
            url: null(),
        },
        color_format: match config.s.latency.color_format() {
            ColorFormat::UyvyBgra => 1, // NDIlib_recv_color_format_UYVY_BGRA
            ColorFormat::Fastest => 100,
        },
        bandwidth: match config.s.bandwidth {
            Bandwidth::Highest => 100,
            Bandwidth::Lowest => 0,
            Bandwidth::AudioOnly => 10,
        },
        allow_video_fields: false,
        recv_name: c"OBS (AVIP)".as_ptr(),
    };
    let cfg = resume::Config {
        enabled: config.resume,
        ..resume::Config::default()
    };
    let Some(mut rx) = Receiver::create(ndi, &desc, cfg, os_gettime_ns()) else {
        blog(100, c"[obs-avip] recv_create failed".as_ptr());
        return;
    };
    let mut frame: SourceFrame = std::mem::zeroed();
    let (cs, range) = obs_color(&config.s);
    video_format_get_parameters(
        cs,
        range,
        frame.color_matrix.as_mut_ptr(),
        frame.color_range_min.as_mut_ptr(),
        frame.color_range_max.as_mut_ptr(),
    );
    let skipped = RefCell::new(None);
    let s = &config.s;
    let arrival = config.arrival_ts;
    let last_video = std::cell::Cell::new(None::<u64>);
    let mut saved_url = saved_web_control(source);
    let cleared = std::cell::Cell::new(false);
    // What the SDK delivers and how OBS takes it: one line at the first frame,
    // and one more each time the size, FourCC or field mode changes.
    let shape = std::cell::Cell::new(None::<(u32, u32, u32, bool)>);
    let mut weaver = convert::Weaver::default();
    while !stop.load(Ordering::Relaxed) {
        let t = tally::decode(tally_in.load(Ordering::Relaxed));
        if rx.set_tally(t) {
            log(
                300,
                &format!(
                    "'{}': tally program={} preview={}",
                    s.name, t.program, t.preview
                ),
            );
        }
        // Hidden but kept active: take no frames, so they wait in the SDK
        // (a one-frame sender still shows when the source is shown).
        if !obs_source_showing(source) {
            rx.hold(os_gettime_ns());
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }
        let changes = rx.poll(
            100,
            &mut || os_gettime_ns(),
            &mut |video| {
                let now = os_gettime_ns();
                match convert::video(video, s, now) {
                    Ok(mut v) => {
                        let field = convert::field(video.format_type);
                        let now_shape = (v.width, v.height, video.fourcc, field.is_some());
                        if shape.replace(Some(now_shape)) != Some(now_shape) {
                            let fourcc = String::from_utf8_lossy(&video.fourcc.to_le_bytes()).into_owned();
                            log(
                                300,
                                &format!(
                                    "'{}': video {}x{} {fourcc} as OBS {:?}, {} plane(s), full_range={}{}",
                                    s.name,
                                    v.width,
                                    v.height,
                                    v.format,
                                    v.plane_count,
                                    v.full_range,
                                    if field.is_some() {
                                        ", single fields: woven into full frames"
                                    } else {
                                        ""
                                    }
                                ),
                            );
                        }
                        // Single fields (interlaced sender, "fastest" color
                        // format): weave each pair into one frame.
                        let mut data = video.data.cast_const();
                        if let Some(field) = field {
                            if v.plane_count != 1 {
                                log_skip(&skipped, convert::Skip::PlanarField);
                                return;
                            }
                            let stride = v.planes[0].linesize as usize;
                            let rows = v.height as usize;
                            let bytes = std::slice::from_raw_parts(video.data, stride * rows);
                            match weaver.push(field, bytes, stride, rows, v.timestamp) {
                                Some((woven, first)) => {
                                    data = woven.as_ptr();
                                    v.height *= 2;
                                    v.timestamp = first;
                                }
                                None => return,
                            }
                        }
                        frame.format = v.format as c_int;
                        frame.width = v.width;
                        frame.height = v.height;
                        frame.data = [null_mut(); 8];
                        frame.linesize = [0; 8];
                        for (i, plane) in v.planes.iter().take(v.plane_count).enumerate() {
                            frame.data[i] = data.add(plane.offset).cast_mut();
                            frame.linesize[i] = plane.linesize;
                        }
                        frame.full_range = v.full_range;
                        frame.trc = v.trc;
                        frame.timestamp = if arrival { now } else { v.timestamp };
                        gate.pass(generation, || obs_source_output_video(source, &frame));
                        last_video.set(Some(now));
                        cleared.set(false);
                    }
                    Err(e) => log_skip(&skipped, e),
                }
            },
            &mut |audio| {
                let now = os_gettime_ns();
                match convert::audio(audio, s, now) {
                    Ok(a) => {
                        let mut out = SourceAudio {
                            data: [null(); 8],
                            frames: a.frames,
                            speakers: a.speakers,
                            format: 8, /* AUDIO_FORMAT_FLOAT_PLANAR */
                            samples_per_sec: a.rate,
                            timestamp: if arrival { now } else { a.timestamp },
                        };
                        for (i, plane) in out.data.iter_mut().enumerate().take(a.planes) {
                            *plane = audio.data.add(i * a.stride);
                        }
                        gate.pass(generation, || obs_source_output_audio(source, &out));
                    }
                    Err(e) => log_skip(&skipped, e),
                }
            },
        );
        // No video for 3 s: timeout "clear content" blanks once.
        if visibility::clear_after_timeout(
            s.timeout,
            last_video.get(),
            os_gettime_ns(),
            cleared.get(),
        ) {
            gate.pass(generation, || obs_source_output_video(source, null()));
            cleared.set(true);
        }
        if let Some(url) = rx.web_control_change(os_gettime_ns(), &saved_url) {
            log(
                300,
                &if url.is_empty() {
                    format!("'{}': no web control URL", s.name)
                } else {
                    format!("'{}': web control URL {url}", s.name)
                },
            );
            set_web_control(source, &url);
            saved_url = url;
        }
        for change in changes {
            let ms = |ns: u64| ns as f64 / 1e6;
            let mut level = 300;
            let text = match change {
                Change::Connected {
                    after_ns,
                    reconnects,
                } => {
                    format!(
                        "connected after {:.0} ms ({reconnects} re-connects)",
                        ms(after_ns)
                    )
                }
                Change::Stalled => {
                    "no frames; checking whether the sender is reachable".to_string()
                }
                Change::Reconnecting(Reason::NetworkBack) => {
                    "the sender is reachable again: re-connecting".to_string()
                }
                Change::Reconnecting(Reason::Safety) => {
                    "the sender is reachable but sent no frame for 30 s: re-connecting once"
                        .to_string()
                }
                Change::Reconnecting(Reason::NoAddress) => {
                    "the sender is not in the NDI source list: re-connecting (back-off up to 30 s)"
                        .to_string()
                }
                Change::ReachableNoFrames { after_ns } => {
                    level = 200;
                    format!(
                        "the sender is reachable but sent no frame for {:.0} s (no input on the sender, or the sender stopped serving this OBS; some encoders need an OBS restart then)",
                        ms(after_ns) / 1e3
                    )
                }
                Change::Resumed {
                    after_ns,
                    reconnects,
                } => {
                    format!(
                        "resumed {:.1} s after the last frame ({reconnects} re-connects)",
                        ms(after_ns) / 1e3
                    )
                }
            };
            let line = CString::new(format!("[obs-avip] '{}': {text}", s.name)).unwrap_or_default();
            blog(level, c"%s".as_ptr(), line.as_ptr());
        }
    }
    // recv_destroy runs here, on this thread, never on OBS's video thread.
    drop(rx);
}

/// Log a skipped frame kind once per receiver, not per frame.
unsafe fn log_skip(last: &RefCell<Option<convert::Skip>>, e: convert::Skip) {
    let mut last = last.borrow_mut();
    if last.as_ref() != Some(&e) {
        let line = CString::new(format!("[obs-avip] skipping frames: {e:?}")).unwrap_or_default();
        blog(200, c"%s".as_ptr(), line.as_ptr());
        *last = Some(e);
    }
}

unsafe extern "C" fn update(data: *mut c_void, settings: *mut c_void) {
    let p = &*data.cast::<Proto>();
    let (s, rewrite) = settings::read(&Data(settings));
    for (key, value) in &rewrite {
        write(settings, key, value, false);
    }
    let config = Config {
        name: CString::new(s.name.clone()).unwrap_or_default(),
        arrival_ts: obs_data_get_bool(settings, c"arrival_ts".as_ptr()),
        resume: obs_data_get_bool(settings, c"ndi_resume".as_ptr()),
        s,
    };
    let mut run = p.run.lock().unwrap();
    if run.config.as_ref() == Some(&config) {
        return;
    }
    // No join: this runs on the video thread. The old thread's frames are
    // stale from here on, and it ends by itself.
    run.signal_stop(&p.gate);
    let line = CString::new(format!(
        "[obs-avip] '{}' {:?} arrival_ts={}",
        config.s.name, config.s, config.arrival_ts
    ))
    .unwrap_or_default();
    blog(300, c"%s".as_ptr(), line.as_ptr());
    obs_source_set_async_unbuffered(p.source, config.s.latency.unbuffered());
    obs_source_set_audio_active(p.source, config.s.audio);
    run.config = Some(config.clone());
    tally_event(p, Event::Update);
    let a = visibility::action(
        config.s.behavior,
        Event::Update,
        visibility::State {
            running: false,
            active: obs_source_active(p.source),
            showing: obs_source_showing(p.source),
            has_source: !config.name.as_bytes().is_empty(),
        },
    );
    if a.clear {
        // DistroAV clears when the receiver is reset.
        obs_source_output_video(p.source, null());
    }
    if a.start {
        start(p, &mut run);
    }
}

unsafe extern "C" fn get_defaults(settings: *mut c_void) {
    for (key, value) in settings::defaults() {
        write(settings, key, &value, true);
    }
    obs_data_set_default_bool(settings, c"ndi_resume".as_ptr(), true);
}

/// An int list property with fixed entries.
unsafe fn int_list(props: *mut c_void, key: &str, desc: &CStr, entries: &[(&CStr, i64)]) {
    let list = obs_properties_add_list(
        props,
        ckey(key).as_ptr(),
        desc.as_ptr(),
        2, /* LIST */
        1, /* INT */
    );
    for (name, value) in entries {
        obs_property_list_add_int(list, name.as_ptr(), *value);
    }
}

unsafe extern "C" fn get_properties(data: *mut c_void) -> *mut c_void {
    let current = data
        .cast::<Proto>()
        .as_ref()
        .and_then(|p| {
            p.run
                .lock()
                .unwrap()
                .config
                .as_ref()
                .map(|c| c.s.name.clone())
        })
        .unwrap_or_default();
    let props = obs_properties_create();
    let list = obs_properties_add_list(
        props,
        ckey(keys::SOURCE).as_ptr(),
        c"Source name".as_ptr(),
        1, /* OBS_COMBO_TYPE_EDITABLE: a name can also be typed */
        3, /* OBS_COMBO_FORMAT_STRING */
    );
    let (items, refresh) = {
        let mut cache = FINDER.lock().unwrap();
        (
            finder::source_items(cache.list(), &current),
            cache.start_refresh(os_gettime_ns()),
        )
    };
    for item in items {
        let label = CString::new(item.label).unwrap_or_default();
        let value = CString::new(item.value).unwrap_or_default();
        obs_property_list_add_string(list, label.as_ptr(), value.as_ptr());
    }
    if refresh {
        spawn_find();
    }
    int_list(
        props,
        keys::BANDWIDTH,
        c"Bandwidth",
        &[(c"Highest", 0), (c"Lowest", 1), (c"Audio only", 2)],
    );
    int_list(
        props,
        keys::LATENCY,
        c"Latency",
        &[(c"Normal", 0), (c"Low", 1), (c"Lowest", 2)],
    );
    int_list(
        props,
        keys::SYNC,
        c"Sync",
        &[(c"Sender timecode", 2), (c"Sender timestamp", 1)],
    );
    int_list(
        props,
        keys::YUV_RANGE,
        c"YUV range",
        &[(c"Partial", 1), (c"Full", 2)],
    );
    int_list(
        props,
        keys::YUV_COLORSPACE,
        c"YUV color space",
        &[(c"BT.709", 2), (c"BT.601", 1), (c"BT.2100 (HLG)", 3)],
    );
    obs_properties_add_bool(props, ckey(keys::AUDIO).as_ptr(), c"Audio".as_ptr());
    obs_properties_add_bool(
        props,
        c"ndi_resume".as_ptr(),
        c"Resume at once after a network cut".as_ptr(),
    );
    obs_properties_add_text(
        props,
        c"ndi_info".as_ptr(),
        c"NDI® is a registered trademark of Vizrt NDI AB. More about NDI: https://ndi.video"
            .as_ptr(),
        3, /* OBS_TEXT_INFO */
    );
    props
}

/// Discovery takes a second or more: never on the UI thread. A finished
/// discovery refreshes the open properties of every source. Unload waits
/// for it (at most one wait of the finder).
fn spawn_find() {
    let spawned = spawn_background("avip-find", || {
        let shutting_down = || SHUTDOWN.load(Ordering::SeqCst);
        let found = finder::find_all(NDI.get().expect("loaded"), finder::WAIT_MS, &shutting_down);
        if shutting_down() {
            FINDER.lock().unwrap().cancel();
            return;
        }
        FINDER
            .lock()
            .unwrap()
            .finish(found, unsafe { os_gettime_ns() });
        for source in SOURCES.lock().unwrap().iter() {
            unsafe { obs_source_update_properties(source.0) };
        }
    });
    if !spawned {
        FINDER.lock().unwrap().cancel();
    }
}

unsafe extern "C" fn get_name(_: *mut c_void) -> *const c_char {
    c"AVIP (NDI® source)".as_ptr()
}

unsafe extern "C" fn create(settings: *mut c_void, source: *mut c_void) -> *mut c_void {
    let p = Box::into_raw(Box::new(Proto {
        source,
        run: Mutex::new(Run::default()),
        tally: Arc::new(AtomicU8::new(0)),
        gate: Arc::new(Gate::default()),
    }));
    SOURCES.lock().unwrap().push(SendPtr(source));
    // Start discovery now, so the first properties dialog has a list.
    if FINDER.lock().unwrap().start_refresh(os_gettime_ns()) {
        spawn_find();
    }
    update(p.cast(), settings);
    p.cast()
}

unsafe extern "C" fn destroy(data: *mut c_void) {
    let p = Box::from_raw(data.cast::<Proto>());
    SOURCES.lock().unwrap().retain(|s| s.0 != p.source);
    let mut run = p.run.lock().unwrap();
    // The one place that waits: the source is freed after this.
    run.signal_stop(&p.gate);
    run.join_stopping();
}

static mut MODULE: *mut c_void = null_mut();

#[no_mangle]
pub unsafe extern "C" fn obs_module_set_pointer(module: *mut c_void) {
    MODULE = module;
}

#[no_mangle]
pub unsafe extern "C" fn obs_current_module() -> *mut c_void {
    MODULE
}

#[no_mangle]
pub extern "C" fn obs_module_ver() -> u32 {
    33 << 24 // LIBOBS_API_MAJOR_VER 33
}

#[no_mangle]
pub unsafe extern "C" fn obs_module_load() -> bool {
    // Nothing is registered yet: another NDI plugin may load after this one.
    true
}

/// The registered source id, alive as long as the module (see post_load).
const SOURCE_ID_STATIC: &CStr = c"ndi_source";

/// After every module loaded: register DistroAV's ids unless another module
/// provides them, and only then load the NDI runtime.
#[no_mangle]
pub unsafe extern "C" fn obs_module_post_load() {
    let source_id = ckey(module::SOURCE_ID);
    let output_id = ckey(module::OUTPUT_ID);
    let decision = module::decide(
        !obs_source_get_display_name(source_id.as_ptr()).is_null(),
        !obs_output_get_display_name(output_id.as_ptr()).is_null(),
    );
    if let Some(line) = decision.log_line() {
        log(200, &line);
        return;
    }
    // The runtime reads its network settings once, when it initializes.
    network_ui::prepare();
    let env = std::env::var("NDI_RUNTIME_DIR_V6").ok();
    // A runtime shipped next to the plugin comes first. libobs can give the
    // plugin's path relative to the working directory (Windows portable
    // mode: "../../plugins/obs-avip/obs-avip.dll"); the loader needs an
    // absolute one.
    let own_dir = {
        let p = obs_get_module_binary_path(obs_current_module());
        (!p.is_null())
            .then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
            .and_then(|p| std::path::absolute(&p).ok())
            .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
    };
    let list = |dir: &str| -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let lib = match Lib::load(&ndi::candidates(
        Os::current(),
        own_dir.as_deref(),
        env.as_deref(),
        &list,
    )) {
        Ok(lib) => lib,
        Err(e) => {
            let line = CString::new(format!(
                "[obs-avip] NDI runtime {} not found: {e}",
                ndi::RUNTIME_MAJOR
            ))
            .unwrap_or_default();
            blog(200, c"%s".as_ptr(), line.as_ptr());
            return;
        }
    };
    let line = CString::new(format!(
        "[obs-avip] NDI runtime '{}' from {}",
        lib.version(),
        lib.path
    ))
    .unwrap_or_default();
    blog(300, c"%s".as_ptr(), line.as_ptr());
    let _ = NDI.set(lib);
    // libobs keeps this pointer as the type's id (it does not copy the
    // string), so it must outlive the module: a static, not `source_id`.
    debug_assert_eq!(SOURCE_ID_STATIC.to_str(), Ok(module::SOURCE_ID));
    let info = SourceInfo {
        id: SOURCE_ID_STATIC.as_ptr(),
        kind: 0,         // OBS_SOURCE_TYPE_INPUT
        output_flags: 7, // OBS_SOURCE_ASYNC_VIDEO | OBS_SOURCE_AUDIO
        get_name,
        create,
        destroy,
        get_width: None,
        get_height: None,
        get_defaults: Some(get_defaults),
        get_properties: Some(get_properties),
        update,
        activate: Some(activate),
        deactivate: Some(deactivate),
        show: Some(show),
        hide: Some(hide),
    };
    obs_register_source_s(&info, std::mem::size_of::<SourceInfo>());
    output::register();
    settings_ui::register();
    network_ui::register();
    log(
        300,
        &format!(
            "registered '{}' and '{}'",
            module::SOURCE_ID,
            module::OUTPUT_ID
        ),
    );
}

#[no_mangle]
pub unsafe extern "C" fn obs_module_unload() {
    // First: no discovery or apply thread may outlive the module or the runtime.
    join_background();
    output::close_main();
    settings_ui::close();
    network_ui::close();
    // Sources are destroyed (and their receivers with them) before modules unload.
    if let Some(lib) = NDI.get() {
        lib.shutdown();
    }
}
