use serde::Deserialize;
use std::path::Path;
use crate::tailscale::TailscaleConfig;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub agent: AgentConfig,
    pub brain: BrainConfig,
    pub perception: PerceptionConfig,
    pub action: ActionConfig,
    pub server: ServerConfig,
    #[serde(default)]
    pub cron: Vec<CronJob>,
    #[serde(default)]
    pub hooks: HooksConfig,
    #[serde(default)]
    pub tailscale: TailscaleConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub heartbeat_interval_secs: u64,
    /// Deeper check-in interval (memory flush, pattern curation)
    #[serde(default = "default_gateway_heartbeat")]
    pub gateway_heartbeat_interval_secs: u64,
    pub workspace_path: String,
    #[serde(default = "default_bootstrap_max_chars")]
    pub bootstrap_max_chars: usize,
}

fn default_gateway_heartbeat() -> u64 { 1800 } // 30 min
fn default_bootstrap_max_chars() -> usize { 20000 }

#[derive(Debug, Clone, Deserialize)]
pub struct BrainConfig {
    /// "ollama", "openai_compatible", "llamacpp"
    pub backend: String,
    pub model: String,
    pub endpoint: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub vision_enabled: bool,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Thinking level: off, low, medium, high
    #[serde(default = "default_thinking")]
    pub thinking: String,
    /// Path to Codex OAuth auth.json (defaults to ~/.codex/auth.json)
    #[serde(default)]
    pub codex_auth_path: Option<String>,
}

fn default_max_tokens() -> u32 { 2048 }
fn default_temperature() -> f32 { 0.7 }
fn default_thinking() -> String { "medium".into() }

#[derive(Debug, Clone, Deserialize)]
pub struct PerceptionConfig {
    /// "adb" or "websocket"
    pub bridge_mode: String,
    #[serde(default)]
    pub adb_device: Option<String>,
    #[serde(default = "default_ws_addr")]
    pub android_ws_address: String,
    #[serde(default)]
    pub screen_capture_interval_secs: u64,
    #[serde(default = "default_true")]
    pub notifications_enabled: bool,
    #[serde(default = "default_true")]
    pub accessibility_enabled: bool,
    /// Priority apps (notifications from these trigger immediate ticks)
    #[serde(default)]
    pub priority_apps: Vec<String>,
}

fn default_ws_addr() -> String { "ws://192.168.1.100:9090".into() }
fn default_true() -> bool { true }

#[derive(Debug, Clone, Deserialize)]
pub struct ActionConfig {
    pub dry_run: bool,
    #[serde(default = "default_timeout")]
    pub confirmation_timeout_secs: u64,
    /// Apps that are always RED-classified regardless of action
    #[serde(default)]
    pub restricted_apps: Vec<String>,
}

fn default_timeout() -> u64 { 60 }

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub auth_token: Option<String>,
}

fn default_host() -> String { "0.0.0.0".into() }
fn default_port() -> u16 { 8420 }

#[derive(Debug, Clone, Deserialize)]
pub struct CronJob {
    pub name: String,
    pub schedule: String, // cron expression
    pub message: String,  // message to inject into agent
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default)]
    pub on_boot: Option<String>,        // file to run on startup
    #[serde(default)]
    pub on_session_new: Option<String>,  // on /new command
    #[serde(default)]
    pub on_unlock: Option<String>,       // on device unlock
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
