# Architecture

## Two crates

| Crate | Kind | What it holds |
|---|---|---|
| `crates/obs-avip` | library, no OBS types | The NDI API as a function table (`ndi::Api`), the runtime loader (`ndi::Lib`) and a mock (`ndi::mock::Mock`); the receiver with resume (`resume`); discovery (`finder`); the sender (`send`); frame and audio conversion (`convert`); DistroAV's settings keys, defaults and value rules (`settings`); the show/hide/timeout rules (`visibility`); tally (`tally`); the registration decision (`module`). All unit tests are here. |
| `crates/obs-avip-plugin` | `cdylib` (`obs-avip.so`, `obs-avip.plugin`) | The OBS shell: libobs declarations, the source (`lib.rs`), the outputs (`output.rs`), the output settings window (`settings_ui.rs`). It turns OBS callbacks into calls of the core and core results into libobs calls. |

The rule for new code: a decision goes into the core with a test against the mock; the shell only translates. The core makes no libobs calls and has no global state, so it can later move into the obs-rust tree as a crate without change (see [roadmap.md](roadmap.md)).

No NDI SDK file is in the repository. `ndi.rs` declares the structs and functions it uses, following the SDK's MIT-licensed headers (see NOTICE). The runtime is loaded with `dlopen` (`ndi::candidates` gives the search order, NDI 6 only).

## Loading

1. `obs_module_load` registers nothing: another NDI plugin may load after this one.
2. `obs_module_post_load` (after every module loaded) asks libobs whether `ndi_source` or `ndi_output` already has a display name. If one has, another plugin (for example DistroAV) provides it: obs-avip logs one line and stops. It does not load the NDI runtime.
3. Otherwise it loads the NDI runtime, registers `ndi_source`, `ndi_output` and the private settings source `obs_avip_output_settings`, and adds the Tools menu item.
4. On `OBS_FRONTEND_EVENT_FINISHED_LOADING` it creates "NDI Main Output" and "NDI Preview Output" from `[NDIPlugin]` and starts the enabled ones.
5. `obs_module_unload` closes the outputs and the settings source and calls `NDIlib_destroy`.

The frontend functions (`obs_frontend_*`) are looked up with `dlsym` at run time, so the plugin also loads without a frontend (then without the main output and the menu item).

## Threads

| Thread | Started by | Work |
|---|---|---|
| `avip-recv` (one per running source) | show, activate, update | `recv_capture_v3` with 100 ms timeout, frames to `obs_source_output_video/audio` (interlaced fields woven first), resume (a finder of its own, made with the receiver, for the sender's address; while stalled a TCP probe of up to 300 ms every 2 s), tally, web control URL (once a second). Stopping it never waits (show, hide and update run on the video thread): its generation moves on, so its frames are dropped, and it ends by itself. Only `destroy` joins it. |
| `avip-find` | the source properties, source create | One discovery (until the list stops growing, 1 s per wait), at most every 5 s. Refreshes the open properties of every source. Joined at unload; stops after its current wait. |
| `avip-apply` | an edit in the output settings window | Waits 0.8 s for more edits, then queues the apply as a UI task (the outputs are opened and closed on the UI thread). Joined at unload. |
| OBS output threads | `ndi_output` start | `raw_video` copies each frame into one of two buffers and calls `send_video_async_v2`; `raw_audio` calls `send_audio_v3`. |

The receive thread passes the NDI frame buffer to `obs_source_output_video` and frees it after the call (libobs copies async frames).

## The output's frame copy

`NDIlib_send_send_video_async_v2` may read a frame until the next call. OBS reuses its frame buffer after `raw_video` returns. So the sender copies each frame into one of two owned buffers and alternates: handing over frame N+1 releases frame N. The copy also packs NV12 and I420 planes tightly, as NDI reads them, and the frame rate is OBS's exact `fps_num/fps_den`. The copy costs about 1 % of a core at 1080p.

## Settings compatibility

`settings::read` reproduces DistroAV's `ndi_source_update`: the integer values, the defaults (`settings::defaults`), the handling of unknown values, and the keys it writes back (an invalid behavior, the removed "Internal" sync mode, the alpha fix). The tests in `settings.rs` are the contract; each cites the DistroAV line it follows. Keys of our own: `ndi_resume` (bool, default on) and `arrival_ts` (bool, measurement only: frames get their arrival time).
