# obs-avip

obs-avip ("audio and video over IP") is an OBS plugin in Rust. It receives and sends NDI® video and audio. It is a drop-in for DistroAV: it uses the same source and output ids and the same settings keys, so a scene collection or profile that DistroAV saved loads with no change.

NDI® is a registered trademark of Vizrt NDI AB. More about NDI: https://ndi.video.

**Status: 0.1.0, early.** It was tested on Linux in place of DistroAV in a production setup, on macOS with the obs-rust nightly, and on Windows x64 with the obs-rust nightly in a virtual machine. Read [docs/status.md](docs/status.md) before you use it for a production show.

## What it does

- **NDI source** (`ndi_source`): a list of the NDI senders on the network, bandwidth, latency, sync, YUV range and color space, audio, tally to the sender, and the sender's web control URL.
- **Resume after a network cut.** When no frame comes for 1.5 s, the source checks every 2 s whether the sender answers on its NDI port, and connects again once when it answers again. The picture comes back shortly after the network is back, for a cut of any length (0.7 s measured with the earlier rule). A sender that still answers but sends nothing (for example an encoder that restarts) is not re-connected, so encoders that limit their connections are not used up; a warning is logged after 10 s. (The NDI SDK's own reconnect takes longer for longer cuts: 17 to 20 s after a 10 s cut, 65 s after a 45 s cut.) You can turn this off in the source properties.
- **NDI output** (`ndi_output`), **main output** ("NDI Main Output", the program) and **preview output** ("NDI Preview Output", the studio-mode preview).
- **Dedicated output filters**, DistroAV's `ndi_filter` ("AVIP (NDI® output)": the picture and audio of one source) and `ndi_audiofilter` ("AVIP (NDI® output, audio only)"), each its own NDI stream next to the main output. Same keys as DistroAV (`ndi_filter_ndiname`, `ndi_filter_ndigroups`), so saved filters load. The name takes `${source}` and `${filter}` (default `${filter} (${source})`) and follows renames. The picture is the source at its own size, with the filters before this one, while the source is showing and the filter is enabled, as BGRA with alpha. Audio comes only from sources that deliver their own audio (captures, media, NDI sources), not from a scene, as in DistroAV. The audio is tapped pre-fader: after the filters above this one and the balance, before the volume fader, mute and the track assignment, so a muted source still sends (as in DistroAV).
- **Output options** per `ndi_output`: a scaled size (`avip_width`, `avip_height`; 0 = canvas size), a frame-rate divisor (`avip_fps_divisor`, 1 to 60: every nth frame, the NDI frame rate says so) and the OBS mix track (`avip_track`, 0 to 5 = track 1 to 6). There is no window for them yet: set them with obs-websocket `SetOutputSettings` and restart the output. They are not saved in the profile. DistroAV ignores these keys.
- **Output settings** in OBS's own properties window: Tools → "AVIP: NDI® Output Settings". The settings are in `[NDIPlugin]` of the profile's user settings, the same keys as DistroAV.
- **NDI network settings** in OBS's own properties window: Tools → "AVIP: NDI® Network Settings". Discovery servers, extra source IPs, receive and send groups, a source name filter, the transports (reliable UDP, multi-TCP, UDP, multicast receive), and the network adapter NDI may use. By default NDI uses the system settings (NDI Access Manager, `~/.ndi`); select "obs-avip" in the window to use these instead. They are in `[AVIPNetwork]` of the user settings; obs-avip writes them as `ndi-config.v1.json` into its plugin config folder. The NDI runtime reads its network settings once per process, so a change applies at the next OBS start (the window says so). A `NDI_CONFIG_DIR` that is set before OBS starts always wins.
- **Steps aside for another NDI plugin.** obs-avip registers its ids only when no other module registered them. With DistroAV installed, obs-avip writes one log line and does nothing else.

What it does not do: see "Limitations" in [docs/status.md](docs/status.md).

## Requirements

- obs-rust (libobs API 33), 64-bit Linux, macOS 13 or later, or Windows 10/11 x64. OBS Studio 31 or later should work (same API) but is not tested.
- The NDI 6 runtime, installed on the computer:
  - Linux: `libndi.so.6` in `/usr/lib`, `/usr/lib64`, `/usr/local/lib`, the directory in `NDI_RUNTIME_DIR_V6`, or the loader's search path. Get it from the NDI SDK for Linux (https://ndi.video/for-developers/ndi-sdk/) or your distribution's package (for example `ndi-sdk` in the AUR).
  - macOS: `libndi.dylib` in `/usr/local/lib` or in `NDI_RUNTIME_DIR_V6`. The NDI Tools or the NDI SDK for Apple install it.
  - Windows: `Processing.NDI.Lib.x64.dll` in `NDI_RUNTIME_DIR_V6` (the NDI 6 Runtime installer and the NDI Tools set it), or next to the plugin (from the NDI SDK for Windows, `Bin\x64`, with `Processing.NDI.Lib.Licenses.txt`).
  - On every system a runtime next to the plugin is used first.
- To build: Rust (stable); on Windows the MSVC toolchain (Visual Studio Build Tools with C++). The repository uses `soldr` as the cargo front end (`uvx soldr cargo ...`); plain `cargo` also works.

## Build and install

Linux (installs to `~/.config/obs-studio/plugins/obs-avip`):

```sh
linux/install.sh
```

macOS (installs `obs-avip.plugin` to `~/Library/Application Support/obs-studio/plugins`, signed ad hoc):

```sh
macos/install.sh
```

Windows (installs to `C:\ProgramData\obs-studio\plugins\obs-avip`; run PowerShell as administrator):

```powershell
windows\install.ps1
windows\install.ps1 -Portable C:\path\to\obs          # a portable OBS (started with --portable)
windows\install.ps1 -NdiRuntime C:\path\to\Bin\x64    # also copy the NDI runtime next to the plugin
```

obs-rust loads user plugins from the `obs-studio` folder, also when its own settings are in `obs-studio-rust`.

On Windows, the firewall asks the first time OBS uses NDI. Allow it on private networks: without that, OBS finds and receives no NDI senders on the network (its own outputs still show).

To replace DistroAV: disable or remove DistroAV, install obs-avip, then start OBS. Your NDI sources and the main output come back with their saved settings. To go back, remove obs-avip and enable DistroAV again.

To remove obs-avip: delete the `obs-avip` folder (Linux, Windows) or `obs-avip.plugin` (macOS) from the plugin folder.

## Test

```sh
uvx soldr cargo test --workspace --locked
uvx soldr cargo clippy --workspace --all-targets --locked -- -D warnings
```

The tests need no NDI runtime: the core runs against a mock of the NDI API.

## Documents

- [docs/status.md](docs/status.md): what works, what was measured, limitations and known issues.
- [docs/architecture.md](docs/architecture.md): the two crates, the threads, and how the plugin loads.
- [docs/roadmap.md](docs/roadmap.md): from a plugin repository to native support in obs-rust, and the conditions to ship the NDI runtime.

## Licence

GPL-2.0-or-later ([LICENSE](LICENSE)). [NOTICE](NOTICE) has the NDI trademark notice, the licence of the NDI SDK header declarations, and the DistroAV compatibility note.

## NDI terms

- This repository contains no file of the NDI SDK and no NDI runtime. The NDI runtime is licensed by Vizrt NDI AB under its own terms (https://ndi.link/ndisdk_license). Do not modify, decompile or reverse engineer the NDI runtime or SDK.
- The NDI runtime is provided by Vizrt NDI AB "as is", without warranty.
- obs-avip is not made, tested or endorsed by Vizrt NDI AB.
