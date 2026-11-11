# Build obs-avip on Windows (x64) and install it as a plugin.
#   windows\install.ps1                      -> C:\ProgramData\obs-studio\plugins\obs-avip (run as administrator)
#   windows\install.ps1 -Portable <OBS dir>  -> <OBS dir>\plugins\obs-avip (portable OBS, started with --portable)
#   -NdiRuntime <dir>: also copy Processing.NDI.Lib.x64.dll and Processing.NDI.Lib.Licenses.txt
#                      from <dir> next to the plugin (obs-avip loads a runtime there first).
# obs-rust loads <root>\obs-studio\plugins\<name>\<name>.dll, data from <name>\data.
param([string]$Portable = "", [string]$NdiRuntime = "")
Set-Location (Join-Path $PSScriptRoot "..")
$env:RUSTUP_TOOLCHAIN = "stable"
# cargo writes its progress to stderr; Windows PowerShell 5.1 would show it as errors.
cmd /c "cargo build --release -p obs-avip-plugin 2>&1"
if ($LASTEXITCODE -ne 0) { throw "build failed" }
$ErrorActionPreference = "Stop"
if ($Portable) {
    $dest = Join-Path $Portable "plugins\obs-avip"
} else {
    $dest = Join-Path $env:ProgramData "obs-studio\plugins\obs-avip"
}
New-Item -ItemType Directory -Force (Join-Path $dest "data") | Out-Null
Copy-Item "target\release\obs_avip_plugin.dll" (Join-Path $dest "obs-avip.dll")
if ($NdiRuntime) {
    foreach ($f in "Processing.NDI.Lib.x64.dll", "Processing.NDI.Lib.Licenses.txt") {
        Copy-Item (Join-Path $NdiRuntime $f) $dest
    }
}
"installed $dest\obs-avip.dll"
