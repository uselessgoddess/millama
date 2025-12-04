mod bot;
mod config;
mod llm;

use std::{
  collections::HashMap,
  io::{self, Write},
  sync::{Arc, Mutex},
  time::Duration,
};

use {
  clap::Parser,
  grammers_client::{Client, SignInError, Update, UpdatesConfiguration},
  grammers_mtsender::SenderPool,
  grammers_session::{
    defs::{PeerId, PeerRef},
    storages::SqliteSession,
  },
};

use {
  anyhow::{Context, Result},
  config::{Config, TrackedUser},
  llm::ChatMessage,
  tokio::{task::JoinSet, time::sleep},
  tracing::{debug, error, info, trace, warn},
};

struct BotState {
  pending_tasks: HashMap<PeerId, tokio::task::AbortHandle>,
  users: HashMap<PeerId, TrackedUser>,
  config: Config,
  bot_client: Arc<bot::BotClient>,
  bot_self_id: i64,
  // Maps callback_id to (target_id, message_text)
  draft_messages: HashMap<String, (i64, String)>,
  // Maps target_id to (chat_id, message_id, original_history)
  pending_rephrase: HashMap<i64, (i64, i64, Vec<ChatMessage>)>,
}

#[derive(Parser, Debug)]
#[command(name = "millama")]
#[command(about = "AI-powered Telegram message assistant", long_about = None)]
struct Cli {
  /// Path to configuration file
  #[arg(short, long, default_value = "config.toml")]
  config: String,

  /// Enable debug logging
  #[arg(short, long)]
  debug: bool,

  /// Enable trace logging
  #[arg(short, long)]
  trace: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
  let cli = Cli::parse();

  // Initialize logging
  let log_level = if cli.trace {
    "trace"
  } else if cli.debug {
    "debug"
  } else {
    "info"
  };

  tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(
        |_| {
          tracing_subscriber::EnvFilter::new(format!("millama={}", log_level))
        },
      ),
    )
    .init();

  info!("Starting millama...");

  // Load configuration
  let config = Config::load(&cli.config)
    .with_context(|| format!("Failed to load config from {}", cli.config))?;

  info!("Loaded configuration with {} tracked users", config.users.len());

  run_client(config).await
}

async fn run_client(config: Config) -> Result<()> {
  let users_map = config.users_map();

  let bot_client =
    Arc::new(bot::BotClient::new(config.telegram.bot_token.clone()));
  info!("Bot token configured, using Bot API for approval workflow");

  let state = Arc::new(Mutex::new(BotState {
    pending_tasks: HashMap::new(),
    users: users_map,
    config: config.clone(),
    bot_client,
    bot_self_id: 0, // Will be set after login
    draft_messages: HashMap::new(),
    pending_rephrase: HashMap::new(),
  }));

  info!("Connecting to Telegram...");
  let session = Arc::new(
    SqliteSession::open(&config.settings.session_file)
      .context("Failed to open session file")?,
  );
  let pool = SenderPool::new(session.clone(), config.telegram.api_id);
  let client = Client::new(&pool);
  let SenderPool { runner, updates, handle } = pool;

  let pool_task = tokio::spawn(runner.run());

  if !client.is_authorized().await? {
    info!("Not authorized, starting login flow");
    let phone = prompt("Phone: ");
    let token = client
      .request_login_code(&phone, &config.telegram.api_hash)
      .await
      .context("Failed to request login code")?;
    let code = prompt("Code: ");
    if let Err(e) = client.sign_in(&token, &code).await {
      if let SignInError::PasswordRequired(token) = e {
        let password = rpassword::prompt_password("2FA Password: ")
          .context("Failed to read password")?;
        client
          .check_password(token, password)
          .await
          .context("Failed to check password")?;
      } else {
        return Err(e.into());
      }
    }
  }
  info!("Signed in successfully!");

  // Get self user ID
  let me = client.get_me().await?;
  let self_id_bare = me.raw.id();

  // Store self ID for bot messages
  {
    let mut lock = state.lock().unwrap();
    lock.bot_self_id = self_id_bare;
  }

  info!("Running as self user (ID: {})", self_id_bare);

  let mut update_stream =
    client.stream_updates(updates, UpdatesConfiguration::default());
  let mut tasks = JoinSet::new();

  // Start bot updates polling task
  let bot_client_for_polling = {
    let lock = state.lock().unwrap();
    lock.bot_client.clone()
  };

  let state_for_bot = state.clone();
  let client_for_bot = client.clone();
  tasks.spawn(async move {
    if let Err(e) =
      poll_bot_updates(bot_client_for_polling, client_for_bot, state_for_bot)
        .await
    {
      error!("Bot updates polling error: {}", e);
    }
  });
  info!("Started bot updates polling task");

  info!("Bot is ready and listening for updates");

  loop {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl+C, shutting down...");
            break;
        }
        update = update_stream.next() => {
            let update = match update {
                Ok(u) => u,
                Err(e) => {
                    error!("Update error: {}", e);
                    continue;
                }
            };

            let client = client.clone();
            let state = state.clone();

            tasks.spawn(async move {
              if let Err(e) = handle_update(client, update, state).await {
                error!("Error handling update: {}", e);
              }
            });
        }
    }
  }

  info!("Shutting down...");
  handle.quit();
  let _ = pool_task.await;
  Ok(())
}

async fn handle_update(
  client: Client,
  update: Update,
  state: Arc<Mutex<BotState>>,
) -> Result<()> {
  if let Update::NewMessage(message) = update {
    let peer = match message.peer() {
      Ok(peer) => PeerRef::from(peer),
      Err(peer) => peer,
    };

    // Escape control characters for logging to prevent log injection
    let message_text = message.text().escape_debug().to_string();
    trace!("Message from user ({}): {}", peer.id, message_text);

    // Handle messages from tracked users
    let tracked_user = {
      let lock = state.lock().unwrap();
      lock.users.get(&peer.id).cloned()
    };

    if let Some(user) = tracked_user {
      debug!(
        "Message from tracked user {} ({}): {}",
        user.name,
        peer.id,
        message.text()
      );

      // Cancel any pending task for this user
      {
        let mut lock = state.lock().unwrap();
        if let Some(handle) = lock.pending_tasks.remove(&peer.id) {
          debug!("Cancelling pending task for user {}", user.name);
          handle.abort();
        }
      }

      let client_clone = client.clone();
      let state_clone = state.clone();
      let user_clone = user.clone();
      let debounce_seconds = {
        let lock = state.lock().unwrap();
        lock.config.settings.debounce_seconds
      };

      let handle = tokio::spawn(async move {
        sleep(Duration::from_secs(debounce_seconds)).await;

        {
          let mut lock = state_clone.lock().unwrap();
          lock.pending_tasks.remove(&peer.id);
        }

        info!(
          "Silence detected for {} ({}). Generating draft...",
          user_clone.name, peer.id
        );

        if let Err(e) =
          process_ai_draft(&client_clone, peer, &user_clone, &state_clone).await
        {
          error!("Error processing AI draft: {}", e);
        }
      });

      let mut lock = state.lock().unwrap();
      lock.pending_tasks.insert(peer.id, handle.abort_handle());

      return Ok(());
    }
  }
  Ok(())
}

async fn process_ai_draft(
  client: &Client,
  peer: PeerRef,
  user: &TrackedUser,
  state: &Arc<Mutex<BotState>>,
) -> Result<()> {
  process_ai_draft_with_guidance(client, peer, user, state, None).await
}

async fn process_ai_draft_with_guidance(
  client: &Client,
  peer: PeerRef,
  user: &TrackedUser,
  state: &Arc<Mutex<BotState>>,
  rephrase_guidance: Option<String>,
) -> Result<()> {
  let (
    api_key,
    api_url,
    models,
    temperature,
    history_limit,
    bot_client,
    bot_self_id,
    base_system_prompt,
  ) = {
    let lock = state.lock().unwrap();
    (
      lock.config.ai.api_key.clone(),
      lock.config.ai.api_url.clone(),
      lock.config.ai.models.clone(),
      lock.config.ai.temperature,
      lock.config.settings.history_limit,
      lock.bot_client.clone(),
      lock.bot_self_id,
      lock.config.ai.base_system_prompt.clone(),
    )
  };

  // Fetch message history
  let mut history_buf: Vec<ChatMessage> = Vec::new();

  debug!("Fetching message history for peer {}", peer.id);

  // Convert peer ID to user peer for message history access
  // This handles both private messages and ensures proper peer resolution
  let peer_for_messages =
    PeerRef { id: PeerId::user(peer.id.bare_id()), auth: Default::default() };

  let chat_peer = client
    .resolve_peer(peer_for_messages)
    .await
    .context("Could not resolve peer to fetch history")?;

  let mut messages_iter = client.iter_messages(chat_peer).limit(history_limit);

  while let Some(msg) = messages_iter.next().await? {
    let text = msg.text();
    if text.is_empty() {
      continue;
    }

    let role = if msg.outgoing() { "assistant" } else { "user" };

    history_buf.insert(
      0,
      ChatMessage { role: role.to_string(), content: text.to_string() },
    );
  }

  if history_buf.is_empty() {
    warn!("No message history found for peer {}", peer.id);
    return Ok(());
  }

  debug!("Loaded {} messages from history", history_buf.len());

  // Build the system prompt with optional base prompt and rephrase guidance
  let system_prompt = {
    let mut prompt = String::new();

    // Add base system prompt if configured
    if let Some(base) = base_system_prompt.as_ref() {
      prompt.push_str(base);
      prompt.push_str("\n\n");
    }

    // Add user-specific system prompt
    prompt.push_str(&user.system_prompt);

    // Add rephrase guidance if provided
    if let Some(guidance) = rephrase_guidance.as_ref() {
      prompt.push_str("\n\nAdditional guidance: ");
      prompt.push_str(guidance);
    }

    prompt
  };

  let response_text = llm::generate_reply_with_fallback(
    &api_key,
    &api_url,
    models,
    temperature,
    &system_prompt,
    history_buf.clone(),
  )
  .await
  .context("Failed to generate AI reply")?;

  info!("Generated AI response for user {}", user.name);

  // Send draft via Bot API with inline buttons
  let target_id = peer.id.bare_id();
  let draft_message = format!(
    "*AI Draft Suggestion for @{}*\n\n{}\n\n",
    user.name, response_text
  );

  let callback_data = format!("approve:{}", target_id);
  let rephrase_data = format!("rephrase:{}", target_id);
  let reject_data = format!("reject:{}", target_id);

  let buttons = vec![vec![
    ("✅ Approve".to_string(), callback_data.clone()),
    ("🔄 Rephrase".to_string(), rephrase_data.clone()),
    ("❌ Reject".to_string(), reject_data.clone()),
  ]];

  let message_id = bot_client
    .send_message_with_buttons(bot_self_id, draft_message, buttons)
    .await
    .context("Failed to send draft via bot")?;

  // Store draft message and history for later retrieval
  {
    let mut lock = state.lock().unwrap();
    lock.draft_messages.insert(callback_data, (target_id, response_text));
    lock
      .pending_rephrase
      .insert(target_id, (bot_self_id, message_id, history_buf));
  }

  debug!("Sent draft message via bot to self");

  Ok(())
}

async fn poll_bot_updates(
  bot_client: Arc<bot::BotClient>,
  client: Client,
  state: Arc<Mutex<BotState>>,
) -> Result<()> {
  let mut offset: Option<i64> = None;

  loop {
    let updates = bot_client.get_updates(offset).await?;

    for update in updates {
      offset = Some(update.update_id + 1);

      if let Some(callback) = update.callback_query {
        let bot_client = bot_client.clone();
        let client = client.clone();
        let state = state.clone();

        tokio::spawn(async move {
          if let Err(e) =
            handle_bot_callback(bot_client, client, state, callback).await
          {
            error!("Error handling bot callback: {}", e);
          }
        });
      } else if let Some(message) = update.message {
        let bot_client = bot_client.clone();
        let client = client.clone();
        let state = state.clone();

        tokio::spawn(async move {
          if let Err(e) =
            handle_bot_message(bot_client, client, state, message).await
          {
            error!("Error handling bot message: {}", e);
          }
        });
      }
    }
  }
}

async fn handle_bot_callback(
  bot_client: Arc<bot::BotClient>,
  client: Client,
  state: Arc<Mutex<BotState>>,
  callback: bot::CallbackQuery,
) -> Result<()> {
  let data = callback.data.as_ref().context("No callback data")?;
  let message = callback.message.as_ref().context("No callback message")?;

  debug!("Received callback: {}", data);

  // Answer the callback query to remove the loading state
  bot_client
    .answer_callback_query(&callback.id, None)
    .await
    .context("Failed to answer callback query")?;

  if data.starts_with("approve:") {
    // Retrieve draft message from state
    let (target_id, message_text) = {
      let mut lock = state.lock().unwrap();
      lock.draft_messages.remove(data).context("Draft message not found")?
    };

    info!("Approving message to target ID: {}", target_id);

    let target =
      PeerRef { id: PeerId::user(target_id), auth: Default::default() };

    debug!("Sending approved message to ({}): {}", target.id, message_text);

    let target_peer = client.resolve_peer(target).await?;
    client
      .send_message(target_peer, &message_text)
      .await
      .context("Failed to send approved message")?;

    // Update the bot message to show it was sent
    bot_client
      .edit_message_text(message.chat.id, message.message_id, message_text)
      .await
      .context("Failed to edit message")?;

    // Clean up rephrase state
    {
      let mut lock = state.lock().unwrap();
      lock.pending_rephrase.remove(&target_id);
    }

    info!("Message sent successfully to {}", target_id);
  } else if data.starts_with("rephrase:") {
    let target_id: i64 = data
      .strip_prefix("rephrase:")
      .context("Invalid rephrase data")?
      .parse()
      .context("Failed to parse target_id")?;

    info!("Rephrase requested for target ID: {}", target_id);

    // Update the bot message to prompt for rephrase guidance
    let rephrase_prompt = concat!(
      "🔄 *Rephrase Mode*\n\n",
      "Please send me the guidance for rephrasing ",
      "(e.g., \"the name of user is John\")"
    );
    bot_client
      .edit_message_text(
        message.chat.id,
        message.message_id,
        rephrase_prompt.to_string(),
      )
      .await
      .context("Failed to edit message")?;

    debug!("Waiting for rephrase guidance for target {}", target_id);
  } else if data.starts_with("reject:") {
    let target_id: i64 = data
      .strip_prefix("reject:")
      .context("Invalid reject data")?
      .parse()
      .context("Failed to parse target_id")?;

    info!("Rejecting draft for target ID: {}", target_id);

    // Remove draft message and rephrase state
    {
      let mut lock = state.lock().unwrap();
      let reject_key = format!("approve:{}", target_id);
      lock.draft_messages.remove(&reject_key);
      lock.pending_rephrase.remove(&target_id);
    }

    // Update the bot message to show it was rejected
    bot_client
      .edit_message_text(
        message.chat.id,
        message.message_id,
        "❌ *Rejected*".to_string(),
      )
      .await
      .context("Failed to edit message")?;
  }

  Ok(())
}

async fn handle_bot_message(
  bot_client: Arc<bot::BotClient>,
  client: Client,
  state: Arc<Mutex<BotState>>,
  message: bot::BotMessage,
) -> Result<()> {
  let text = match message.text.as_ref() {
    Some(t) if !t.is_empty() => t,
    _ => return Ok(()), // Ignore messages without text
  };

  let bot_self_id = {
    let lock = state.lock().unwrap();
    lock.bot_self_id
  };

  // Only process messages from self
  if message.from.id != bot_self_id {
    return Ok(());
  }

  debug!("Received bot message from self: {}", text);

  // Check if any rephrase request is pending
  let pending_rephrase_targets: Vec<i64> = {
    let lock = state.lock().unwrap();
    lock.pending_rephrase.keys().copied().collect()
  };

  if pending_rephrase_targets.is_empty() {
    debug!("No pending rephrase requests, ignoring message");
    return Ok(());
  }

  // Process rephrase for all pending targets (should typically be just one)
  for target_id in pending_rephrase_targets {
    info!("Processing rephrase guidance for target {}: {}", target_id, text);

    // Retrieve rephrase state and user info
    let (user, history) = {
      let mut lock = state.lock().unwrap();
      let (_, _, history) = lock
        .pending_rephrase
        .remove(&target_id)
        .context("No pending rephrase")?;

      let user =
        lock.users.get(&PeerId::chat(target_id)).cloned().context(format!(
          "User not found for target_id {}. Available users: {:?}",
          target_id,
          lock.users.keys().collect::<Vec<_>>()
        ))?;

      (user, history)
    };

    debug!("Found user {} for rephrase, regenerating with guidance", user.name);

    // Regenerate AI response with guidance
    let peer =
      PeerRef { id: PeerId::user(target_id), auth: Default::default() };

    // We need to pass the history and guidance to regenerate
    // Let's call a modified version that accepts history directly
    if let Err(e) = regenerate_with_guidance(
      &client,
      peer,
      &user,
      &state,
      text.clone(),
      history,
    )
    .await
    {
      error!("Error regenerating with guidance: {}", e);

      // Send error message to user
      bot_client
        .send_message_with_buttons(
          message.chat.id,
          format!("❌ Failed to regenerate: {}", e),
          vec![],
        )
        .await?;
    }
  }

  Ok(())
}

async fn regenerate_with_guidance(
  _client: &Client,
  peer: PeerRef,
  user: &TrackedUser,
  state: &Arc<Mutex<BotState>>,
  guidance: String,
  history: Vec<ChatMessage>,
) -> Result<()> {
  let (
    api_key,
    api_url,
    models,
    temperature,
    bot_client,
    bot_self_id,
    base_system_prompt,
  ) = {
    let lock = state.lock().unwrap();
    (
      lock.config.ai.api_key.clone(),
      lock.config.ai.api_url.clone(),
      lock.config.ai.models.clone(),
      lock.config.ai.temperature,
      lock.bot_client.clone(),
      lock.bot_self_id,
      lock.config.ai.base_system_prompt.clone(),
    )
  };

  // Build the system prompt with optional base prompt and rephrase guidance
  let system_prompt = {
    let mut prompt = String::new();

    // Add base system prompt if configured
    if let Some(base) = base_system_prompt.as_ref() {
      prompt.push_str(base);
      prompt.push_str("\n\n");
    }

    // Add user-specific system prompt
    prompt.push_str(&user.system_prompt);

    // Add rephrase guidance
    prompt.push_str("\n\nAdditional guidance: ");
    prompt.push_str(&guidance);

    prompt
  };

  debug!("Regenerating AI response with guidance");

  let response_text = llm::generate_reply_with_fallback(
    &api_key,
    &api_url,
    models,
    temperature,
    &system_prompt,
    history.clone(),
  )
  .await
  .context("Failed to generate AI reply with guidance")?;

  info!("Regenerated AI response with guidance for user {}", user.name);

  // Send new draft via Bot API with inline buttons
  let target_id = peer.id.bare_id();
  let draft_message = format!(
    "*AI Draft Suggestion for @{}*\n_(Rephrased)_\n\n{}\n\n",
    user.name, response_text
  );

  let callback_data = format!("approve:{}", target_id);
  let rephrase_data = format!("rephrase:{}", target_id);
  let reject_data = format!("reject:{}", target_id);

  let buttons = vec![vec![
    ("✅ Approve".to_string(), callback_data.clone()),
    ("🔄 Rephrase".to_string(), rephrase_data.clone()),
    ("❌ Reject".to_string(), reject_data.clone()),
  ]];

  let message_id = bot_client
    .send_message_with_buttons(bot_self_id, draft_message, buttons)
    .await
    .context("Failed to send rephrased draft via bot")?;

  // Store draft message and history for later retrieval
  {
    let mut lock = state.lock().unwrap();
    lock.draft_messages.insert(callback_data, (target_id, response_text));
    lock.pending_rephrase.insert(target_id, (bot_self_id, message_id, history));
  }

  debug!("Sent rephrased draft message via bot to self");

  Ok(())
}

fn prompt(msg: &str) -> String {
  print!("{}", msg);
  io::stdout().flush().unwrap();
  let mut input = String::new();
  io::stdin().read_line(&mut input).unwrap();
  input.trim().to_string()
}
