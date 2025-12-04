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
  grammers_client::{
    Client, InputMessage, SignInError, Update, UpdatesConfiguration,
  },
  grammers_mtsender::SenderPool,
  grammers_session::{
    defs::{PeerId, PeerRef},
    storages::SqliteSession,
  },
  grammers_tl_types::{enums::MessageEntity, types::MessageEntityBold},
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

  let state = Arc::new(Mutex::new(BotState {
    pending_tasks: HashMap::new(),
    users: users_map,
    config: config.clone(),
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

  let self_id = PeerId::self_user();
  info!("Running as self user");

  let mut update_stream =
    client.stream_updates(updates, UpdatesConfiguration::default());
  let mut tasks = JoinSet::new();

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

            tasks.spawn(handle_update(client, update, state, self_id));
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
  self_id: PeerId,
) -> Result<()> {
  if let Update::NewMessage(message) = update {
    let peer = match message.peer() {
      Ok(peer) => PeerRef::from(peer),
      Err(peer) => peer,
    };

    trace!("Message from user ({}): {}", peer.id, message.text());

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

    // Handle approval messages
    if peer.id == self_id {
      let text = message.text().trim();
      debug!("Message to self: {}", text);

      // Check for approval keywords
      if ["+", "y", "yes", "ok", "да", "approve"]
        .contains(&text.to_lowercase().as_str())
        && let Some(reply_to) = message.get_reply().await?
      {
        let reply_text = reply_to.text();

        if reply_text.contains("--- METADATA ---") {
          debug!("Found metadata in reply, processing approval");
          handle_approval(&client, &message, reply_text).await?;
        }
      }
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
  let (api_key, api_url, model, temperature, history_limit) = {
    let lock = state.lock().unwrap();
    (
      lock.config.ai.api_key.clone(),
      lock.config.ai.api_url.clone(),
      lock.config.ai.model.clone(),
      lock.config.ai.temperature,
      lock.config.settings.history_limit,
    )
  };

  // Fetch message history
  let mut history_buf: Vec<ChatMessage> = Vec::new();

  debug!("Fetching message history for peer {}", peer.id);
  let chat_peer = client
    .resolve_peer(peer)
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

  let response_text = llm::generate_reply(
    &api_key,
    &api_url,
    &model,
    temperature,
    &user.system_prompt,
    history_buf,
  )
  .await
  .context("Failed to generate AI reply")?;

  info!("Generated AI response for user {}", user.name);

  let draft_message = format!(
    concat!(
      "**AI Draft Suggestion for {}**\n\n{}\n\n`{}`\n\n",
      "Reply with '+', 'yes', or 'approve' to send\n\n",
      "--- METADATA ---\nTARGET_ID:{}\n"
    ),
    user.name,
    response_text,
    "-".repeat(20),
    peer.id.bare_id()
  );

  // Send draft to self
  client
    .send_message(
      PeerRef { id: PeerId::self_user(), auth: Default::default() },
      InputMessage::new().text(draft_message).fmt_entities([
        MessageEntity::Bold(MessageEntityBold {
          offset: 0,
          length: (32 + user.name.len()) as i32,
        }),
      ]),
    )
    .await
    .context("Failed to send draft message")?;

  debug!("Sent draft message to self");

  Ok(())
}

async fn handle_approval(
  client: &Client,
  my_approve_msg: &grammers_client::types::Message,
  draft_text: &str,
) -> Result<()> {
  // Parse metadata from draft text
  let target_id = draft_text
    .lines()
    .find(|line| line.starts_with("TARGET_ID:"))
    .and_then(|line| line.strip_prefix("TARGET_ID:"))
    .and_then(|s| s.parse::<i64>().ok())
    .context("Failed to parse TARGET_ID")?;

  info!("Approving message to target ID: {}", target_id);

  let target =
    PeerRef { id: PeerId::chat(target_id), auth: Default::default() };

  // Extract the actual message content
  let content_part = draft_text.split("--- METADATA ---").next().unwrap_or("");
  let lines: Vec<&str> = content_part.lines().collect();

  // Skip header and instructions
  let clean_text = lines
    .iter()
    .skip_while(|line| {
      line.trim().is_empty() || line.contains("AI Draft") || line.contains("**")
    })
    .take_while(|line| !line.contains("Reply with") && !line.starts_with('`'))
    .copied()
    .collect::<Vec<&str>>()
    .join("\n");

  let final_text = clean_text.trim();

  if final_text.is_empty() {
    warn!("Final text is empty, aborting approval");
    return Ok(());
  }

  debug!("Sending approved message: {}", final_text);

  let target_peer = client.resolve_peer(target).await?;
  client
    .send_message(target_peer, final_text)
    .await
    .context("Failed to send approved message")?;

  if let Some(reply_to) = my_approve_msg.get_reply().await? {
    reply_to
      .edit(format!("✅ **Sent.**\n\n{}", final_text))
      .await
      .context("Failed to edit reply")?;
  }

  my_approve_msg.delete().await.context("Failed to delete approval message")?;

  info!("Message sent successfully to {}", target_id);

  Ok(())
}

fn prompt(msg: &str) -> String {
  print!("{}", msg);
  io::stdout().flush().unwrap();
  let mut input = String::new();
  io::stdin().read_line(&mut input).unwrap();
  input.trim().to_string()
}
