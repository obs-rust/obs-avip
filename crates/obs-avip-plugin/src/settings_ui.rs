//! Tools → "NDI Output Settings": the main and preview outputs and the tally
//! switches, in OBS's own properties window. No Qt.
//!
//! A private source of a hidden type carries the settings; the menu entry
//! fills it from `[NDIPlugin]` (DistroAV's section and keys, so a DistroAV
//! user's settings show up) and opens its properties. OBS calls `update` on
//! every change, and with the old values on Cancel, so the window writes
//! `[NDIPlugin]` at once. The outputs follow after the edits pause for
//! `APPLY_AFTER`, so typing a name does not announce every half-typed name
//! on the network. The tally switches apply at once.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use obs_avip::ndi::Api;

use crate::output::{self, ndi_plugin_string};
use crate::{
    log, ndi_plugin_bool, reload_tally, rtld_default, spawn_background, user_config, SourceInfo,
    NDI, SHUTDOWN,
};

extern "C" {
    fn obs_register_source_s(info: *const SourceInfo, size: usize);
    fn obs_source_create_private(
        id: *const c_char,
        name: *const c_char,
        settings: *mut c_void,
    ) -> *mut c_void;
    fn obs_source_get_settings(source: *mut c_void) -> *mut c_void;
    fn obs_data_release(data: *mut c_void);
    fn obs_data_get_bool(data: *mut c_void, name: *const c_char) -> bool;
    fn obs_data_get_string(data: *mut c_void, name: *const c_char) -> *const c_char;
    fn obs_data_set_bool(data: *mut c_void, name: *const c_char, val: bool);
    fn obs_data_set_string(data: *mut c_void, name: *const c_char, val: *const c_char);
    fn obs_properties_create() -> *mut c_void;
    fn obs_properties_add_bool(
        props: *mut c_void,
        name: *const c_char,
        desc: *const c_char,
    ) -> *mut c_void;
    fn obs_properties_add_text(
        props: *mut c_void,
        name: *const c_char,
        desc: *const c_char,
        kind: c_int,
    ) -> *mut c_void;
    fn obs_properties_add_group(
        props: *mut c_void,
        name: *const c_char,
        desc: *const c_char,
        kind: c_int,
        group: *mut c_void,
    ) -> *mut c_void;
    fn config_set_bool(config: *mut c_void, section: *const c_char, name: *const c_char, val: bool);
    fn config_set_string(
        config: *mut c_void,
        section: *const c_char,
        name: *const c_char,
        val: *const c_char,
    );
    fn config_save(config: *mut c_void) -> c_int;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn obs_queue_task(
        kind: c_int,
        task: unsafe extern "C" fn(*mut c_void),
        param: *mut c_void,
        wait: bool,
    );
}

/// `OBS_TASK_UI` (`enum obs_task_type` in libobs/obs.h)
const OBS_TASK_UI: c_int = 0;

const SETTINGS_ID: &CStr = c"obs_avip_output_settings";
/// `OBS_SOURCE_CAP_DISABLED`: not in the Add Source list.
const OBS_SOURCE_CAP_DISABLED: u32 = 1 << 10;
const OBS_TEXT_DEFAULT: c_int = 0;
const OBS_TEXT_INFO: c_int = 3;
const OBS_GROUP_NORMAL: c_int = 1;
const APPLY_AFTER: Duration = Duration::from_millis(800);

/// The window's keys are `[NDIPlugin]`'s.
const BOOLS: [(&CStr, bool); 4] = [
    (c"MainOutputEnabled", false),
    (c"PreviewOutputEnabled", false),
    (c"TallyProgramEnabled", true),
    (c"TallyPreviewEnabled", true),
];
const STRINGS: [(&CStr, &str); 4] = [
    (c"MainOutputName", "OBS PGM"),
    (c"MainOutputGroups", ""),
    (c"PreviewOutputName", "OBS Preview"),
    (c"PreviewOutputGroups", ""),
];

struct SettingsSource(*mut c_void);
// SAFETY: created and used on the frontend's thread; released at unload.
unsafe impl Send for SettingsSource {}

static SETTINGS: Mutex<Option<SettingsSource>> = Mutex::new(None);
/// Bumped on every change; the delayed apply runs only for the newest one.
static GENERATION: AtomicU64 = AtomicU64::new(0);

unsafe fn text(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

unsafe extern "C" fn get_name(_: *mut c_void) -> *const c_char {
    c"AVIP: NDI® Output Settings".as_ptr()
}

unsafe extern "C" fn create(_: *mut c_void, _: *mut c_void) -> *mut c_void {
    // libobs needs non-null data; this type keeps nothing of its own.
    Box::into_raw(Box::new(0u8)).cast()
}

unsafe extern "C" fn destroy(data: *mut c_void) {
    drop(Box::from_raw(data.cast::<u8>()));
}

unsafe fn group(props: *mut c_void, name: &CStr, title: &CStr, add: impl FnOnce(*mut c_void)) {
    let sub = obs_properties_create();
    add(sub);
    obs_properties_add_group(props, name.as_ptr(), title.as_ptr(), OBS_GROUP_NORMAL, sub);
}

unsafe extern "C" fn get_properties(_: *mut c_void) -> *mut c_void {
    let props = obs_properties_create();
    for (prefix, title) in [
        ("Main", c"Main output (program)"),
        ("Preview", c"Preview output (studio mode preview)"),
    ] {
        let key = |k: &str| CString::new(format!("{prefix}Output{k}")).unwrap_or_default();
        let group_name = CString::new(format!("{prefix}Group")).unwrap_or_default();
        group(props, &group_name, title, |g| {
            obs_properties_add_bool(g, key("Enabled").as_ptr(), c"Send over NDI".as_ptr());
            obs_properties_add_text(
                g,
                key("Name").as_ptr(),
                c"NDI name".as_ptr(),
                OBS_TEXT_DEFAULT,
            );
            obs_properties_add_text(
                g,
                key("Groups").as_ptr(),
                c"NDI groups (comma-separated, empty = public)".as_ptr(),
                OBS_TEXT_DEFAULT,
            );
        });
    }
    group(props, c"TallyGroup", c"Tally to NDI senders", |g| {
        obs_properties_add_bool(
            g,
            c"TallyProgramEnabled".as_ptr(),
            c"Program (red)".as_ptr(),
        );
        obs_properties_add_bool(
            g,
            c"TallyPreviewEnabled".as_ptr(),
            c"Preview (green)".as_ptr(),
        );
    });
    let runtime = NDI.get().map_or("not loaded".to_string(), |l| {
        format!("{} ({})", l.version(), l.path)
    });
    let info = CString::new(format!(
        "NDI runtime: {runtime}\nSaved in [NDIPlugin] of the profile's user settings, the same keys as DistroAV.\nNDI® is a registered trademark of Vizrt NDI AB. More about NDI: https://ndi.video"
    ))
    .unwrap_or_default();
    obs_properties_add_text(props, c"Info".as_ptr(), info.as_ptr(), OBS_TEXT_INFO);
    props
}

/// Write the window's values to `[NDIPlugin]`; `true` when something changed.
unsafe fn write(settings: *mut c_void) -> (bool, bool) {
    let config = user_config();
    if config.is_null() {
        return (false, false);
    }
    let (mut outputs, mut tally) = (false, false);
    for (key, default) in BOOLS {
        let v = obs_data_get_bool(settings, key.as_ptr());
        if ndi_plugin_bool(config, key, default) != v {
            config_set_bool(config, c"NDIPlugin".as_ptr(), key.as_ptr(), v);
            if key.to_bytes().starts_with(b"Tally") {
                tally = true;
            } else {
                outputs = true;
            }
        }
    }
    for (key, default) in STRINGS {
        let v = text(obs_data_get_string(settings, key.as_ptr()));
        if ndi_plugin_string(config, key, default) != v {
            let cv = CString::new(v).unwrap_or_default();
            config_set_string(config, c"NDIPlugin".as_ptr(), key.as_ptr(), cv.as_ptr());
            outputs = true;
        }
    }
    if outputs || tally {
        config_save(config);
    }
    (outputs, tally)
}

unsafe extern "C" fn update(_: *mut c_void, settings: *mut c_void) {
    let (outputs, tally) = write(settings);
    if tally {
        reload_tally();
    }
    if outputs {
        // Wait for more edits off the UI thread, then apply on the UI
        // thread: the outputs are opened and closed there (frontend events),
        // so the two never race.
        let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        spawn_background("avip-apply", move || {
            let step = Duration::from_millis(50);
            let mut waited = Duration::ZERO;
            while waited < APPLY_AFTER {
                if SHUTDOWN.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(step);
                waited += step;
            }
            if GENERATION.load(Ordering::SeqCst) == generation {
                unsafe {
                    obs_queue_task(OBS_TASK_UI, apply_task, generation as *mut c_void, false)
                };
            }
        });
    }
}

/// On the UI thread: apply the output settings, unless a later edit is
/// waiting or OBS is shutting down (the outputs are already closed).
unsafe extern "C" fn apply_task(param: *mut c_void) {
    if SHUTDOWN.load(Ordering::SeqCst) || GENERATION.load(Ordering::SeqCst) != param as u64 {
        return;
    }
    output::apply_config();
    log(300, "output settings applied");
}

/// Fill the settings source from `[NDIPlugin]` (it may have changed from
/// the websocket or the outputs' write-back since the window was last open).
unsafe fn fill(source: *mut c_void) {
    let config = user_config();
    let settings = obs_source_get_settings(source);
    for (key, default) in BOOLS {
        obs_data_set_bool(
            settings,
            key.as_ptr(),
            ndi_plugin_bool(config, key, default),
        );
    }
    for (key, default) in STRINGS {
        let v = CString::new(ndi_plugin_string(config, key, default)).unwrap_or_default();
        obs_data_set_string(settings, key.as_ptr(), v.as_ptr());
    }
    obs_data_release(settings);
}

unsafe extern "C" fn open_window(_: *mut c_void) {
    let source = {
        let mut s = SETTINGS.lock().unwrap();
        if s.is_none() {
            let src = obs_source_create_private(
                SETTINGS_ID.as_ptr(),
                c"AVIP: NDI® Output Settings".as_ptr(),
                null_mut(),
            );
            if src.is_null() {
                log(100, "the settings window could not be created");
                return;
            }
            *s = Some(SettingsSource(src));
        }
        s.as_ref().map(|s| s.0).unwrap_or(null_mut())
    };
    fill(source);
    let open = dlsym(
        rtld_default(),
        c"obs_frontend_open_source_properties".as_ptr(),
    );
    if open.is_null() {
        log(100, "this OBS has no obs_frontend_open_source_properties");
        return;
    }
    let open: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(open);
    open(source);
}

/// Register the settings type and add the Tools menu entry.
pub(crate) unsafe fn register() {
    let info = SourceInfo {
        id: SETTINGS_ID.as_ptr(),
        kind: 0, // OBS_SOURCE_TYPE_INPUT
        output_flags: OBS_SOURCE_CAP_DISABLED,
        get_name,
        create,
        destroy,
        get_width: None,
        get_height: None,
        get_defaults: None,
        get_properties: Some(get_properties),
        update,
        activate: None,
        deactivate: None,
        show: None,
        hide: None,
    };
    obs_register_source_s(&info, std::mem::size_of::<SourceInfo>());
    let add = dlsym(rtld_default(), c"obs_frontend_add_tools_menu_item".as_ptr());
    if add.is_null() {
        return;
    }
    let add: unsafe extern "C" fn(*const c_char, unsafe extern "C" fn(*mut c_void), *mut c_void) =
        std::mem::transmute(add);
    add(
        c"AVIP: NDI® Output Settings".as_ptr(),
        open_window,
        null_mut(),
    );
}

/// Release the settings source (unload).
pub(crate) unsafe fn close() {
    extern "C" {
        fn obs_source_release(source: *mut c_void);
    }
    if let Some(SettingsSource(src)) = SETTINGS.lock().unwrap().take() {
        obs_source_release(src);
    }
}
