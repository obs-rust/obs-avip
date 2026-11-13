//! DistroAV's dedicated output filters: `ndi_filter` (picture and audio of the
//! source it is on) and `ndi_audiofilter` (audio only), each its own NDI
//! stream. The rules (ids, keys, default name, tokens, when video is sent,
//! the read-back order) are in `obs_avip::filter`; this file only translates
//! between libobs and the core.
//!
//! Threads: `filter_add`, `filter_remove`, `update` and the rename signals
//! open and close the stream (any thread that edits filters, also
//! obs-websocket's); `video_render` and `video_tick` run on the graphics
//! thread; `filter_audio` on the audio thread. The sender is behind one lock.
//!
//! Audio: libobs calls `filter_audio` only for async sources (captures,
//! media, NDI sources). A filter on a scene or an image sends no audio, as in
//! DistroAV.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::mem::offset_of;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use obs_avip::filter::{self, keys, Readback};
use obs_avip::send::{OutFormat, Sender, VideoIn};

use crate::{log, NDI};

#[cfg_attr(windows, link(name = "obs", kind = "raw-dylib"))]
extern "C" {
    // The same declaration as lib.rs (one signature per symbol).
    fn obs_register_source_s(info: *const crate::SourceInfo, size: usize);
    fn obs_source_get_settings(source: *mut c_void) -> *mut c_void;
    fn obs_source_get_name(source: *mut c_void) -> *const c_char;
    fn obs_source_get_signal_handler(source: *mut c_void) -> *mut c_void;
    fn obs_source_enabled(source: *mut c_void) -> bool;
    fn obs_source_showing(source: *mut c_void) -> bool;
    fn obs_source_get_width(source: *mut c_void) -> u32;
    fn obs_source_get_height(source: *mut c_void) -> u32;
    fn obs_source_skip_video_filter(filter: *mut c_void);
    fn obs_source_video_render(source: *mut c_void);
    fn obs_filter_get_parent(filter: *mut c_void) -> *mut c_void;
    fn obs_filter_get_target(filter: *mut c_void) -> *mut c_void;
    fn obs_data_release(data: *mut c_void);
    fn obs_data_get_string(data: *mut c_void, name: *const c_char) -> *const c_char;
    fn obs_data_set_string(data: *mut c_void, name: *const c_char, val: *const c_char);
    fn obs_data_set_default_string(data: *mut c_void, name: *const c_char, val: *const c_char);
    fn obs_properties_create() -> *mut c_void;
    fn obs_properties_set_flags(props: *mut c_void, flags: u32);
    fn obs_properties_add_text(
        props: *mut c_void,
        name: *const c_char,
        desc: *const c_char,
        kind: c_int,
    ) -> *mut c_void;
    fn obs_property_set_long_description(prop: *mut c_void, text: *const c_char);
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
    fn obs_get_video() -> *mut c_void;
    fn obs_get_audio() -> *mut c_void;
    fn video_output_get_info(video: *mut c_void) -> *const VideoOutputInfo;
    fn audio_output_get_sample_rate(audio: *mut c_void) -> u32;
    fn audio_output_get_channels(audio: *mut c_void) -> usize;
    fn obs_get_video_frame_time() -> u64;
    fn os_gettime_ns() -> u64;
    fn obs_enter_graphics();
    fn obs_leave_graphics();
    fn gs_texrender_create(format: c_int, zstencil: c_int) -> *mut c_void;
    fn gs_texrender_destroy(texrender: *mut c_void);
    fn gs_texrender_reset(texrender: *mut c_void);
    fn gs_texrender_begin(texrender: *mut c_void, cx: u32, cy: u32) -> bool;
    fn gs_texrender_end(texrender: *mut c_void);
    fn gs_texrender_get_texture(texrender: *mut c_void) -> *mut c_void;
    fn gs_stagesurface_create(width: u32, height: u32, format: c_int) -> *mut c_void;
    fn gs_stagesurface_destroy(stagesurf: *mut c_void);
    fn gs_stage_texture(dst: *mut c_void, src: *mut c_void);
    fn gs_stagesurface_map(stagesurf: *mut c_void, data: *mut *mut u8, linesize: *mut u32) -> bool;
    fn gs_stagesurface_unmap(stagesurf: *mut c_void);
    fn gs_clear(flags: u32, color: *const Vec4, depth: f32, stencil: u8);
    fn gs_ortho(left: f32, right: f32, top: f32, bottom: f32, znear: f32, zfar: f32);
    fn gs_blend_state_push();
    fn gs_blend_state_pop();
    fn gs_blend_function(src: c_int, dest: c_int);
}

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

/// `struct vec4` (16-byte aligned, SSE).
#[repr(C, align(16))]
struct Vec4([f32; 4]);

/// `struct obs_audio_data`.
#[repr(C)]
struct AudioData {
    data: [*mut u8; 8],
    frames: u32,
    timestamp: u64,
}

type Unused = Option<unsafe extern "C" fn()>;

/// `struct obs_source_info` up to `filter_add`, the last field a filter
/// uses; `obs_register_source_s` takes the size, libobs zeroes the rest.
#[repr(C)]
struct FilterInfo {
    id: *const c_char,
    kind: c_int,
    output_flags: u32,
    get_name: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    create: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
    destroy: unsafe extern "C" fn(*mut c_void),
    get_width: Unused,
    get_height: Unused,
    get_defaults: Option<unsafe extern "C" fn(*mut c_void)>,
    get_properties: Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void>,
    update: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    activate: Unused,
    deactivate: Unused,
    show: Unused,
    hide: Unused,
    video_tick: Option<unsafe extern "C" fn(*mut c_void, f32)>,
    video_render: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    filter_video: Unused,
    filter_audio: Option<unsafe extern "C" fn(*mut c_void, *mut AudioData) -> *mut AudioData>,
    enum_active_sources: Unused,
    save: Unused,
    load: Unused,
    mouse_click: Unused,
    mouse_move: Unused,
    mouse_wheel: Unused,
    focus: Unused,
    key_click: Unused,
    filter_remove: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    type_data: *mut c_void,
    free_type_data: Unused,
    audio_render: Unused,
    enum_all_sources: Unused,
    transition_start: Unused,
    transition_stop: Unused,
    get_defaults2: Unused,
    get_properties2: Unused,
    audio_mix: Unused,
    icon_type: c_int,
    media: [Unused; 9],
    version: u32,
    unversioned_id: *const c_char,
    missing_files: Unused,
    video_get_color_space: Unused,
    filter_add: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
}

// The offsets of libobs's struct on 64-bit hosts (checked against the C
// header with offsetof).
const _: () = {
    assert!(offset_of!(FilterInfo, video_tick) == 112);
    assert!(offset_of!(FilterInfo, filter_remove) == 208);
    assert!(offset_of!(FilterInfo, icon_type) == 288);
    assert!(offset_of!(FilterInfo, version) == 368);
    assert!(offset_of!(FilterInfo, filter_add) == 400);
    assert!(std::mem::size_of::<FilterInfo>() == 408);
};

const OBS_SOURCE_TYPE_FILTER: c_int = 1;
const OBS_SOURCE_VIDEO: u32 = 1;
const OBS_SOURCE_AUDIO: u32 = 2;
const OBS_PROPERTIES_DEFER_UPDATE: u32 = 1;
const OBS_TEXT_DEFAULT: c_int = 0;
const GS_BGRA: c_int = 5;
const GS_ZS_NONE: c_int = 0;
const GS_CLEAR_COLOR: u32 = 1;
const GS_BLEND_ZERO: c_int = 0;
const GS_BLEND_ONE: c_int = 1;

/// The stream of one filter.
#[derive(Default)]
struct Stream {
    sender: Option<Sender<'static>>,
    name: String,
    groups: String,
    /// The source whose "rename" signal is connected (`${source}`).
    watched_parent: Option<SendPtr>,
    /// The filter's own "rename" signal is connected (`${filter}`).
    watched_self: bool,
    /// The size the sender's video is set to.
    size: (u32, u32),
}

/// The graphics thread's objects of a video filter.
struct Gfx {
    texrender: *mut c_void,
    surfaces: [*mut c_void; 2],
    /// The OBS frame time of the frame in each surface.
    stamps: [u64; 2],
    readback: Readback,
}

struct SendPtr(*mut c_void);
// SAFETY: a source pointer, used only while the source lives (the filter is
// closed in filter_remove, before its parent goes).
unsafe impl Send for SendPtr {}

struct Filter {
    source: *mut c_void,
    video: bool,
    stream: Mutex<Stream>,
    gfx: Mutex<Gfx>,
    /// Rendered in this frame (a source can be rendered more than once:
    /// program, preview, projectors); reset in `video_tick`.
    rendered: AtomicBool,
}

unsafe fn text(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

unsafe extern "C" fn video_name(_: *mut c_void) -> *const c_char {
    c"AVIP (NDI® output)".as_ptr()
}

unsafe extern "C" fn audio_name(_: *mut c_void) -> *const c_char {
    c"AVIP (NDI® output, audio only)".as_ptr()
}

unsafe extern "C" fn get_defaults(settings: *mut c_void) {
    let name = CString::new(filter::DEFAULT_NAME).unwrap();
    obs_data_set_default_string(settings, c"ndi_filter_ndiname".as_ptr(), name.as_ptr());
    obs_data_set_default_string(settings, c"ndi_filter_ndigroups".as_ptr(), c"".as_ptr());
}

unsafe extern "C" fn get_properties(_: *mut c_void) -> *mut c_void {
    let props = obs_properties_create();
    // Apply on OK, not on every key press (each apply announces a new stream).
    obs_properties_set_flags(props, OBS_PROPERTIES_DEFER_UPDATE);
    let name = obs_properties_add_text(
        props,
        c"ndi_filter_ndiname".as_ptr(),
        c"NDI® name".as_ptr(),
        OBS_TEXT_DEFAULT,
    );
    obs_property_set_long_description(
        name,
        c"${source} is the source's name, ${filter} this filter's. These characters become spaces: \\ / : * ? \" < > |".as_ptr(),
    );
    obs_properties_add_text(
        props,
        c"ndi_filter_ndigroups".as_ptr(),
        c"NDI® groups".as_ptr(),
        OBS_TEXT_DEFAULT,
    );
    props
}

unsafe fn create(source: *mut c_void, video: bool) -> *mut c_void {
    // The parent is not known yet: the stream opens in filter_add.
    Box::into_raw(Box::new(Filter {
        source,
        video,
        stream: Mutex::new(Stream::default()),
        gfx: Mutex::new(Gfx {
            texrender: null_mut(),
            surfaces: [null_mut(); 2],
            stamps: [0; 2],
            readback: Readback::default(),
        }),
        rendered: AtomicBool::new(false),
    }))
    .cast()
}

unsafe extern "C" fn create_video(_settings: *mut c_void, source: *mut c_void) -> *mut c_void {
    create(source, true)
}

unsafe extern "C" fn create_audio(_settings: *mut c_void, source: *mut c_void) -> *mut c_void {
    create(source, false)
}

/// End the stream (the rename signals stay as they are).
unsafe fn stop(s: &mut Stream) {
    if s.sender.take().is_some() {
        log(300, &format!("filter stream '{}' stopped", s.name));
    }
    s.size = (0, 0);
}

/// Connect or disconnect the "rename" signals of the parent and the filter
/// so they match `parent` and the tokens of the name. Runs outside the
/// stream lock: a rename signal calls `on_rename`, which takes it.
unsafe fn watch(f: &Filter, parent: *mut c_void, source: bool, own: bool) {
    let data = (f as *const Filter).cast_mut().cast();
    let (old_parent, old_own) = {
        let mut s = f.stream.lock().unwrap();
        let want_parent = (source && !parent.is_null()).then_some(SendPtr(parent));
        (
            std::mem::replace(&mut s.watched_parent, want_parent).map(|p| p.0),
            std::mem::replace(&mut s.watched_self, own),
        )
    };
    let new_parent = (source && !parent.is_null()).then_some(parent);
    if old_parent != new_parent {
        if let Some(p) = old_parent {
            let sh = obs_source_get_signal_handler(p);
            if !sh.is_null() {
                signal_handler_disconnect(sh, c"rename".as_ptr(), on_rename, data);
            }
        }
        if let Some(p) = new_parent {
            let sh = obs_source_get_signal_handler(p);
            if !sh.is_null() {
                signal_handler_connect(sh, c"rename".as_ptr(), on_rename, data);
            }
        }
    }
    if old_own != own {
        let sh = obs_source_get_signal_handler(f.source);
        if !sh.is_null() {
            if own {
                signal_handler_connect(sh, c"rename".as_ptr(), on_rename, data);
            } else {
                signal_handler_disconnect(sh, c"rename".as_ptr(), on_rename, data);
            }
        }
    }
}

/// (Re)start the stream from the filter's settings and the current names.
/// `settings` is the filter's settings, or null to read them. Returns the
/// stream name's tokens (source, filter) for `watch`.
unsafe fn restart(f: &Filter, parent: *mut c_void, settings: *mut c_void) -> (bool, bool) {
    if parent.is_null() {
        stop(&mut f.stream.lock().unwrap());
        return (false, false);
    }
    let own = settings.is_null();
    let settings = if own {
        obs_source_get_settings(f.source)
    } else {
        settings
    };
    let template = text(obs_data_get_string(
        settings,
        c"ndi_filter_ndiname".as_ptr(),
    ));
    let groups = text(obs_data_get_string(
        settings,
        c"ndi_filter_ndigroups".as_ptr(),
    ));
    // DistroAV saves the template with the forbidden characters replaced.
    let clean = filter::sanitize(&template);
    if clean != template {
        if let Ok(c) = CString::new(clean) {
            obs_data_set_string(settings, c"ndi_filter_ndiname".as_ptr(), c.as_ptr());
        }
    }
    if own {
        obs_data_release(settings);
    }
    let name = filter::stream_name(
        &template,
        &text(obs_source_get_name(parent)),
        &text(obs_source_get_name(f.source)),
    );
    let tokens = (name.watch_source, name.watch_filter);
    let mut s = f.stream.lock().unwrap();
    // Same name and groups (a save that changed nothing, enable or
    // disable): keep the stream, receivers stay connected.
    if s.sender.is_some() && s.name == name.name && s.groups == groups {
        return tokens;
    }
    stop(&mut s);
    let Some(api) = NDI.get() else {
        return tokens;
    };
    let (Ok(cname), Ok(cgroups)) = (
        CString::new(name.name.clone()),
        CString::new(groups.clone()),
    ) else {
        return tokens;
    };
    let groups = (!cgroups.as_bytes().is_empty()).then_some(cgroups.as_c_str());
    // The video size is set at the first rendered frame.
    match Sender::create(api, &cname, groups, OutFormat::Bgra, 0, 0, (0, 1)) {
        Some(sender) => {
            log(
                300,
                &format!(
                    "filter stream '{}' started ({})",
                    name.name,
                    if f.video {
                        "video and audio"
                    } else {
                        "audio only"
                    }
                ),
            );
            s.sender = Some(sender);
            s.name = name.name;
            s.groups = cgroups.to_string_lossy().into_owned();
        }
        None => log(
            100,
            &format!("filter stream '{}': NDIlib_send_create failed", name.name),
        ),
    }
    tokens
}

/// Start the stream and connect the signals its name needs.
unsafe fn open(f: &Filter, parent: *mut c_void, settings: *mut c_void) {
    let (source, own) = restart(f, parent, settings);
    watch(f, parent, source, own);
}

/// End the stream and disconnect every signal.
unsafe fn close(f: &Filter) {
    stop(&mut f.stream.lock().unwrap());
    watch(f, null_mut(), false, false);
}

unsafe extern "C" fn on_rename(data: *mut c_void, _calldata: *mut c_void) {
    // Inside the signal: only the stream restarts, the signals stay (the
    // template, and so the tokens, did not change).
    let f = &*data.cast::<Filter>();
    restart(f, obs_filter_get_parent(f.source), null_mut());
}

unsafe extern "C" fn filter_add(data: *mut c_void, parent: *mut c_void) {
    let f = &*data.cast::<Filter>();
    open(f, parent, null_mut());
}

unsafe extern "C" fn filter_remove(data: *mut c_void, _parent: *mut c_void) {
    close(&*data.cast::<Filter>());
}

unsafe extern "C" fn update(data: *mut c_void, settings: *mut c_void) {
    let f = &*data.cast::<Filter>();
    // Before filter_add (at load) the parent is null: nothing to open yet.
    let parent = obs_filter_get_parent(f.source);
    if !parent.is_null() {
        open(f, parent, settings);
    }
}

unsafe extern "C" fn destroy(data: *mut c_void) {
    let f = Box::from_raw(data.cast::<Filter>());
    close(&f);
    let g = f.gfx.lock().unwrap();
    if !g.texrender.is_null() || g.surfaces.iter().any(|p| !p.is_null()) {
        obs_enter_graphics();
        for s in g.surfaces {
            if !s.is_null() {
                gs_stagesurface_destroy(s);
            }
        }
        if !g.texrender.is_null() {
            gs_texrender_destroy(g.texrender);
        }
        obs_leave_graphics();
    }
}

unsafe extern "C" fn video_tick(data: *mut c_void, _seconds: f32) {
    (*data.cast::<Filter>())
        .rendered
        .store(false, Ordering::Relaxed);
}

unsafe extern "C" fn video_render(data: *mut c_void, _effect: *mut c_void) {
    let f = &*data.cast::<Filter>();
    // The scene always gets the source unchanged.
    obs_source_skip_video_filter(f.source);
    if f.rendered.swap(true, Ordering::Relaxed) {
        return;
    }
    let parent = obs_filter_get_parent(f.source);
    let target = obs_filter_get_target(f.source);
    let mut g = f.gfx.lock().unwrap();
    if parent.is_null() || target.is_null() {
        g.readback.idle();
        return;
    }
    let (width, height) = (
        obs_source_get_width(f.source),
        obs_source_get_height(f.source),
    );
    let showing = obs_source_showing(parent);
    if !filter::sends_video(obs_source_enabled(f.source), showing, width, height) {
        g.readback.idle();
        return;
    }
    let step = g.readback.frame(width, height);
    if step.recreate {
        for s in &mut g.surfaces {
            if !s.is_null() {
                gs_stagesurface_destroy(*s);
            }
            *s = gs_stagesurface_create(width, height, GS_BGRA);
        }
        if g.texrender.is_null() {
            g.texrender = gs_texrender_create(GS_BGRA, GS_ZS_NONE);
        }
    }
    if g.texrender.is_null() || g.surfaces.iter().any(|s| s.is_null()) {
        return;
    }
    gs_texrender_reset(g.texrender);
    if !gs_texrender_begin(g.texrender, width, height) {
        g.readback.idle();
        return;
    }
    gs_clear(GS_CLEAR_COLOR, &Vec4([0.0; 4]), 0.0, 0);
    gs_ortho(0.0, width as f32, 0.0, height as f32, -100.0, 100.0);
    gs_blend_state_push();
    gs_blend_function(GS_BLEND_ONE, GS_BLEND_ZERO);
    if target == parent {
        // This is the first filter: the source without filters.
        obs_source_skip_video_filter(f.source);
    } else {
        obs_source_video_render(target);
    }
    gs_blend_state_pop();
    gs_texrender_end(g.texrender);
    gs_stage_texture(
        g.surfaces[step.stage],
        gs_texrender_get_texture(g.texrender),
    );
    g.stamps[step.stage] = obs_get_video_frame_time();
    let Some(read) = step.read else {
        return;
    };
    let (mut ptr, mut linesize) = (null_mut::<u8>(), 0u32);
    if !gs_stagesurface_map(g.surfaces[read], &mut ptr, &mut linesize) {
        return;
    }
    {
        let mut s = f.stream.lock().unwrap();
        let fps = {
            let info = &*video_output_get_info(obs_get_video());
            (info.fps_num, info.fps_den)
        };
        let resize = (width, height) != s.size;
        if let Some(sender) = s.sender.as_mut() {
            if resize {
                sender.set_video(OutFormat::Bgra, width, height, fps);
            }
            s.size = (width, height);
        }
        let name = s.name.clone();
        if let Some(sender) = s.sender.as_mut() {
            let len = linesize as usize * height as usize;
            let v = VideoIn {
                planes: [std::slice::from_raw_parts(ptr, len), &[], &[]],
                linesize: [linesize, 0, 0],
                timestamp_ns: g.stamps[read],
            };
            sender.video(&v);
            if let Some(n) = sender.connections(os_gettime_ns()) {
                log(300, &format!("filter stream '{name}': {n} receiver(s)"));
            }
        }
    }
    gs_stagesurface_unmap(g.surfaces[read]);
}

unsafe extern "C" fn filter_audio(data: *mut c_void, audio: *mut AudioData) -> *mut AudioData {
    let f = &*data.cast::<Filter>();
    if audio.is_null() {
        return audio;
    }
    let a = &*audio;
    let mut s = f.stream.lock().unwrap();
    let name = s.name.clone();
    let video = f.video;
    let Some(sender) = s.sender.as_mut() else {
        return audio;
    };
    let out = obs_get_audio();
    let (rate, channels) = (
        audio_output_get_sample_rate(out),
        audio_output_get_channels(out).min(8),
    );
    let frames = a.frames as usize;
    let planes: Vec<&[f32]> = (0..channels)
        .take_while(|&i| !a.data[i].is_null())
        .map(|i| std::slice::from_raw_parts(a.data[i].cast::<f32>(), frames))
        .collect();
    if !planes.is_empty() && frames > 0 {
        sender.audio(&planes, a.frames, rate, a.timestamp);
    }
    // The video filter logs receivers from the render thread.
    if !video {
        if let Some(n) = sender.connections(os_gettime_ns()) {
            log(300, &format!("filter stream '{name}': {n} receiver(s)"));
        }
    }
    audio
}

fn info(id: &'static CStr, video: bool) -> FilterInfo {
    FilterInfo {
        id: id.as_ptr(),
        kind: OBS_SOURCE_TYPE_FILTER,
        output_flags: if video {
            OBS_SOURCE_VIDEO
        } else {
            OBS_SOURCE_AUDIO
        },
        get_name: if video { video_name } else { audio_name },
        create: if video { create_video } else { create_audio },
        destroy,
        get_width: None,
        get_height: None,
        get_defaults: Some(get_defaults),
        get_properties: Some(get_properties),
        update: Some(update),
        activate: None,
        deactivate: None,
        show: None,
        hide: None,
        video_tick: video.then_some(video_tick as unsafe extern "C" fn(*mut c_void, f32)),
        video_render: video
            .then_some(video_render as unsafe extern "C" fn(*mut c_void, *mut c_void)),
        filter_video: None,
        // Also on the video filter, as DistroAV (for async sources).
        filter_audio: Some(filter_audio),
        enum_active_sources: None,
        save: None,
        load: None,
        mouse_click: None,
        mouse_move: None,
        mouse_wheel: None,
        focus: None,
        key_click: None,
        filter_remove: Some(filter_remove),
        type_data: null_mut(),
        free_type_data: None,
        audio_render: None,
        enum_all_sources: None,
        transition_start: None,
        transition_stop: None,
        get_defaults2: None,
        get_properties2: None,
        audio_mix: None,
        icon_type: 0,
        media: [None; 9],
        version: 0,
        unversioned_id: std::ptr::null(),
        missing_files: None,
        video_get_color_space: None,
        filter_add: Some(filter_add),
    }
}

/// Register both filters (called from post_load, after the ids were found
/// free).
pub(crate) unsafe fn register() {
    debug_assert_eq!(keys::NAME, "ndi_filter_ndiname");
    debug_assert_eq!(keys::GROUPS, "ndi_filter_ndigroups");
    debug_assert_eq!(filter::VIDEO_ID, "ndi_filter");
    debug_assert_eq!(filter::AUDIO_ID, "ndi_audiofilter");
    let video = info(c"ndi_filter", true);
    obs_register_source_s((&raw const video).cast(), std::mem::size_of::<FilterInfo>());
    let audio = info(c"ndi_audiofilter", false);
    obs_register_source_s((&raw const audio).cast(), std::mem::size_of::<FilterInfo>());
}
