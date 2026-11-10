# Roadmap

The plan agreed with the obs-rust maintainers (2026-11-10, after Discussion #4439 in obs-rust/obs-studio): first a plugin repository, then in the obs-rust tree, then native. Each step starts only when the step before has users.

## 1. Plugin repository (now)

`obs-rust/obs-avip`, next to obs-browser and obs-websocket. The user installs the plugin and the NDI runtime. It uses DistroAV's ids, so users can switch in both directions.

To do in this step:
- Check the review fixes in [status.md](status.md) that have no real-sender test yet: a sender that delivers NV12 or I420 (an NDI HX phone camera delivered UYVY), an HLG sender, the output settings window, a sender that stays reachable and sends no frame at all.
- CI: build and test both crates on Linux and macOS (the tests run against the mock, no NDI runtime needed).
- A release: one archive per platform with the plugin and the install steps.
- Windows: the loader (`ndi::Os`, `LoadLibrary`, the NDI runtime path from `NDI_RUNTIME_DIR_V6`), the build, and a test on a Windows machine.

## 2. In the obs-rust tree ("inline")

The plugin ships with obs-rust:
- The core moves to `rust/obs-avip`, the shell to `plugins/obs-avip` (as `obs-avip-plugin`), under the naming and layout rules of the tree (`docs/rust-port/naming.md`).
- The obs-rust CI builds and tests it on every platform; an ABI change in libobs shows in CI.
- It keeps the drop-in behavior: it registers its ids in `obs_module_post_load` and only when no other module took them, so a user who installs DistroAV or a plugin on the NDI Advanced SDK keeps it.

## 3. Native

NDI becomes part of OBS's own UI and settings:
- The output settings become a page in OBS Settings; the Tools menu item and the hidden settings source go away.
- A setting "Built-in NDI: auto / off" for third-party plugins with other ids.
- The output options users ask for: scaling per output, a frame-rate divisor (a 1080p25 monitor feed next to a 4K main), audio track per output, more outputs with their own scene or source, alpha.
- Later, open standards (IPMX, SMPTE ST 2110) behind the same source and output types, with no closed SDK.

## Shipping the NDI runtime

The maintainers agreed that obs-rust may ship the NDI runtime, so that users need no separate install. This is not done in step 1: today the user installs the runtime.

Conditions from the NDI SDK licence (read from the licence in NDI SDK 6.3.2, dated 2025-08-21; not legal advice):
- **Latest SDK.** A product that bundles the runtime must use the latest SDK version at the time of distribution. There is no grace period: every NDI SDK release makes an obs-rust release (or at least a runtime update) necessary.
- **Only the redistributable files.** Only the files the SDK documentation names may be distributed (the runtime libraries), with `libndi_licenses.txt` next to them, in the application's own folder, not in a system path. The NDI Tools may not be distributed.
- **Downstream terms.** The licence that comes with the product must prohibit modifying, decompiling or reverse engineering the NDI SDK and runtime, and must carry NDI's warranty disclaimer and copyright notice.
- **Products.** General-purpose computers only: not embedded devices or appliances, and not cloud or hosted services (those need a commercial licence from NDI).
- **Trademark.** "NDI®" with the notice "NDI® is a registered trademark of Vizrt NDI AB" at its first use and in the About box, and a link to https://ndi.video near every place where NDI is used or selected. A product name with "NDI" in it needs NDI's permission (the reason for the name "obs-avip").
- **Codecs.** NDI HX needs H.264/HEVC decoding (FFmpeg on Linux); its licensing is the product's.

Open before the runtime ships:
- GPL-2 says the distributor may impose no further restrictions; the downstream terms above restrict the NDI runtime, not the GPL code. A maintainer decision, probably with a question to NDI, on how obs-rust packages the two (separate component and licence file, as other GPL projects with closed runtimes do).
- Who is the licensee towards NDI for obs-rust's releases.
- macOS: the NDI-signed runtime inside a notarized app bundle. A test with an unmodified, NDI-signed `libndi.dylib` in an app's resources passed notarization with no re-signing.
- The loader then searches the plugin's or application's own folder first.
