# Downloads the models the installer bundles (run once before building).
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$models = Join-Path $root "models"
New-Item -ItemType Directory -Force -Path (Join-Path $models "voices") | Out-Null

$files = @{
    "needle3.cact"          = "https://huggingface.co/Cactus-Compute/needle3/resolve/main/needle3.cact"
    "ggml-tiny.en-q5_1.bin" = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en-q5_1.bin"
}
foreach ($f in $files.Keys) {
    $dest = Join-Path $models $f
    if (-not (Test-Path $dest)) { Write-Host "Downloading $f"; curl.exe -L --fail -o $dest $files[$f] }
}

# The wake-word spotter: a 3.3 M parameter keyword model. Only the int8 files
# are kept — the fp32 copies triple the size for no gain at this scale.
$kws = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01"
$kwsDir = Join-Path $models "kws\$kws"
if (-not (Test-Path (Join-Path $kwsDir "tokens.txt"))) {
    Write-Host "Downloading the wake-word model"
    New-Item -ItemType Directory -Force -Path (Join-Path $models "kws") | Out-Null
    $tmp = Join-Path $env:TEMP "$kws.tar.bz2"
    curl.exe -L --fail -o $tmp "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/$kws.tar.bz2"
    tar -xjf $tmp -C (Join-Path $models "kws") `
        --include "*/encoder-*.int8.onnx" --include "*/decoder-*.int8.onnx" `
        --include "*/joiner-*.int8.onnx" --include "*/tokens.txt" `
        --include "*/bpe.model" --include "*/README.md" `
        --include "*/keywords.txt" --include "*/keywords_raw.txt"
    Remove-Item $tmp
    if (-not (Test-Path (Join-Path $kwsDir "tokens.txt"))) { throw "the wake-word model didn't extract" }
}

$voice = "vits-piper-en_US-lessac-medium"
if (-not (Test-Path (Join-Path $models "voices\$voice"))) {
    Write-Host "Downloading default voice $voice"
    $tmp = Join-Path $env:TEMP "$voice.tar.bz2"
    curl.exe -L --fail -o $tmp "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/$voice.tar.bz2"
    tar -xjf $tmp -C (Join-Path $models "voices")
    Remove-Item $tmp
}
Write-Host "Models ready."
