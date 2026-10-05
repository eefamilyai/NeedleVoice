use std::path::PathBuf;

/// `%APPDATA%\NeedleVoice` — config, app cache and logs. Survives reinstalls.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    let dir = base.join(crate::PRODUCT);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_file() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn apps_cache_file() -> PathBuf {
    data_dir().join("apps.json")
}

pub fn log_file() -> PathBuf {
    data_dir().join("agent.log")
}

/// Directory holding the running executable.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Default install location: `%LOCALAPPDATA%\Programs\NeedleVoice` (no admin needed).
pub fn default_install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("Programs").join(crate::PRODUCT)
}

/// Where model files live. Prefers `<exe dir>\models`; during development
/// also walks up from the exe looking for a `models` folder.
///
/// Resolved once and remembered: the answer cannot change while the process
/// runs, and the settings app asks for it from several pages on every frame,
/// each time walking the directory tree and probing the disk.
pub fn models_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(resolve_models_dir).clone()
}

fn resolve_models_dir() -> PathBuf {
    let mut dir = Some(exe_dir());
    while let Some(d) = dir {
        let candidate = d.join("models");
        if candidate.join(crate::NEEDLE_MODEL).exists() {
            return candidate;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    exe_dir().join("models")
}

/// Path of a sibling executable (e.g. the config app next to the agent).
pub fn sibling_exe(name: &str) -> PathBuf {
    exe_dir().join(name)
}
