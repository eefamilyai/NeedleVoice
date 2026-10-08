//! Packs the built agent, config app, runtime DLLs and default models into a
//! compressed payload that the installer embeds.
//! Build the agent and config app first (see build-installer.ps1).

use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rel = root.join("target/release");
    let models = root.join("models");
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    println!("cargo:rerun-if-env-changed=NV_SKIP_PAYLOAD");

    nv_version::apply(
        &PathBuf::from(std::env::var("OUT_DIR").unwrap()),
        &root.join("assets/icon.ico"),
        "NeedleVoiceSetup.exe",
        "NeedleVoice Setup",
    );

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("payload.tar.zst");
    // The uninstaller travels inside this payload as a program of its own. It
    // is deliberately *not* this executable copied under another name — see
    // docs/installer.md for what went wrong when it was.
    let files: Vec<(PathBuf, String)> = [
        "NeedleVoice.exe",
        "NeedleVoiceConfig.exe",
        "NeedleVoiceUninstall.exe",
        "onnxruntime.dll",
        "onnxruntime_providers_shared.dll",
        "sherpa-onnx-c-api.dll",
        "sherpa-onnx-cxx-api.dll",
    ]
    .iter()
    .map(|f| (rel.join(f), f.to_string()))
    .chain([
        (models.join("needle3.cact"), "models/needle3.cact".into()),
        // One small Whisper, so switching engine in Settings works straight
        // away; the bigger ones are a download.
        (models.join("ggml-base.en-q5_1.bin"), "models/ggml-base.en-q5_1.bin".into()),
    ])
    .collect();

    if std::env::var_os("NV_SKIP_PAYLOAD").is_some() {
        std::fs::write(&out, []).unwrap();
        return;
    }
    let voice = "vits-piper-en_US-lessac-medium";
    let voice_dir = models.join("voices").join(voice);
    assert!(voice_dir.exists(), "missing default voice {}", voice_dir.display());
    println!("cargo:rerun-if-changed={}", voice_dir.display());

    // Moonshine is the default recogniser, and v2 is the better of the two packs:
    // the same speed as v1, smaller, and it reads "chrome" correctly where v1 read
    // "loafy". Shipping the weaker tiny pack would make the default engine the
    // worst one available out of the box.
    let moonshine_pack = "sherpa-onnx-moonshine-base-en-quantized-2026-02-27";
    let moonshine_dir = models.join("moonshine").join(moonshine_pack);
    assert!(
        ["encoder_model.ort", "decoder_model_merged.ort", "tokens.txt"]
            .iter()
            .all(|f| moonshine_dir.join(f).exists()),
        "missing the Moonshine v2 model at {} — run scripts/fetch-models.ps1",
        moonshine_dir.display()
    );

    // The wake-word keyword model (about 5 MB): without it the agent has to fall
    // back to Whisper for wake detection.
    let kws = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";
    let kws_dir = models.join("kws").join(kws);
    assert!(kws_dir.join("tokens.txt").exists(), "missing the wake-word model — run scripts\\fetch-models.ps1");

    // The streaming model behind the live transcript. Without it a fresh install
    // has the caption switched on and nothing to show, which looks like a broken
    // setting rather than a missing download.
    let streaming_pack = "sherpa-onnx-nemo-streaming-fast-conformer-ctc-en-80ms-int8";
    let streaming_dir = models.join("streaming").join(streaming_pack);
    assert!(
        ["model.int8.onnx", "tokens.txt"].iter().all(|f| streaming_dir.join(f).exists()),
        "missing the streaming model at {} — needed for the live transcript",
        streaming_dir.display()
    );

    for (src, _) in &files {
        println!("cargo:rerun-if-changed={}", src.display());
        assert!(src.exists(), "missing {} — build nv-agent and nv-config in release first", src.display());
    }
    for dir in [&voice_dir, &kws_dir, &moonshine_dir, &streaming_dir] {
        println!("cargo:rerun-if-changed={}", dir.display());
    }

    let file = std::fs::File::create(&out).unwrap();
    let mut enc = zstd::stream::Encoder::new(file, 12).unwrap();
    enc.multithread(8).unwrap();
    {
        let mut tar = tar::Builder::new(&mut enc);
        for (src, name) in &files {
            tar.append_path_with_name(src, name).unwrap();
        }
        add_dir(&mut tar, &voice_dir, &format!("models/voices/{voice}"));
        add_dir(&mut tar, &kws_dir, &format!("models/kws/{kws}"));
        add_dir(&mut tar, &moonshine_dir, &format!("models/moonshine/{moonshine_pack}"));
        add_dir(&mut tar, &streaming_dir, &format!("models/streaming/{streaming_pack}"));
        tar.finish().unwrap();
    }
    enc.finish().unwrap();
}

fn add_dir<W: std::io::Write>(tar: &mut tar::Builder<W>, dir: &Path, prefix: &str) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        let name = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        if p.is_dir() {
            add_dir(tar, &p, &name);
        } else {
            tar.append_path_with_name(&p, &name).unwrap();
        }
    }
}
