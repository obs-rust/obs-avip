#!/bin/sh
# Build obs-avip for macOS and install it as a user plugin bundle.
#   macos/install.sh [config-dir-name]   (default: obs-studio)
# obs-rust loads user plugins from ~/Library/Application Support/obs-studio/plugins,
# also when the config itself lives in obs-studio-rust.
set -eu
cd "$(dirname "$0")/.."
name=${1:-obs-studio}
RUSTUP_TOOLCHAIN=stable uvx --quiet soldr cargo build --release -p obs-avip-plugin
bundle="$HOME/Library/Application Support/$name/plugins/obs-avip.plugin"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp macos/Info.plist "$bundle/Contents/Info.plist"
cp target/release/libobs_avip_plugin.dylib "$bundle/Contents/MacOS/obs-avip"
# Ad hoc: OBS's app has disable-library-validation, so it loads this bundle.
codesign -f -s - "$bundle"
echo "installed $bundle"
