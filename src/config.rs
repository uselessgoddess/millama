use std::{collections::HashMap, fs, path::Path};

use {
  anyhow::{Context, Result},
  grammers_session::defs::PeerId,
  serde::{Deserialize, Serialize},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
  pub telegram: TelegramConfig,
  pub groq: GroqConfig,
  pub settings: Settings,
  #[serde(default)]
  pub users: Vec<TrackedUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
  pub api_id: i32,
  pub api_hash: String,
  pub bot_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroqConfig {
  pub api_key: String,
  #[serde(default = "default_groq_url")]
  pub api_url: String,
  #[serde(default = "default_model")]
  pub model: String,
  #[serde(default = "default_temperature")]
  pub temperature: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
  #[serde(default = "default_session_file")]
  pub session_file: String,
  #[serde(default = "default_debounce")]
  pub debounce_seconds: u64,
  #[serde(default = "default_history_limit")]
  pub history_limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedUser {
  pub id: i64,
  pub name: String,
  pub system_prompt: String,
}

impl TrackedUser {
  pub fn peer_id(&self) -> PeerId {
    PeerId::user(self.id)
  }
}

pub fn default_groq_url() -> String {
  "https://api.groq.com/openai/v1/chat/completions".to_string()
}

pub fn default_model() -> String {
  "meta-llama/llama-4-maverick-17b-128e-instruct".to_string()
}

pub fn default_temperature() -> f32 {
  1.5
}

pub fn default_session_file() -> String {
  "userbot.session".to_string()
}

pub fn default_debounce() -> u64 {
  1
}

pub fn default_history_limit() -> usize {
  25
}

impl Config {
  pub fn load(path: impl AsRef<Path>) -> Result<Self> {
    let path = path.as_ref();
    let content = fs::read_to_string(path).with_context(|| {
      format!("Failed to read config file: {}", path.display())
    })?;

    let config: Config = toml::from_str(&content).with_context(|| {
      format!("Failed to parse config file: {}", path.display())
    })?;

    Ok(config)
  }

  pub fn users_map(&self) -> HashMap<PeerId, TrackedUser> {
    self.users.iter().map(|user| (user.peer_id(), user.clone())).collect()
  }
}
