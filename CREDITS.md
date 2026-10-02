# Credits and third-party licences

NeedleVoice is a thin layer of glue around other people's work. Everything below
is used as published, with the licence it came with.

## The brain: Needle 3

The tool calling — turning "put the kettle on and tell me the time" into two
ordered function calls — is done by **Needle 3**, a foundation tool-calling
model for tiny devices by **Cactus Compute, Inc.**

- Model: <https://huggingface.co/Cactus-Compute/needle3>
- Source: <https://github.com/cactus-compute/needle>
- Project page: <https://cactuscompute.com/needle>
- Licence: **Apache-2.0**
- Weights file: `models/needle3.cact` (the 20-layer, CQ2-quantised release)

If you use Needle in your own work, the authors ask for this citation:

```bibtex
@misc{needle3_2026,
  title        = {Needle: Foundation Tool-Calling Model for Tiny Devices},
  author       = {Ndubuaku, Henry and Mosoyan, Karen and Mroz, Jakub and Cylich, Noah and
                  Kumar, Satyajit and Sandhu, Parkirat and Shemet, Roman and Lee, Justin H.},
  year         = {2026},
  organization = {Cactus Compute, Inc.},
  howpublished = {\url{https://github.com/cactus-compute/needle}}
}
```

The Rust inference engine that loads the `.cact` file is a separate project:

- **needle-infer** (crate `needle-infer`, MIT) — <https://github.com/geekgineer/needle-rs>

## Speech

- **whisper.cpp** — <https://github.com/ggml-org/whisper.cpp> (MIT), through the
  `whisper-rs` crate (<https://codeberg.org/tazz4843/whisper-rs>, Unlicense).
  The bundled `models/ggml-tiny.en-q5_1.bin` is a quantisation of OpenAI's
  Whisper `tiny.en` (MIT) published by the whisper.cpp project.
- **sherpa-onnx** by the k2-fsa / Next-gen Kaldi project —
  <https://github.com/k2-fsa/sherpa-onnx> (Apache-2.0). It runs the wake-word
  spotter, the Piper voices and the Kokoro voices.
- **The wake-word model** — `sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01`
  from the sherpa-onnx `kws-models` release (Apache-2.0). Its `bpe.model` is also
  what turns your assistant's name into the sub-word tokens the spotter listens
  for; the tokeniser that does it (`crates/nv-core/src/bpe.rs`) is ours, written
  against that model.
- **Piper** — <https://github.com/rhasspy/piper> (MIT). Default voice
  `vits-piper-en_US-lessac-medium`; the other voices on the Voice tab come from
  the same collection.
- **Kokoro** — <https://huggingface.co/hexgrad/Kokoro-82M> (Apache-2.0), via the
  `kokoro-multi-lang-v1_0` sherpa-onnx build.
- **WebRTC VAD** — <https://github.com/kaegi/webrtc-vad> (MIT), the voice
  activity detector that keeps Whisper from running on silence.

## Everything else

| Component | Licence | Used for |
| --- | --- | --- |
| [egui / eframe](https://github.com/emilk/egui) | MIT OR Apache-2.0 | the settings app and installer windows |
| [windows-rs](https://github.com/microsoft/windows-rs) | MIT OR Apache-2.0 | microphone, media keys, volume, tray icon, toasts |
| [cpal](https://github.com/RustAudio/cpal) | Apache-2.0 | audio capture |
| [ureq](https://github.com/algesten/ureq) | MIT OR Apache-2.0 | model downloads |
| [png](https://github.com/image-rs/image-png) | MIT OR Apache-2.0 | screenshots |
| [tar](https://github.com/composefs/tar-rs), [bzip2](https://github.com/alexcrichton/bzip2-rs) | MIT OR Apache-2.0 | unpacking model packs |
| [zstd](https://github.com/gyscos/zstd-rs) | BSD-3-Clause | the installer payload |
| [serde](https://github.com/serde-rs/serde), [toml](https://github.com/toml-rs/toml), [log](https://github.com/rust-lang/log), [strsim](https://github.com/rapidfuzz/strsim-rs) | MIT OR Apache-2.0 | settings, logging, fuzzy app matching |

## What this repository does not include

`models/`, `dist/` and `target/` are not tracked. Run
`scripts/fetch-models.ps1` to download the models listed above from their
original publishers; each keeps its own licence.
