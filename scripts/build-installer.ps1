# Builds NeedleVoice.exe, NeedleVoiceConfig.exe, then packs them into dist\NeedleVoiceSetup.exe.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    & "$PSScriptRoot\fetch-models.ps1"
    cargo build --release -p nv-agent -p nv-config
    if ($LASTEXITCODE) { throw "app build failed" }
    cargo build --release -p nv-setup
    if ($LASTEXITCODE) { throw "installer build failed" }
    New-Item -ItemType Directory -Force -Path dist | Out-Null
    Copy-Item target\release\NeedleVoiceSetup.exe dist\ -Force
    $mb = [math]::Round((Get-Item dist\NeedleVoiceSetup.exe).Length / 1MB)
    Write-Host "Built dist\NeedleVoiceSetup.exe ($mb MB)"
} finally { Pop-Location }
