# Downloads the models the installer bundles (run once before building).
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$models = Join-Path $root "models"
New-Item -ItemType Directory -Force -Path (Join-Path $models "voices") | Out-Null

param([switch]$WithMoonshine)

$files = @{
    "needle3.cact"          = "https://huggingface.co/Cactus-Compute/needle3/resolve/main/needle3.cact"
    "ggml-medium.en-q5_0.bin" = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en-q5_0.bin"
}
if ($WithMoonshine) {
    # Bundling this makes the installer ~275 MB bigger, so it is opt-in: the
    # settings app downloads the same files on demand otherwise.
    $moonshine = @("preprocess.onnx", "encode.int8.onnx", "uncached_decode.int8.onnx",
                   "cached_decode.int8.onnx", "tokens.txt")
    foreach ($m in $moonshine) {
        $files["moonshine\sherpa-onnx-moonshine-base-en-int8\$m"] =
            "https://huggingface.co/csukuangfj/sherpa-onnx-moonshine-base-en-int8/resolve/main/$m"
    }
}
foreach ($f in $files.Keys) {
    $dest = Join-Path $models $f
    New-Item -ItemType Directory -Force -Path (Split-Path $dest -Parent) | Out-Null
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
