//! The NDI output and DistroAV's "NDI Main Output".
//!
//! The output id is DistroAV's `ndi_output`; it is registered only when no
//! other module provides it (`obs_avip::module`, checked in post_load).
//!
//! The main output is created when the frontend has finished loading, from
//! DistroAV's keys in `[NDIPlugin]` of the user config (`MainOutputEnabled`,
//! `MainOutputName`, `MainOutputGroups`), with DistroAV's defaults (off,
//! "OBS PGM", no groups), under DistroAV's output name "NDI Main Output", so
//! obs-websocket clients that start or stop it by name keep working.
//!
//! The preview output ("NDI Preview Output", `PreviewOutput*` keys, default
//! off and "OBS Preview", video only) sends the studio-mode preview scene, or
//! the program scene outside studio mode. Its picture comes from an
//! `obs_view` that exists only while the output runs.
//!
//! Both write their `...OutputEnabled` key back when they are started or
//! stopped, also remotely (obs-websocket), as DistroAV does; the exit stop is
//! not written.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr::null_mut;
use std::sync::Mutex;

use obs_avip::send::{self, OutFormat, Sender, VideoIn};

use crate::{log, ndi_plugin_bool, user_config, NDI};

#[cfg_attr(windows, link(name = "obs", kind = "raw-dylib"))]
extern "C" {
    fn obs_register_output_s(info: *const OutputInfo, size: usize);
    fn obs_output_video(output: *mut c_void) -> *mut c_void;
    fn obs_output_audio(output: *mut c_void) -> *mut c_void;
    fn obs_output_begin_data_capture(output: *mut c_void, flags: u32) -> bool;
    fn obs_output_end_data_capture(output: *mut c_void);
    fn obs_output_set_last_error(output: *mut c_void, message: *const c_char);
    fn obs_output_create(
        id: *const c_char,
        name: *const c_char,
        settings: *mut c_void,
        hotkeys: *mut c_void,
    ) -> *mut c_void;
    fn obs_output_start(output: *mut c_void) -> bool;
    fn obs_output_stop(output: *mut c_void);
    fn obs_output_active(output: *mut c_void) -> bool;
    fn obs_output_release(output: *mut c_void);
    fn obs_output_get_last_error(output: *mut c_void) -> *const c_char;
    fn video_output_get_info(video: *mut c_void) -> *const VideoOutputInfo;
    fn audio_output_get_sample_rate(audio: *mut c_void) -> u32;
    fn audio_output_get_channels(audio: *mut c_void) -> usize;
    fn obs_data_create() -> *mut c_void;
    fn obs_data_release(data: *mut c_void);
    fn obs_data_get_string(data: *mut c_void, name: *const c_char) -> *const c_char;
    fn obs_data_get_bool(data: *mut c_void, name: *const c_char) -> bool;
    fn obs_data_set_string(data: *mut c_void, name: *const c_char, val: *const c_char);
    fn obs_data_set_default_string(data: *mut c_void, name: *const c_char, val: *const c_char);
    fn obs_data_set_default_bool(data: *mut c_void, name: *const c_char, val: bool);
    fn obs_properties_create() -> *mut c_void;
    fn obs_properties_add_text(
        props: *mut c_void,
        name: *const c_char,
        desc: *const c_char,
        kind: c_int,
    ) -> *mut c_void;
    fn config_get_string(
        config: *mut c_void,
        section: *const c_char,
        name: *const c_char,
    ) -> *const c_char;
    fn config_has_user_value(
        config: *mut c_void,
        section: *const c_char,
        name: *const c_char,
    ) -> bool;
    fn os_gettime_ns() -> u64;
    fn obs_output_get_name(output: *mut c_void) -> *const c_char;
    fn obs_output_get_settings(output: *mut c_void) -> *mut c_void;
    fn obs_output_get_signal_handler(output: *mut c_void) -> *mut c_void;
    fn obs_output_set_media(output: *mut c_void, video: *mut c_void, audio: *mut c_void);
    fn obs_data_set_bool(data: *mut c_void, name: *const c_char, val: bool);
    fn signal_handler_connect(
        handler: *mut c_void,
        signal: *const c_char,
        callback: unsafe extern "C" fn(*mut c_void, *mut c_void),
        data: *mut c_void,
    );
    fn signal_handler_disconnect(
        handler: *mut c_void,
        signal: *const c_char,
        callback: unsafe extern "C" fn(*mut c_void, *mut c_void),
        data: *mut c_void,
    );
    fn config_set_bool(config: *mut c_void, section: *const c_char, name: *const c_char, val: bool);
    fn config_save(config: *mut c_void) -> c_int;
    fn obs_get_video_info(ovi: *mut c_void) -> bool;
    fn obs_get_audio() -> *mut c_void;
    fn obs_view_create() -> *mut c_void;
    fn obs_view_destroy(view: *mut c_void);
    fn obs_view_set_source(view: *mut c_void, channel: u32, source: *mut c_void);
    fn obs_view_add2(view: *mut c_void, ovi: *mut c_void) -> *mut c_void;
    fn obs_view_remove(view: *mut c_void);
    fn obs_source_release(source: *mut c_void);
}

/// `struct obs_output_info`, up to `get_properties`.
#[repr(C)]
struct OutputInfo {
    id: *const c_char,
    flags: u32,
    get_name: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    create: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    start: unsafe extern "C" fn(*mut c_void) -> bool,
    stop: unsafe extern "C" fn(*mut c_void, u64),
    raw_video: Option<unsafe extern "C" fn(*mut c_void, *mut VideoData)>,
    raw_audio: Option<unsafe extern "C" fn(*mut c_void, *mut AudioData)>,
    encoded_packet: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    update: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    get_defaults: Option<unsafe extern "C" fn(*mut c_void)>,
    get_properties: Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void>,
}

/// `struct video_data`
#[repr(C)]
struct VideoData {
    data: [*mut u8; 8],
    linesize: [u32; 8],
    timestamp: u64,
}

/// `struct audio_data`
#[repr(C)]
struct AudioData {
    data: [*mut u8; 8],
    frames: u32,
    timestamp: u64,
}

/// `struct video_output_info`
#[repr(C)]
struct VideoOutputInfo {
    name: *const c_char,
    format: c_int,
    fps_num: u32,
    fps_den: u32,
    width: u32,
    height: u32,
    cache_size: usize,
    colorspace: c_int,
    range: c_int,
}

const OUTPUT_ID: &CStr = c"ndi_output";
const MAIN_OUTPUT_NAME: &CStr = c"NDI Main Output";
const OBS_OUTPUT_VIDEO: u32 = 1;
const OBS_OUTPUT_AUDIO: u32 = 2;

struct Running {
    /// The NDI name, for the log lines of the output threads.
    name: String,
    sender: Sender<'static>,
    format: OutFormat,
    height: u32,
    rate: u32,
    channels: usize,
}

/// The output's settings. `update` can come at any time (obs-websocket
/// `SetOutputSettings`), also while the output runs; they take effect at the
/// next start.
#[derive(Clone, Default)]
struct OutSettings {
    name: CString,
    groups: CString,
    uses_video: bool,
    uses_audio: bool,
}

struct Out {
    output: *mut c_void,
    settings: Mutex<OutSettings>,
    /// Set by `start`, cleared by `stop`; `raw_video` and `raw_audio` run on
    /// OBS's output threads and take the lock.
    running: Mutex<Option<Running>>,
}

unsafe fn text(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

unsafe extern "C" fn get_name(_: *mut c_void) -> *const c_char {
    c"AVIP (NDI® output)".as_ptr()
}

unsafe extern "C" fn get_defaults(settings: *mut c_void) {
    obs_data_set_default_string(settings, c"ndi_name".as_ptr(), c"OBS".as_ptr());
    obs_data_set_default_string(settings, c"ndi_groups".as_ptr(), c"".as_ptr());
    obs_data_set_default_bool(settings, c"uses_video".as_ptr(), true);
    obs_data_set_default_bool(settings, c"uses_audio".as_ptr(), true);
}

unsafe extern "C" fn get_properties(_: *mut c_void) -> *mut c_void {
    let props = obs_properties_create();
    obs_properties_add_text(
        props,
        c"ndi_name".as_ptr(),
        c"NDI name".as_ptr(),
        0, /* OBS_TEXT_DEFAULT */
    );
    obs_properties_add_text(props, c"ndi_groups".as_ptr(), c"NDI groups".as_ptr(), 0);
    props
}

unsafe fn read(settings: *mut c_void) -> OutSettings {
    let string = |key: &CStr| CString::new(text(obs_data_get_string(settings, key.as_ptr())));
    OutSettings {
        name: string(c"ndi_name").unwrap_or_default(),
        groups: string(c"ndi_groups").unwrap_or_default(),
        uses_video: obs_data_get_bool(settings, c"uses_video".as_ptr()),
        uses_audio: obs_data_get_bool(settings, c"uses_audio".as_ptr()),
    }
}

unsafe extern "C" fn create(settings: *mut c_void, output: *mut c_void) -> *mut c_void {
    let o = Box::new(Out {
        output,
        settings: Mutex::new(read(settings)),
        running: Mutex::new(None),
    });
    Box::into_raw(o).cast()
}

unsafe extern "C" fn destroy(data: *mut c_void) {
    drop(Box::from_raw(data.cast::<Out>()));
}

unsafe fn fail(o: &Out, name: &CStr, why: &str) -> bool {
    let msg = CString::new(why).unwrap_or_default();
    obs_output_set_last_error(o.output, msg.as_ptr());
    log(
        200,
        &format!("output '{}' cannot start: {why}", name.to_string_lossy()),
    );
    false
}

unsafe extern "C" fn start(data: *mut c_void) -> bool {
    let o = &*data.cast::<Out>();
    if o.running.lock().unwrap().is_some() {
        return false;
    }
    let cfg = o.settings.lock().unwrap().clone();
    let Some(api) = NDI.get() else {
        return fail(o, &cfg.name, "the NDI runtime is not loaded");
    };
    // The preview output renders its own view; it exists only while running
    // (freed on the output's "deactivate", once the capture has ended).
    if kind_of(o.output) == Some(Kind::Preview) && !preview_attach(o.output) {
        return fail(o, &cfg.name, "the preview view could not be created");
    }
    let video = obs_output_video(o.output);
    let audio = obs_output_audio(o.output);
    let mut flags = 0;
    let (mut format, mut width, mut height, mut fps) = (OutFormat::Bgra, 0, 0, (0, 1));
    if cfg.uses_video && !video.is_null() {
        let info = &*video_output_get_info(video);
        format = match send::out_format(info.format) {
            Ok(f) => f,
            Err(v) => {
                return fail(
                    o,
                    &cfg.name,
                    &format!("OBS video format {v} has no NDI format"),
                )
            }
        };
        (width, height, fps) = (info.width, info.height, (info.fps_num, info.fps_den));
        flags |= OBS_OUTPUT_VIDEO;
    }
    let (mut rate, mut channels) = (0, 0);
    if cfg.uses_audio && !audio.is_null() {
        rate = audio_output_get_sample_rate(audio);
        channels = audio_output_get_channels(audio);
        flags |= OBS_OUTPUT_AUDIO;
    }
    if flags == 0 {
        return fail(o, &cfg.name, "no video and no audio");
    }
    let groups = (!cfg.groups.as_bytes().is_empty()).then_some(cfg.groups.as_c_str());
    let Some(sender) = Sender::create(api, &cfg.name, groups, format, width, height, fps) else {
        return fail(o, &cfg.name, "NDIlib_send_create failed");
    };
    let name = cfg.name.to_string_lossy().into_owned();
    *o.running.lock().unwrap() = Some(Running {
        name: name.clone(),
        sender,
        format,
        height,
        rate,
        channels,
    });
    if !obs_output_begin_data_capture(o.output, flags) {
        o.running.lock().unwrap().take();
        return fail(o, &cfg.name, "obs_output_begin_data_capture failed");
    }
    log(
        300,
        &format!(
            "output '{}' started: {width}x{height} {:?} at {}/{} fps, {channels} channels at {rate} Hz",
            name,
            format,
            fps.0,
            fps.1
        ),
    );
    true
}

unsafe extern "C" fn stop(data: *mut c_void, _ts: u64) {
    let o = &*data.cast::<Out>();
    if o.running.lock().unwrap().is_none() {
        return;
    }
    // libobs ends the capture on its own thread; a raw callback that still
    // comes finds no sender. The preview view stays until "deactivate".
    obs_output_end_data_capture(o.output);
    // Dropping the sender waits for the SDK to finish the last frame.
    let name = o
        .running
        .lock()
        .unwrap()
        .take()
        .map(|r| r.name)
        .unwrap_or_default();
    log(300, &format!("output '{name}' stopped"));
}

/// New settings take effect at the next start: stopping and starting here
/// would race with libobs ending the capture on its own thread. The main
/// and preview outputs are re-created when their name or groups change
/// (`apply_config`).
unsafe extern "C" fn update(data: *mut c_void, settings: *mut c_void) {
    let o = &*data.cast::<Out>();
    *o.settings.lock().unwrap() = read(settings);
    if o.running.lock().unwrap().is_some() {
        log(
            300,
            "output settings changed: they take effect at the next start",
        );
    }
}

unsafe extern "C" fn raw_video(data: *mut c_void, frame: *mut VideoData) {
    let o = &*data.cast::<Out>();
    let mut running = o.running.lock().unwrap();
    let Some(r) = running.as_mut() else { return };
    let f = &*frame;
    let rows = send::plane_rows(r.format, r.height);
    let plane = |i: usize| -> &[u8] {
        if f.data[i].is_null() || rows[i] == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(f.data[i], f.linesize[i] as usize * rows[i])
        }
    };
    let v = VideoIn {
        planes: [plane(0), plane(1), plane(2)],
        linesize: [f.linesize[0], f.linesize[1], f.linesize[2]],
        timestamp_ns: f.timestamp,
    };
    r.sender.video(&v);
    if let Some(n) = r.sender.connections(os_gettime_ns()) {
        log(300, &format!("output '{}': {n} receiver(s)", r.name));
    }
}

unsafe extern "C" fn raw_audio(data: *mut c_void, frames: *mut AudioData) {
    let o = &*data.cast::<Out>();
    let mut running = o.running.lock().unwrap();
    let Some(r) = running.as_mut() else { return };
    let a = &*frames;
    let n = a.frames as usize;
    // No allocation on the audio path: at most 8 planes (MAX_AV_PLANES).
    let mut planes: [&[f32]; 8] = [&[]; 8];
    let mut count = 0;
    for i in 0..r.channels.min(8) {
        if !a.data[i].is_null() {
            planes[count] = std::slice::from_raw_parts(a.data[i].cast::<f32>(), n);
            count += 1;
        }
    }
    r.sender
        .audio(&planes[..count], a.frames, r.rate, a.timestamp);
}

/// Register the output type.
pub(crate) unsafe fn register() {
    let info = OutputInfo {
        id: OUTPUT_ID.as_ptr(),
        flags: OBS_OUTPUT_VIDEO | OBS_OUTPUT_AUDIO,
        get_name,
        create,
        destroy,
        start,
        stop,
        raw_video: Some(raw_video),
        raw_audio: Some(raw_audio),
        encoded_packet: None,
        update: Some(update),
        get_defaults: Some(get_defaults),
        get_properties: Some(get_properties),
    };
    obs_register_output_s(&info, std::mem::size_of::<OutputInfo>());
    // The main output needs the frontend: wait for its "finished loading".
    let add = crate::frontend_sym(c"obs_frontend_add_event_callback");
    if add.is_null() {
        log(300, "no frontend: the main output is not created");
        return;
    }
    let add: unsafe extern "C" fn(unsafe extern "C" fn(c_int, *mut c_void), *mut c_void) =
        std::mem::transmute(add);
    add(on_frontend_event, null_mut());
}

/// The two outputs DistroAV keeps under fixed names, with their `[NDIPlugin]` keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Main,
    Preview,
}

impl Kind {
    const ALL: [Kind; 2] = [Kind::Main, Kind::Preview];
    fn output_name(self) -> &'static CStr {
        match self {
            Kind::Main => MAIN_OUTPUT_NAME,
            Kind::Preview => c"NDI Preview Output",
        }
    }
    fn enabled_key(self) -> &'static CStr {
        match self {
            Kind::Main => c"MainOutputEnabled",
            Kind::Preview => c"PreviewOutputEnabled",
        }
    }
    fn name_key(self) -> &'static CStr {
        match self {
            Kind::Main => c"MainOutputName",
            Kind::Preview => c"PreviewOutputName",
        }
    }
    fn groups_key(self) -> &'static CStr {
        match self {
            Kind::Main => c"MainOutputGroups",
            Kind::Preview => c"PreviewOutputGroups",
        }
    }
    fn default_name(self) -> &'static str {
        match self {
            Kind::Main => "OBS PGM",
            Kind::Preview => "OBS Preview",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Kind::Main => "main",
            Kind::Preview => "preview",
        }
    }
    /// The signal callbacks' data: the index, not a pointer.
    fn tag(self) -> *mut c_void {
        (self as usize + 1) as *mut c_void
    }
    fn from_tag(tag: *mut c_void) -> Option<Kind> {
        Kind::ALL.get((tag as usize).checked_sub(1)?).copied()
    }
}

/// Which managed output this is, from its name.
unsafe fn kind_of(output: *mut c_void) -> Option<Kind> {
    let name = obs_output_get_name(output);
    if name.is_null() {
        return None;
    }
    let name = CStr::from_ptr(name);
    Kind::ALL.into_iter().find(|k| k.output_name() == name)
}

struct Managed(*mut c_void);
// SAFETY: the pointers are only used on the frontend's thread and at unload.
unsafe impl Send for Managed {}

static MANAGED: Mutex<[Option<Managed>; 2]> = Mutex::new([None, None]);

const OBS_FRONTEND_EVENT_SCENE_CHANGED: c_int = 8;
const OBS_FRONTEND_EVENT_SCENE_COLLECTION_CHANGED: c_int = 13;
const OBS_FRONTEND_EVENT_EXIT: c_int = 17;
const OBS_FRONTEND_EVENT_STUDIO_MODE_ENABLED: c_int = 22;
const OBS_FRONTEND_EVENT_STUDIO_MODE_DISABLED: c_int = 23;
const OBS_FRONTEND_EVENT_PREVIEW_SCENE_CHANGED: c_int = 24;
const OBS_FRONTEND_EVENT_SCENE_COLLECTION_CLEANUP: c_int = 25;
const OBS_FRONTEND_EVENT_FINISHED_LOADING: c_int = 26;

unsafe extern "C" fn on_frontend_event(event: c_int, _: *mut c_void) {
    match event {
        OBS_FRONTEND_EVENT_FINISHED_LOADING => {
            for kind in Kind::ALL {
                open(kind);
            }
        }
        OBS_FRONTEND_EVENT_EXIT => {
            // No output is opened again after this (a pending settings apply).
            crate::SHUTDOWN.store(true, std::sync::atomic::Ordering::SeqCst);
            close_main();
        }
        OBS_FRONTEND_EVENT_SCENE_CHANGED
        | OBS_FRONTEND_EVENT_SCENE_COLLECTION_CHANGED
        | OBS_FRONTEND_EVENT_STUDIO_MODE_ENABLED
        | OBS_FRONTEND_EVENT_STUDIO_MODE_DISABLED
        | OBS_FRONTEND_EVENT_PREVIEW_SCENE_CHANGED => preview_follow(false),
        // The collection's scenes are about to be freed: let go of them.
        OBS_FRONTEND_EVENT_SCENE_COLLECTION_CLEANUP => preview_follow(true),
        _ => {}
    }
}

pub(crate) unsafe fn ndi_plugin_string(config: *mut c_void, name: &CStr, default: &str) -> String {
    if !config.is_null() && config_has_user_value(config, c"NDIPlugin".as_ptr(), name.as_ptr()) {
        text(config_get_string(
            config,
            c"NDIPlugin".as_ptr(),
            name.as_ptr(),
        ))
    } else {
        default.to_string()
    }
}

/// Write `...OutputEnabled` back, as DistroAV does on its start/stop signals.
unsafe fn write_enabled(kind: Kind, enabled: bool) {
    let config = user_config();
    if config.is_null() || ndi_plugin_bool(config, kind.enabled_key(), false) == enabled {
        return;
    }
    config_set_bool(
        config,
        c"NDIPlugin".as_ptr(),
        kind.enabled_key().as_ptr(),
        enabled,
    );
    config_save(config);
    log(
        300,
        &format!(
            "{} output {}: {}={enabled}",
            kind.label(),
            if enabled { "started" } else { "stopped" },
            kind.enabled_key().to_string_lossy()
        ),
    );
}

unsafe extern "C" fn on_started(tag: *mut c_void, _: *mut c_void) {
    if let Some(kind) = Kind::from_tag(tag) {
        write_enabled(kind, true);
    }
}

unsafe extern "C" fn on_stopped(tag: *mut c_void, _: *mut c_void) {
    if let Some(kind) = Kind::from_tag(tag) {
        write_enabled(kind, false);
    }
}

/// libobs sends "deactivate" from its end-capture thread after the raw
/// callbacks are disconnected: only now may the preview view go. The next
/// start waits for this (`obs_output_actual_start` waits for the stop).
unsafe extern "C" fn on_deactivate(tag: *mut c_void, _: *mut c_void) {
    if Kind::from_tag(tag) == Some(Kind::Preview) {
        preview_detach();
    }
}

unsafe fn signals(output: *mut c_void, kind: Kind, connect: bool) {
    let sh = obs_output_get_signal_handler(output);
    if sh.is_null() {
        return;
    }
    let f = if connect {
        signal_handler_connect
    } else {
        signal_handler_disconnect
    };
    f(sh, c"start".as_ptr(), on_started, kind.tag());
    f(sh, c"stop".as_ptr(), on_stopped, kind.tag());
    f(sh, c"deactivate".as_ptr(), on_deactivate, kind.tag());
}

unsafe fn open(kind: Kind) {
    let config = user_config();
    let enabled = ndi_plugin_bool(config, kind.enabled_key(), false);
    let name = ndi_plugin_string(config, kind.name_key(), kind.default_name());
    let groups = ndi_plugin_string(config, kind.groups_key(), "");
    let settings = obs_data_create();
    let (cname, cgroups) = (
        CString::new(name.clone()).unwrap_or_default(),
        CString::new(groups).unwrap_or_default(),
    );
    obs_data_set_string(settings, c"ndi_name".as_ptr(), cname.as_ptr());
    obs_data_set_string(settings, c"ndi_groups".as_ptr(), cgroups.as_ptr());
    // The preview has no audio (DistroAV's choice).
    obs_data_set_bool(settings, c"uses_audio".as_ptr(), kind == Kind::Main);
    let output = obs_output_create(
        OUTPUT_ID.as_ptr(),
        kind.output_name().as_ptr(),
        settings,
        null_mut(),
    );
    obs_data_release(settings);
    if output.is_null() {
        log(
            100,
            &format!("the {} output could not be created", kind.label()),
        );
        return;
    }
    signals(output, kind, true);
    MANAGED.lock().unwrap()[kind as usize] = Some(Managed(output));
    if enabled && !name.is_empty() {
        if !obs_output_start(output) {
            log(
                100,
                &format!(
                    "the {} output did not start: {}",
                    kind.label(),
                    text(obs_output_get_last_error(output))
                ),
            );
        }
    } else {
        log(
            300,
            &format!(
                "{} output '{name}' is off ({})",
                kind.label(),
                kind.enabled_key().to_string_lossy()
            ),
        );
    }
}

/// Stop and release one output. The signals go first, so this stop does not
/// write `...OutputEnabled=false`.
unsafe fn close(kind: Kind) {
    let taken = MANAGED.lock().unwrap()[kind as usize].take();
    if let Some(Managed(output)) = taken {
        signals(output, kind, false);
        if obs_output_active(output) {
            obs_output_stop(output);
        }
        obs_output_release(output);
    }
    if kind == Kind::Preview {
        preview_detach();
    }
}

/// Stop and release both outputs (exit and unload).
pub(crate) unsafe fn close_main() {
    for kind in Kind::ALL {
        close(kind);
    }
}

/// Bring both outputs in line with `[NDIPlugin]` after the settings window
/// changed it: a new name or groups re-creates the output; otherwise it is
/// only started or stopped to match `...OutputEnabled`.
pub(crate) unsafe fn apply_config() {
    let config = user_config();
    for kind in Kind::ALL {
        let enabled = ndi_plugin_bool(config, kind.enabled_key(), false);
        let name = ndi_plugin_string(config, kind.name_key(), kind.default_name());
        let groups = ndi_plugin_string(config, kind.groups_key(), "");
        let current = MANAGED.lock().unwrap()[kind as usize].as_ref().map(|m| m.0);
        let Some(output) = current else {
            open(kind);
            continue;
        };
        let settings = obs_output_get_settings(output);
        let same = text(obs_data_get_string(settings, c"ndi_name".as_ptr())) == name
            && text(obs_data_get_string(settings, c"ndi_groups".as_ptr())) == groups;
        obs_data_release(settings);
        if !same {
            close(kind);
            open(kind);
            continue;
        }
        let active = obs_output_active(output);
        if enabled && !name.is_empty() && !active {
            if !obs_output_start(output) {
                log(
                    100,
                    &format!(
                        "the {} output did not start: {}",
                        kind.label(),
                        text(obs_output_get_last_error(output))
                    ),
                );
            }
        } else if !enabled && active {
            obs_output_stop(output);
        }
    }
}

// ---- preview view ----------------------------------------------------

struct View {
    view: *mut c_void,
}
// SAFETY: libobs locks the view itself; we only hand the pointer back to it.
unsafe impl Send for View {}

static PREVIEW: Mutex<Option<View>> = Mutex::new(None);

unsafe fn frontend<T: Copy>(name: &CStr) -> Option<T> {
    let f = crate::frontend_sym(name);
    (!f.is_null()).then(|| std::mem::transmute_copy(&f))
}

/// A new reference to what the preview output shows: the studio-mode
/// preview scene, or the program scene outside studio mode.
unsafe fn preview_scene() -> *mut c_void {
    type Get = unsafe extern "C" fn() -> *mut c_void;
    type Active = unsafe extern "C" fn() -> bool;
    let studio =
        frontend::<Active>(c"obs_frontend_preview_program_mode_active").is_some_and(|f| f());
    let name = if studio {
        c"obs_frontend_get_current_preview_scene"
    } else {
        c"obs_frontend_get_current_scene"
    };
    frontend::<Get>(name).map_or(null_mut(), |f| f())
}

unsafe fn set_view_scene(view: *mut c_void, scene: *mut c_void) {
    obs_view_set_source(view, 0, scene);
    if !scene.is_null() {
        obs_source_release(scene);
    }
}

/// Create the preview view and give its video to `output`. `false` when the
/// view gives no video.
unsafe fn preview_attach(output: *mut c_void) -> bool {
    let mut preview = PREVIEW.lock().unwrap();
    if preview.is_none() {
        let view = obs_view_create();
        set_view_scene(view, preview_scene());
        // Room for `struct obs_video_info` (56 bytes on 64-bit).
        let mut ovi = [0u64; 16];
        if !obs_get_video_info(ovi.as_mut_ptr().cast()) {
            obs_view_destroy(view);
            return false;
        }
        let video = obs_view_add2(view, ovi.as_mut_ptr().cast());
        if video.is_null() {
            obs_view_set_source(view, 0, null_mut());
            obs_view_destroy(view);
            return false;
        }
        obs_output_set_media(output, video, obs_get_audio());
        *preview = Some(View { view });
    }
    true
}

unsafe fn preview_detach() {
    if let Some(View { view }) = PREVIEW.lock().unwrap().take() {
        obs_view_remove(view);
        obs_view_set_source(view, 0, null_mut());
        obs_view_destroy(view);
    }
}

/// Follow the scene the preview output shows; `release` drops it (the scene
/// collection is being cleared).
unsafe fn preview_follow(release: bool) {
    if let Some(View { view }) = PREVIEW.lock().unwrap().as_ref() {
        let scene = if release { null_mut() } else { preview_scene() };
        set_view_scene(*view, scene);
    }
}
