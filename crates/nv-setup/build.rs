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

    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id("../../assets/icon.ico", "1");
    res.set("FileDescription", "NeedleVoice Setup");
    res.set("ProductName", "NeedleVoice");
    res.compile().expect("embedding icon");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("payload.tar.zst");
    let files: Vec<(PathBuf, String)> = [
        "NeedleVoice.exe",
        "NeedleVoiceConfig.exe",
        "onnxruntime.dll",
        "onnxruntime_providers_shared.dll",
        "sherpa-onnx-c-api.dll",
        "sherpa-onnx-cxx-api.dll",
    ]
    .iter()
    .map(|f| (rel.join(f), f.to_string()))
    .chain([
        (models.join("needle3.cact"), "models/needle3.cact".into()),
        (models.join("ggml-tiny.en-q5_1.bin"), "models/ggml-tiny.en-q5_1.bin".into()),
    ])
    .collect();

    if std::env::var_os("NV_SKIP_PAYLOAD").is_some() {
        std::fs::write(&out, []).unwrap();
        return;
    }
    for (src, _) in &files {
        println!("cargo:rerun-if-changed={}", src.display());
        assert!(src.exists(), "missing {} — build nv-agent and nv-config in release first", src.display());
    }
    let voice = "vits-piper-en_US-lessac-medium";
    let voice_dir = models.join("voices").join(voice);
    assert!(voice_dir.exists(), "missing default voice {}", voice_dir.display());
    println!("cargo:rerun-if-changed={}", voice_dir.display());

    // The wake-word keyword model (about 5 MB): without it the agent has to
    // fall back to Whisper for wake detection.
    let kws = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";
    let kws_dir = models.join("kws").join(kws);
    assert!(kws_dir.join("tokens.txt").exists(), "missing the wake-word model — run scripts\\fetch-models.ps1");
    println!("cargo:rerun-if-changed={}", kws_dir.display());

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
