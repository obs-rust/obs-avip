//! Tools → "NDI Network Settings": the NDI runtime's network settings
//! (discovery, groups, transports, multicast, adapters) in OBS's own
//! properties window. No Qt. The same pattern as `settings_ui`: a private
//! source of a hidden type carries the values.
//!
//! The values live in `[AVIPNetwork]` of the user settings. With "obs-avip
//! settings" selected, obs-avip writes them as `ndi-config.v1.json` into its
//! plugin config folder and, at the next start, points `NDI_CONFIG_DIR` there
//! before it loads the runtime (`prepare`). The runtime reads its settings
//! once per process, so every change applies at the next OBS start
//! (`obs_avip::netconfig`).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr::null_mut;
use std::sync::{Mutex, OnceLock};

use obs_avip::netconfig::{
    self, ConfigDir, Kind, Started, FIELDS, SECTION, SOURCE_KEY, SOURCE_OWN, SOURCE_SYSTEM,
    TRI_DEFAULT, TRI_OFF, TRI_ON,
};

use crate::{log, obs_current_module, user_config, SourceInfo};

#[cfg_attr(windows, link(name = "obs", kind = "raw-dylib"))]
extern "C" {
    fn obs_register_source_s(info: *const SourceInfo, size: usize);
    fn obs_source_create_private(
        id: *const c_char,
        name: *const c_char,
        settings: *mut c_void,
    ) -> *mut c_void;
    fn obs_source_get_settings(source: *mut c_void) -> *mut c_void;
    fn obs_data_release(data: *mut c_void);
    fn obs_data_get_string(data: *mut c_void, name: *const c_char) -> *const c_char;
    fn obs_data_set_string(data: *mut c_void, name: *const c_char, val: *const c_char);
    fn obs_data_set_default_string(data: *mut c_void, name: *const c_char, val: *const c_char);
    fn obs_properties_create() -> *mut c_void;
    fn obs_properties_get(props: *mut c_void, name: *const c_char) -> *mut c_void;
    fn obs_properties_add_text(
        props: *mut c_void,
        name: *const c_char,
        desc: *const c_char,
        kind: c_int,
    ) -> *mut c_void;
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
    fn obs_properties_add_group(
        props: *mut c_void,
        name: *const c_char,
        desc: *const c_char,
        kind: c_int,
        group: *mut c_void,
    ) -> *mut c_void;
    fn obs_property_set_enabled(p: *mut c_void, enabled: bool);
    fn obs_property_set_visible(p: *mut c_void, visible: bool);
    fn obs_property_set_modified_callback(
        p: *mut c_void,
        modified: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool,
    );
    fn config_get_string(
        config: *mut c_void,
        section: *const c_char,
        name: *const c_char,
    ) -> *const c_char;
    fn config_set_string(
        config: *mut c_void,
        section: *const c_char,
        name: *const c_char,
        val: *const c_char,
    );
    fn config_save(config: *mut c_void) -> c_int;
    fn obs_module_get_config_path(module: *mut c_void, file: *const c_char) -> *mut c_char;
    fn bfree(ptr: *mut c_void);
}

const SETTINGS_ID: &CStr = c"obs_avip_network_settings";
const TITLE: &CStr = c"AVIP: NDI® Network Settings";
/// `OBS_SOURCE_CAP_DISABLED`: not in the Add Source list.
const OBS_SOURCE_CAP_DISABLED: u32 = 1 << 10;
const OBS_TEXT_DEFAULT: c_int = 0;
const OBS_TEXT_INFO: c_int = 3;
const OBS_GROUP_NORMAL: c_int = 1;
const OBS_COMBO_TYPE_LIST: c_int = 2;
const OBS_COMBO_FORMAT_STRING: c_int = 3;

/// The window's groups: (property name, title, the fields in it).
const GROUPS: [(&CStr, &CStr, &[&str]); 3] = [
    (
        c"DiscoveryGroup",
        c"Discovery and groups",
        &[
            "Discovery",
            "ExtraIps",
            "RecvGroups",
            "SendGroups",
            "SourceFilter",
        ],
    ),
    (
        c"TransportGroup",
        c"Transport (sender and receiver agree on the best one both allow)",
        &[
            "RudpRecv",
            "RudpSend",
            "TcpRecv",
            "TcpSend",
            "UdpRecv",
            "UdpSend",
            "MulticastRecv",
            "MulticastSubnets",
        ],
    ),
    (c"AdapterGroup", c"Network adapters", &["Adapters"]),
];

const ADAPTERS_KEY: &str = "Adapters";

/// The IPv4 addresses of this machine's network adapters, without loopback:
/// (interface name, address).
fn local_adapters() -> Vec<(String, String)> {
    match if_addrs::get_if_addrs() {
        Ok(list) => list
            .into_iter()
            .filter(|i| !i.is_loopback() && i.ip().is_ipv4())
            .map(|i| {
                let ip = i.ip().to_string();
                (i.name, ip)
            })
            .collect(),
        Err(e) => {
            unsafe { log(200, &format!("network adapters not listed: {e}")) };
            Vec::new()
        }
    }
}

/// The adapter field: a list with "All adapters" and this machine's
/// adapters (`netconfig::adapter_choices`).
unsafe fn adapter_list(props: *mut c_void, key: &CStr) -> *mut c_void {
    let l = obs_properties_add_list(
        props,
        key.as_ptr(),
        label(ADAPTERS_KEY).as_ptr(),
        OBS_COMBO_TYPE_LIST,
        OBS_COMBO_FORMAT_STRING,
    );
    let saved = ini(user_config(), ADAPTERS_KEY, "");
    for (desc, val) in netconfig::adapter_choices(&local_adapters(), &saved) {
        obs_property_list_add_string(l, ckey(&desc).as_ptr(), ckey(&val).as_ptr());
    }
    l
}

fn label(key: &str) -> &'static CStr {
    match key {
        "Discovery" => c"Discovery servers (comma-separated, host or host:port)",
        "ExtraIps" => c"Extra source IPs (senders that mDNS does not find)",
        "RecvGroups" => c"Receive groups (comma-separated, empty = Public)",
        "SendGroups" => c"Send groups for outputs without groups of their own",
        "SourceFilter" => c"Source name filter (regular expression)",
        "RudpRecv" => c"Reliable UDP: receive",
        "RudpSend" => c"Reliable UDP: send",
        "TcpRecv" => c"Multi-TCP: receive",
        "TcpSend" => c"Multi-TCP: send",
        "UdpRecv" => c"UDP: receive",
        "UdpSend" => c"UDP: send",
        "MulticastRecv" => c"Multicast: receive",
        "MulticastSubnets" => c"Multicast: sender subnets (comma-separated)",
        "Adapters" => c"Network adapter NDI may use",
        _ => c"?",
    }
}

struct SettingsSource(*mut c_void);
// SAFETY: created and used on the frontend's thread; released at unload.
unsafe impl Send for SettingsSource {}

static SETTINGS: Mutex<Option<SettingsSource>> = Mutex::new(None);
/// What the runtime uses in this process, decided once in `prepare`.
static IN_EFFECT: OnceLock<String> = OnceLock::new();
/// What the runtime took at this start (`netconfig::restart_needed`).
static STARTED: OnceLock<Started> = OnceLock::new();

fn ckey(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

unsafe fn text(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

/// A value of `[AVIPNetwork]`, or `default` when the user config is missing
/// or has no value.
unsafe fn ini(config: *mut c_void, key: &str, default: &str) -> String {
    if config.is_null() {
        return default.to_string();
    }
    let k = ckey(key);
    let v = text(config_get_string(
        config,
        ckey(SECTION).as_ptr(),
        k.as_ptr(),
    ));
    if v.is_empty() {
        default.to_string()
    } else {
        v
    }
}

fn field_default(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "",
        Kind::Tri => TRI_DEFAULT,
    }
}

/// obs-avip's config folder for the runtime's file.
unsafe fn own_dir() -> Option<String> {
    let p = obs_module_get_config_path(obs_current_module(), c"ndi".as_ptr());
    if p.is_null() {
        return None;
    }
    let dir = text(p);
    bfree(p.cast());
    // Absolute: in Windows portable mode libobs gives a path relative to
    // the working directory, and NDI_CONFIG_DIR must not depend on it.
    std::path::absolute(&dir)
        .ok()
        .map(|d| d.to_string_lossy().into_owned())
}

/// Write the runtime's config file from `[AVIPNetwork]`; returns its text.
unsafe fn write_file(config: *mut c_void, dir: &str) -> std::io::Result<String> {
    let json = netconfig::to_json(|k| {
        let kind = FIELDS
            .iter()
            .find(|f| f.key == k)
            .map_or(Kind::Text, |f| f.kind);
        ini(config, k, field_default(kind))
    });
    std::fs::create_dir_all(dir)?;
    let path = format!("{}/{}", dir.trim_end_matches('/'), netconfig::FILE_NAME);
    std::fs::write(&path, &json)?;
    Ok(json)
}

/// Before the runtime loads (`obs_module_post_load`): decide where its
/// settings come from and, for obs-avip's settings, write the file and set
/// `NDI_CONFIG_DIR`.
pub(crate) unsafe fn prepare() {
    let config = user_config();
    let use_own = ini(config, SOURCE_KEY, SOURCE_SYSTEM) == SOURCE_OWN;
    let env = std::env::var("NDI_CONFIG_DIR").ok();
    let dir = own_dir().unwrap_or_default();
    let mut choice = netconfig::choose(use_own, env.as_deref(), &dir);
    let mut started = match &choice {
        ConfigDir::UserEnv(_) => Started::UserEnv,
        _ => Started::System,
    };
    if let ConfigDir::Own(d) = &choice {
        match write_file(config, d) {
            Ok(json) => {
                started = Started::Own(json);
                // The runtime has no API for these settings: it reads
                // NDI_CONFIG_DIR when it initializes, right after this.
                // Set on OBS's main thread during module loading, before any
                // thread of this module runs.
                std::env::set_var("NDI_CONFIG_DIR", d);
            }
            Err(e) => {
                log(200, &format!("network settings not written to {d}: {e}"));
                choice = ConfigDir::System;
            }
        }
    }
    let line = choice.describe(use_own);
    log(300, &line);
    let _ = IN_EFFECT.set(line);
    let _ = STARTED.set(started);
}

/// Whether the window's values (`get`: by key) differ from what the runtime
/// took at this start. False when obs-avip did not load the runtime.
fn restart_needed(get: impl Fn(&str) -> String) -> bool {
    let Some(started) = STARTED.get() else {
        return false;
    };
    let own = get(SOURCE_KEY) == SOURCE_OWN;
    netconfig::restart_needed(started, own, &netconfig::to_json(get))
}

/// Show the restart line when the window's values need a restart.
unsafe fn show_restart_hint(props: *mut c_void, settings: *mut c_void) {
    let hint = obs_properties_get(props, RESTART_HINT.as_ptr());
    if !hint.is_null() {
        let needed = restart_needed(|k| text(obs_data_get_string(settings, ckey(k).as_ptr())));
        obs_property_set_visible(hint, needed);
    }
}

/// Any value changed: update the restart line.
unsafe extern "C" fn value_changed(
    props: *mut c_void,
    _: *mut c_void,
    settings: *mut c_void,
) -> bool {
    show_restart_hint(props, settings);
    true
}

unsafe extern "C" fn get_name(_: *mut c_void) -> *const c_char {
    TITLE.as_ptr()
}

unsafe extern "C" fn create(_: *mut c_void, _: *mut c_void) -> *mut c_void {
    // libobs needs non-null data; this type keeps nothing of its own.
    Box::into_raw(Box::new(0u8)).cast()
}

unsafe extern "C" fn destroy(data: *mut c_void) {
    drop(Box::from_raw(data.cast::<u8>()));
}

unsafe extern "C" fn get_defaults(settings: *mut c_void) {
    obs_data_set_default_string(
        settings,
        ckey(SOURCE_KEY).as_ptr(),
        ckey(SOURCE_SYSTEM).as_ptr(),
    );
    for f in FIELDS {
        obs_data_set_default_string(
            settings,
            ckey(f.key).as_ptr(),
            ckey(field_default(f.kind)).as_ptr(),
        );
    }
}

/// The groups are only editable with "obs-avip settings" selected.
unsafe extern "C" fn source_changed(
    props: *mut c_void,
    _: *mut c_void,
    settings: *mut c_void,
) -> bool {
    let own = text(obs_data_get_string(settings, ckey(SOURCE_KEY).as_ptr())) == SOURCE_OWN;
    for (name, _, _) in GROUPS {
        let g = obs_properties_get(props, name.as_ptr());
        if !g.is_null() {
            obs_property_set_enabled(g, own);
        }
    }
    let hint = obs_properties_get(props, SYSTEM_HINT.as_ptr());
    if !hint.is_null() {
        obs_property_set_visible(hint, !own);
    }
    show_restart_hint(props, settings);
    true
}

/// The line under the selector while the system settings are used.
const SYSTEM_HINT: &CStr = c"SystemHint";
/// The line shown while the window's values differ from this start's.
const RESTART_HINT: &CStr = c"RestartHint";

unsafe extern "C" fn get_properties(_: *mut c_void) -> *mut c_void {
    let props = obs_properties_create();
    let source = obs_properties_add_list(
        props,
        ckey(SOURCE_KEY).as_ptr(),
        c"NDI settings from".as_ptr(),
        OBS_COMBO_TYPE_LIST,
        OBS_COMBO_FORMAT_STRING,
    );
    obs_property_list_add_string(
        source,
        c"The system (NDI Access Manager or NDI's own config file)".as_ptr(),
        ckey(SOURCE_SYSTEM).as_ptr(),
    );
    obs_property_list_add_string(
        source,
        c"obs-avip (the settings below)".as_ptr(),
        ckey(SOURCE_OWN).as_ptr(),
    );
    obs_property_set_modified_callback(source, source_changed);
    obs_properties_add_text(
        props,
        SYSTEM_HINT.as_ptr(),
        c"Using the system's NDI settings. Select obs-avip above to edit the settings below."
            .as_ptr(),
        OBS_TEXT_INFO,
    );
    let restart = obs_properties_add_text(
        props,
        RESTART_HINT.as_ptr(),
        c"Saved, but not in effect yet: restart OBS to apply.".as_ptr(),
        OBS_TEXT_INFO,
    );
    // The window opens with the saved values (`fill`).
    let config = user_config();
    obs_property_set_visible(
        restart,
        restart_needed(|k| {
            let kind = FIELDS
                .iter()
                .find(|f| f.key == k)
                .map_or(Kind::Text, |f| f.kind);
            let default = if k == SOURCE_KEY {
                SOURCE_SYSTEM
            } else {
                field_default(kind)
            };
            ini(config, k, default)
        }),
    );

    for (name, title, keys) in GROUPS {
        let sub = obs_properties_create();
        for key in keys {
            let Some(f) = FIELDS.iter().find(|f| f.key == *key) else {
                continue;
            };
            let k = ckey(f.key);
            let p = match f.kind {
                Kind::Text if f.key == ADAPTERS_KEY => adapter_list(sub, &k),
                Kind::Text => obs_properties_add_text(
                    sub,
                    k.as_ptr(),
                    label(f.key).as_ptr(),
                    OBS_TEXT_DEFAULT,
                ),
                Kind::Tri => {
                    let l = obs_properties_add_list(
                        sub,
                        k.as_ptr(),
                        label(f.key).as_ptr(),
                        OBS_COMBO_TYPE_LIST,
                        OBS_COMBO_FORMAT_STRING,
                    );
                    for (desc, val) in [
                        (c"Default (the NDI runtime decides)", TRI_DEFAULT),
                        (c"On", TRI_ON),
                        (c"Off", TRI_OFF),
                    ] {
                        obs_property_list_add_string(l, desc.as_ptr(), ckey(val).as_ptr());
                    }
                    l
                }
            };
            obs_property_set_modified_callback(p, value_changed);
        }
        obs_properties_add_group(props, name.as_ptr(), title.as_ptr(), OBS_GROUP_NORMAL, sub);
    }

    let in_effect = IN_EFFECT
        .get()
        .cloned()
        .unwrap_or_else(|| "NDI runtime not loaded by obs-avip".to_string());
    let file = own_dir().map_or(String::new(), |d| {
        format!("\nobs-avip's file: {d}/{}", netconfig::FILE_NAME)
    });
    let info = CString::new(format!(
        "In effect now: {in_effect}{file}\nChanges apply at the next OBS start: the NDI runtime reads its network settings once per process.\nNDI® is a registered trademark of Vizrt NDI AB. More about NDI: https://ndi.video"
    ))
    .unwrap_or_default();
    obs_properties_add_text(props, c"Info".as_ptr(), info.as_ptr(), OBS_TEXT_INFO);
    props
}

/// Save the window's values to `[AVIPNetwork]`, and the file when
/// "obs-avip settings" is selected. Applies at the next start.
unsafe extern "C" fn update(_: *mut c_void, settings: *mut c_void) {
    let config = user_config();
    if config.is_null() {
        return;
    }
    let section = ckey(SECTION);
    let mut changed = false;
    let keys = std::iter::once((SOURCE_KEY, SOURCE_SYSTEM))
        .chain(FIELDS.iter().map(|f| (f.key, field_default(f.kind))));
    for (key, default) in keys {
        let k = ckey(key);
        let v = text(obs_data_get_string(settings, k.as_ptr()));
        if ini(config, key, default) != v {
            config_set_string(config, section.as_ptr(), k.as_ptr(), ckey(&v).as_ptr());
            changed = true;
        }
    }
    if !changed {
        return;
    }
    config_save(config);
    if ini(config, SOURCE_KEY, SOURCE_SYSTEM) == SOURCE_OWN {
        if let Some(d) = own_dir() {
            if let Err(e) = write_file(config, &d) {
                log(200, &format!("network settings not written to {d}: {e}"));
            }
        }
    }
    log(
        300,
        "network settings saved; they apply at the next OBS start",
    );
}

/// Fill the settings source from `[AVIPNetwork]`.
unsafe fn fill(source: *mut c_void) {
    let config = user_config();
    let settings = obs_source_get_settings(source);
    let keys = std::iter::once((SOURCE_KEY, SOURCE_SYSTEM))
        .chain(FIELDS.iter().map(|f| (f.key, field_default(f.kind))));
    for (key, default) in keys {
        let v = ckey(&ini(config, key, default));
        obs_data_set_string(settings, ckey(key).as_ptr(), v.as_ptr());
    }
    obs_data_release(settings);
}

unsafe extern "C" fn open_window(_: *mut c_void) {
    let source = {
        let mut s = SETTINGS.lock().unwrap();
        if s.is_none() {
            let src = obs_source_create_private(SETTINGS_ID.as_ptr(), TITLE.as_ptr(), null_mut());
            if src.is_null() {
                log(100, "the network settings window could not be created");
                return;
            }
            *s = Some(SettingsSource(src));
        }
        s.as_ref().map(|s| s.0).unwrap_or(null_mut())
    };
    fill(source);
    let open = crate::frontend_sym(c"obs_frontend_open_source_properties");
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
        get_defaults: Some(get_defaults),
        get_properties: Some(get_properties),
        update,
        activate: None,
        deactivate: None,
        show: None,
        hide: None,
    };
    obs_register_source_s(&info, std::mem::size_of::<SourceInfo>());
    let add = crate::frontend_sym(c"obs_frontend_add_tools_menu_item");
    if add.is_null() {
        return;
    }
    let add: unsafe extern "C" fn(*const c_char, unsafe extern "C" fn(*mut c_void), *mut c_void) =
        std::mem::transmute(add);
    add(TITLE.as_ptr(), open_window, null_mut());
}

/// Release the settings source (unload).
pub(crate) unsafe fn close() {
    #[cfg_attr(windows, link(name = "obs", kind = "raw-dylib"))]
    extern "C" {
        fn obs_source_release(source: *mut c_void);
    }
    if let Some(SettingsSource(src)) = SETTINGS.lock().unwrap().take() {
        obs_source_release(src);
    }
}
