# Architecture

## Two crates

| Crate | Kind | What it holds |
|---|---|---|
| `crates/obs-avip` | library, no OBS types | The NDI API as a function table (`ndi::Api`), the runtime loader (`ndi::Lib`) and a mock (`ndi::mock::Mock`); the receiver with resume (`resume`); discovery (`finder`); the sender (`send`); frame and audio conversion (`convert`); DistroAV's settings keys, defaults and value rules (`settings`); the show/hide/timeout rules (`visibility`); tally (`tally`); the registration decision (`module`); the NDI network settings file, where the settings come from and when a restart is needed (`netconfig`); the output spec that every output is described by (content, video size and frame-rate divisor, audio), its settings keys and value rules, the frame gate, and the main and preview outputs as specs (`outputs`); the dedicated output filters' ids, keys, stream name rule, when they send video and the order of their two stage surfaces (`filter`). All unit tests are here. |
| `crates/obs-avip-plugin` | `cdylib` (`obs-avip.so`, `obs-avip.plugin`) | The OBS shell: libobs declarations, the source (`lib.rs`), the outputs (`output.rs`), the dedicated output filters (`filter.rs`), the output settings window (`settings_ui.rs`), the network settings window (`network_ui.rs`). It turns OBS callbacks into calls of the core and core results into libobs calls. |

The rule for new code: a decision goes into the core with a test against the mock; the shell only translates. The core makes no libobs calls and has no global state, so it can later move into the obs-rust tree as a crate without change (see [roadmap.md](roadmap.md)).

No NDI SDK file is in the repository. `ndi.rs` declares the structs and functions it uses, following the SDK's MIT-licensed headers (see NOTICE). The runtime is loaded with `dlopen`, on Windows with `LoadLibraryExW` (`ndi::candidates` gives the search order, NDI 6 only). A runtime in the plugin's own folder comes first, so a release can ship one. On Windows only full paths are tried (the plugin's folder, then `NDI_RUNTIME_DIR_V6`), never a bare name, which would search the working directory and `PATH`.

On Linux and macOS the libobs functions resolve when OBS loads the plugin (macOS: `-undefined dynamic_lookup` in `.cargo/config.toml`). On Windows a DLL must import them at link time: the `extern` blocks carry `#[cfg_attr(windows, link(name = "obs", kind = "raw-dylib"))]`, so the build needs no `obs.lib` and no OBS files.

## Loading

1. `obs_module_load` registers nothing: another NDI plugin may load after this one.
2. `obs_module_post_load` (after every module loaded) asks libobs whether `ndi_source`, `ndi_output`, `ndi_filter` or `ndi_audiofilter` already has a display name. If one has, another plugin (for example DistroAV) provides it: obs-avip logs one line and stops. It does not load the NDI runtime.
3. Otherwise it decides where the runtime takes its network settings from (`netconfig::choose`): a `NDI_CONFIG_DIR` that is already set, else obs-avip's settings if selected (it writes `ndi-config.v1.json` and sets `NDI_CONFIG_DIR` to its plugin config folder), else the system settings. Then it loads the NDI runtime, registers `ndi_source`, `ndi_output`, `ndi_filter`, `ndi_audiofilter` and the private settings sources `obs_avip_output_settings` and `obs_avip_network_settings`, and adds the two Tools menu items.
4. On `OBS_FRONTEND_EVENT_FINISHED_LOADING` it creates "NDI Main Output" and "NDI Preview Output" from `[NDIPlugin]` and starts the enabled ones.
5. `obs_module_unload` closes the outputs and the settings source and calls `NDIlib_destroy`.

The frontend functions (`obs_frontend_*`) are looked up at run time (`frontend_sym`: `dlsym` on Linux and macOS, `GetProcAddress` on `obs-frontend-api.dll` on Windows), so the plugin also loads without a frontend (then without the main output and the menu items).

libobs can give the plugin's path and its config folder relative to the working directory (Windows portable mode: `../../plugins/obs-avip/obs-avip.dll`). Both are made absolute before use.

## Network settings

The Standard NDI SDK has no API for its network settings. It reads one file, `ndi-config.v1.json`, from `$HOME/.ndi` or from `NDI_CONFIG_DIR`, once per process, at the first `NDIlib_initialize`. Checked with NDI 6.0.1 on macOS and 6.3.2 on Linux, with a recv group as the observable (the `dlclose` row: macOS only):

| After the first initialize | Settings used |
|---|---|
| File changed, new finder | the first ones |
| `NDI_CONFIG_DIR` changed, new finder | the first ones |
| `NDIlib_destroy`, then `NDIlib_initialize` | the first ones |
| `dlclose`, then `dlopen` of the runtime | the first ones (macOS keeps the image loaded: it has Objective-C and thread-local data) |

So obs-avip decides once, before it loads the runtime, and every change applies at the next start. The window compares its values with what the runtime took at this start (`netconfig::restart_needed`) and shows "restart OBS to apply" while they differ.

The file holds only the values the user set: a transport left on "Default" writes nothing, so the runtime keeps its own default. Groups passed in the API (an output's own groups, `NDIlib_find_create_v2`) win over the file's groups.

## Threads

| Thread | Started by | Work |
|---|---|---|
| `avip-recv` (one per running source) | show, activate, update | `recv_capture_v3` with 100 ms timeout, frames to `obs_source_output_video/audio` (interlaced fields woven first), resume (a finder of its own, made with the receiver, for the sender's address; while stalled a TCP probe of up to 300 ms every 2 s), tally, web control URL (once a second). Stopping it never waits (show, hide and update run on the video thread): its generation moves on, so its frames are dropped, and it ends by itself. Only `destroy` joins it. |
| `avip-find` | the source properties, source create | One discovery (until the list stops growing, 1 s per wait), at most every 5 s. Refreshes the open properties of every source. Joined at unload; stops after its current wait. |
| `avip-apply` | an edit in the output settings window | Waits 0.8 s for more edits, then queues the apply as a UI task (the outputs are opened and closed on the UI thread). Joined at unload. |
| OBS output threads | `ndi_output` start | `raw_video` lets every nth frame through (`outputs::FrameGate`, the frame-rate divisor), copies it into one of two buffers and calls `send_video_async_v2`; `raw_audio` calls `send_audio_v3`. The size comes from `obs_output_set_video_conversion` (always set, so a scaled size of an earlier start does not stay), the audio from `obs_output_set_mixer`. |
| OBS graphics thread | an `ndi_filter` that renders | `video_render` renders the filter's target into a texture once per frame (the scene gets the source unchanged), stages it into one of two stage surfaces and reads the surface staged in the frame before, so it never waits for the GPU; the frame goes to the filter's sender as BGRA at the source's size (`Sender::set_video` on a size change). |
| OBS audio thread | an `ndi_filter` or `ndi_audiofilter` on a source with its own audio | `filter_audio` sends the source's audio to the filter's sender and returns it unchanged. |

The receive thread passes the NDI frame buffer to `obs_source_output_video` and frees it after the call (libobs copies async frames).

## The output's frame copy

`NDIlib_send_send_video_async_v2` may read a frame until the next call. OBS reuses its frame buffer after `raw_video` returns. So the sender copies each frame into one of two owned buffers and alternates: handing over frame N+1 releases frame N. The copy also packs NV12 and I420 planes tightly, as NDI reads them, and the frame rate is OBS's exact `fps_num/fps_den`. The copy costs about 1 % of a core at 1080p.

## Settings compatibility

`settings::read` reproduces DistroAV's `ndi_source_update`: the integer values, the defaults (`settings::defaults`), the handling of unknown values, and the keys it writes back (an invalid behavior, the removed "Internal" sync mode, the alpha fix). The tests in `settings.rs` are the contract; each cites the DistroAV line it follows. Keys of our own: `ndi_resume` (bool, default on) and `arrival_ts` (bool, measurement only: frames get their arrival time).
