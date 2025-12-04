pub mod config;
pub mod llm;

#[cfg(test)]
mod tests {
  #[test]
  fn test_tracked_user_peer_id() {
    use {crate::config::TrackedUser, grammers_session::defs::PeerId};

    let user = TrackedUser {
      id: 12345,
      name: "Test User".to_string(),
      system_prompt: "Be helpful".to_string(),
    };

    assert_eq!(user.peer_id(), PeerId::user(12345));
  }
}
