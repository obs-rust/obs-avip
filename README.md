# obs-avip

obs-avip ("audio and video over IP") is an OBS plugin in Rust. It receives and sends NDI® video and audio. It is a drop-in for DistroAV: it uses the same source and output ids and the same settings keys, so a scene collection or profile that DistroAV saved loads with no change.

NDI® is a registered trademark of Vizrt NDI AB. More about NDI: https://ndi.video.

**Status: 0.1.0, early.** It was tested on Linux in place of DistroAV in a production setup, and on macOS with the obs-rust nightly. It is not built or tested on Windows. Read [docs/status.md](docs/status.md) before you use it for a production show.

## What it does

- **NDI source** (`ndi_source`): a list of the NDI senders on the network, bandwidth, latency, sync, YUV range and color space, audio, tally to the sender, and the sender's web control URL.
- **Resume after a network cut.** When no frame comes for 1.5 s, the source checks every 2 s whether the sender answers on its NDI port, and connects again once when it answers again. The picture comes back shortly after the network is back, for a cut of any length (0.7 s measured with the earlier rule). A sender that still answers but sends nothing (for example an encoder that restarts) is not re-connected, so encoders that limit their connections are not used up; a warning is logged after 10 s. (The NDI SDK's own reconnect takes longer for longer cuts: 17 to 20 s after a 10 s cut, 65 s after a 45 s cut.) You can turn this off in the source properties.
- **NDI output** (`ndi_output`), **main output** ("NDI Main Output", the program) and **preview output** ("NDI Preview Output", the studio-mode preview).
- **Output settings** in OBS's own properties window: Tools → "AVIP: NDI® Output Settings". The settings are in `[NDIPlugin]` of the profile's user settings, the same keys as DistroAV.
- **NDI network settings** in OBS's own properties window: Tools → "AVIP: NDI® Network Settings". Discovery servers, extra source IPs, receive and send groups, a source name filter, the transports (reliable UDP, multi-TCP, UDP, multicast receive), and the network adapter NDI may use. By default NDI uses the system settings (NDI Access Manager, `~/.ndi`); select "obs-avip" in the window to use these instead. They are in `[AVIPNetwork]` of the user settings; obs-avip writes them as `ndi-config.v1.json` into its plugin config folder. The NDI runtime reads its network settings once per process, so a change applies at the next OBS start (the window says so). A `NDI_CONFIG_DIR` that is set before OBS starts always wins.
- **Steps aside for another NDI plugin.** obs-avip registers its ids only when no other module registered them. With DistroAV installed, obs-avip writes one log line and does nothing else.

What it does not do: see "Limitations" in [docs/status.md](docs/status.md).

## Requirements

- obs-rust (libobs API 33), 64-bit Linux or macOS 13 or later. OBS Studio 31 or later should work (same API) but is not tested.
- The NDI 6 runtime, installed on the computer:
  - Linux: `libndi.so.6` in `/usr/lib`, `/usr/lib64`, `/usr/local/lib`, the directory in `NDI_RUNTIME_DIR_V6`, or the loader's search path. Get it from the NDI SDK for Linux (https://ndi.video/for-developers/ndi-sdk/) or your distribution's package (for example `ndi-sdk` in the AUR).
  - macOS: `libndi.dylib` in `/usr/local/lib` or in `NDI_RUNTIME_DIR_V6`. The NDI Tools or the NDI SDK for Apple install it.
- To build: Rust (stable). The repository uses `soldr` as the cargo front end (`uvx soldr cargo ...`); plain `cargo` also works.

## Build and install

Linux (installs to `~/.config/obs-studio/plugins/obs-avip`):

```sh
linux/install.sh
```

macOS (installs `obs-avip.plugin` to `~/Library/Application Support/obs-studio/plugins`, signed ad hoc):

```sh
macos/install.sh
```

obs-rust loads user plugins from the `obs-studio` folder, also when its own settings are in `obs-studio-rust`.

To replace DistroAV: disable or remove DistroAV, install obs-avip, then start OBS. Your NDI sources and the main output come back with their saved settings. To go back, remove obs-avip and enable DistroAV again.

To remove obs-avip: delete the `obs-avip` folder (Linux) or `obs-avip.plugin` (macOS) from the plugin folder.

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
