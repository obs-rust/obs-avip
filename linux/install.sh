#!/bin/sh
# Build obs-avip on Linux and install it as a user plugin.
# obs-rust loads user plugins from ~/.config/obs-studio/plugins (not from
# obs-studio-rust/plugins, although its config lives there).
set -eu
cd "$(dirname "$0")/.."
RUSTUP_TOOLCHAIN=stable uvx --quiet soldr cargo build --release -p obs-avip-plugin
dest="$HOME/.config/obs-studio/plugins/obs-avip/bin/64bit"
mkdir -p "$dest"
cp target/release/libobs_avip_plugin.so "$dest/obs-avip.so"
echo "installed $dest/obs-avip.so"
