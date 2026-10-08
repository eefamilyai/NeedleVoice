# Authenticode-sign the NeedleVoice binaries.
#
#   $env:NV_SIGN_CERT     = "C:\path\to\certificate.pfx"
#   $env:NV_SIGN_PASSWORD = "…"
#   powershell -File scripts\sign.ps1 -Files a.exe,b.dll
#
# Why this exists: the installer and its uninstaller were flagged by Defender's
# machine-learning heuristic. Signing is the most effective remedy for that class
# of false positive — and it has to cover every executable and DLL that ships,
# not just the installer, because an unsigned binary inside a signed one is a
# pattern the heuristics dislike. See docs\installer.md.
param(
    [Parameter(Mandatory = $true)]
    [string[]]$Files
)
$ErrorActionPreference = "Stop"

$cert = $env:NV_SIGN_CERT
$password = $env:NV_SIGN_PASSWORD
if (-not $cert) { throw "NV_SIGN_CERT is not set (path to a .pfx, or a subject name for a store certificate)" }
if (-not (Test-Path $cert) -and -not $cert.Contains("=")) {
    throw "NV_SIGN_CERT does not exist: $cert"
}

# signtool ships with the Windows SDK; find the newest one available.
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
    Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
if (-not $signtool) { throw "signtool.exe not found. Install the Windows SDK signing tools." }

foreach ($f in $Files) {
    if (-not (Test-Path $f)) { throw "missing $f" }
}

foreach ($f in $Files) {
    Write-Host "Signing $f"
    # /fd sha256 and a SHA-256 timestamp: an unsigned timestamp or a SHA-1
    # signature is itself a mark of a hastily built package.
    & $signtool sign `
        /fd sha256 `
        /f $cert `
        /p $password `
        /tr http://timestamp.digicert.com `
        /td sha256 `
        /d "NeedleVoice" `
        $f
    if ($LASTEXITCODE) { throw "signing failed for $f" }
}

foreach ($f in $Files) {
    $status = (Get-AuthenticodeSignature $f).Status
    Write-Host "  $f -> $status"
    if ($status -ne "Valid") { throw "$f is not validly signed" }
}
Write-Host "Signed $($Files.Count) files."
