//! Shared logic for the NeedleVoice agent, config app and installer:
//! settings, app discovery, the Needle 3 "brain" and the actions it can take.

pub mod actions;
pub mod apps;
pub mod bpe;
pub mod brain;
pub mod config;
pub mod fuzzy;
pub mod paths;
pub mod schedule;
pub mod schedule_parse;
pub mod voice;
pub mod voices;
pub mod personality;
pub mod tools;
pub mod wake;
pub mod wake_model;
pub mod win;

pub use config::Config;

/// Product name, used for folders, registry keys and window titles.
pub const PRODUCT: &str = "NeedleVoice";
pub const AGENT_EXE: &str = "NeedleVoice.exe";
pub const CONFIG_EXE: &str = "NeedleVoiceConfig.exe";
pub const UNINSTALL_EXE: &str = "Uninstall.exe";
pub const NEEDLE_MODEL: &str = "needle3.cact";
pub const DEFAULT_WHISPER_MODEL: &str = "ggml-medium.en-q5_0.bin";
/// Named mutex the agent holds while running, so only one copy listens.
pub const AGENT_MUTEX: &str = "Local\\NeedleVoiceAgentSingleton";
