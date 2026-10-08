# Builds NeedleVoice.exe, NeedleVoiceConfig.exe and NeedleVoiceUninstall.exe,
# then packs them into dist\NeedleVoiceSetup.exe.
#
#   -Sign   Authenticode-sign every binary in the payload and the installer,
#           using the certificate in $env:NV_SIGN_CERT (a .pfx) and its password
#           in $env:NV_SIGN_PASSWORD.
#
# Signing is all-or-nothing on purpose: signing Setup while leaving the binaries
# inside it unsigned can make Defender's opinion of the package worse, not
# better. See docs\installer.md.
param(
    [switch]$Sign,
    [switch]$NoModels
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    if (-not $NoModels) { & "$PSScriptRoot\fetch-models.ps1" }

    # The uninstaller is built before the installer, because the installer's
    # build script packs it into the payload. It is a program of its own and
    # deliberately small: see docs\installer.md.
    cargo build --release -p nv-uninstall
    if ($LASTEXITCODE) { throw "uninstaller build failed" }

    cargo build --release -p nv-agent -p nv-config
    if ($LASTEXITCODE) { throw "app build failed" }

    cargo build --release -p nv-setup
    if ($LASTEXITCODE) { throw "installer build failed" }

    $payload = @(
        "NeedleVoice.exe",
        "NeedleVoiceConfig.exe",
        "NeedleVoiceUninstall.exe",
        "onnxruntime.dll",
        "onnxruntime_providers_shared.dll",
        "sherpa-onnx-c-api.dll",
        "sherpa-onnx-cxx-api.dll"
    ) | ForEach-Object { Join-Path "target\release" $_ }

    $setup = "target\release\NeedleVoiceSetup.exe"
    foreach ($f in $payload + $setup) {
        if (-not (Test-Path $f)) { throw "missing $f" }
    }

    if ($Sign) {
        & "$PSScriptRoot\sign.ps1" -Files ($payload + $setup)
    } else {
        # Say it out loud rather than leaving it to be discovered: an unsigned
        # build is the one most likely to be flagged. See docs\installer.md.
        Write-Warning "Building unsigned. A code-signing certificate is the single most effective remedy for the Defender false positive on this installer; pass -Sign with NV_SIGN_CERT/NV_SIGN_PASSWORD set."
    }

    New-Item -ItemType Directory -Force -Path dist | Out-Null
    Copy-Item $setup dist\ -Force

    $mb = [math]::Round((Get-Item dist\NeedleVoiceSetup.exe).Length / 1MB)
    $uninstKb = [math]::Round((Get-Item target\release\NeedleVoiceUninstall.exe).Length / 1KB)
    $sig = (Get-AuthenticodeSignature dist\NeedleVoiceSetup.exe).Status
    Write-Host "Built dist\NeedleVoiceSetup.exe ($mb MB, signature: $sig)"
    Write-Host "  uninstaller: $uninstKb KB, no embedded payload"
} finally { Pop-Location }
