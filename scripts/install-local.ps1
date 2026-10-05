# Installs the freshly built binaries over the installed copy and restarts the
# assistant. Run this after `cargo build --release` — otherwise the copy you
# launch from the Start menu is whatever the installer shipped, which is how a
# day of fixes ended up sitting in target\release unattended.
#
#   powershell -ExecutionPolicy Bypass -File scripts\install-local.ps1
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\NeedleVoice",
    [switch]$NoRestart
)

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$release = Join-Path $root "target\release"

if (-not (Test-Path (Join-Path $InstallDir "NeedleVoice.exe"))) {
    throw "No installed copy at $InstallDir — run the installer first."
}
foreach ($exe in @("NeedleVoice.exe", "NeedleVoiceConfig.exe")) {
    if (-not (Test-Path (Join-Path $release $exe))) {
        throw "$exe is not built yet — run: cargo build --release"
    }
}

# The assistant holds its own executable open while running.
Get-Process NeedleVoice, NeedleVoiceConfig -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 2

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$backup = Join-Path $root "target\installed-backup\$stamp"
New-Item -ItemType Directory -Force -Path $backup | Out-Null
foreach ($exe in @("NeedleVoice.exe", "NeedleVoiceConfig.exe")) {
    Copy-Item (Join-Path $InstallDir $exe) (Join-Path $backup $exe) -Force
}

foreach ($exe in @("NeedleVoice.exe", "NeedleVoiceConfig.exe")) {
    Copy-Item (Join-Path $release $exe) (Join-Path $InstallDir $exe) -Force
    Write-Host "installed $exe"
}
# Runtime libraries, in case they changed too.
Get-ChildItem (Join-Path $release "*.dll") -ErrorAction SilentlyContinue | ForEach-Object {
    Copy-Item $_.FullName (Join-Path $InstallDir $_.Name) -Force
}

# Models the installed copy does not ship (Moonshine is a download in Settings,
# so install it here when it is already on hand).
$devModels = Join-Path $root "models"
$instModels = Join-Path $InstallDir "models"
if (Test-Path (Join-Path $devModels "moonshine")) {
    Copy-Item (Join-Path $devModels "moonshine") (Join-Path $instModels "moonshine") -Recurse -Force
    Write-Host "installed the Moonshine models"
}
# Any Whisper model the config asks for that the install does not have.
$cfgFile = Join-Path $env:APPDATA "NeedleVoice\config.toml"
if (Test-Path $cfgFile) {
    $wanted = (Select-String -Path $cfgFile -Pattern '^\s*whisper_model\s*=\s*"([^"]+)"').Matches.Groups[1].Value
    if ($wanted) {
        $src = Join-Path $devModels $wanted
        $dst = Join-Path $instModels $wanted
        if ((Test-Path $src) -and -not (Test-Path $dst)) {
            Copy-Item $src $dst -Force
            Write-Host "installed the Whisper model $wanted"
        }
    }
}

if (-not $NoRestart) {
    Start-Process -FilePath (Join-Path $InstallDir "NeedleVoice.exe") -ArgumentList "--background" -WorkingDirectory $InstallDir
    Start-Sleep -Seconds 6
    $log = Join-Path $env:APPDATA "NeedleVoice\agent.log"
    if (Test-Path $log) {
        Write-Host "--- last lines of the log"
        Get-Content $log -Tail 6 | Where-Object { $_ -notmatch "^whisper_|wake keywords|▁" }
    }
}

# Confirm the installed build is the one that was just built.
$fresh = (Get-FileHash (Join-Path $release "NeedleVoice.exe") -Algorithm SHA256).Hash
$placed = (Get-FileHash (Join-Path $InstallDir "NeedleVoice.exe") -Algorithm SHA256).Hash
if ($fresh -ne $placed) { throw "The installed copy does not match the build." }
Write-Host "installed build matches target\release ($($fresh.Substring(0,12))…)"
