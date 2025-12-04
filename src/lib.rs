pub mod config;
pub mod groq;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_config_defaults() {
    use config::{
      default_debounce, default_groq_url, default_history_limit, default_model,
      default_session_file, default_temperature,
    };

    assert_eq!(
      default_groq_url(),
      "https://api.groq.com/openai/v1/chat/completions"
    );
    assert_eq!(
      default_model(),
      "meta-llama/llama-4-maverick-17b-128e-instruct"
    );
    assert_eq!(default_temperature(), 1.5);
    assert_eq!(default_session_file(), "userbot.session");
    assert_eq!(default_debounce(), 1);
    assert_eq!(default_history_limit(), 25);
  }

  #[test]
  fn test_tracked_user_peer_id() {
    use {config::TrackedUser, grammers_session::defs::PeerId};

    let user = TrackedUser {
      id: 12345,
      name: "Test User".to_string(),
      system_prompt: "Be helpful".to_string(),
    };

    assert_eq!(user.peer_id(), PeerId::user(12345));
  }

  #[test]
  fn test_config_parse() {
    let toml_content = r#"
[telegram]
api_id = 12345
api_hash = "test_hash"

[groq]
api_key = "test_key"

[settings]
session_file = "test.session"

[[users]]
id = 123
name = "Test"
system_prompt = "Test prompt"
    "#;

    let config: config::Config = toml::from_str(toml_content).unwrap();
    assert_eq!(config.telegram.api_id, 12345);
    assert_eq!(config.telegram.api_hash, "test_hash");
    assert_eq!(config.groq.api_key, "test_key");
    assert_eq!(config.settings.session_file, "test.session");
    assert_eq!(config.users.len(), 1);
    assert_eq!(config.users[0].name, "Test");
  }
}
