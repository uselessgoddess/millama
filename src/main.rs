mod groq;

use std::{
  collections::HashMap,
  env,
  io::{self, Write},
  sync::{Arc, Mutex},
  time::Duration,
};

use {
  grammers_client::{
    Client, InputMessage, SignInError, Update, UpdatesConfiguration, button,
  },
  grammers_mtsender::SenderPool,
  grammers_session::{
    defs::{PeerId, PeerRef},
    storages::SqliteSession,
  },
  grammers_tl_types::{enums::MessageEntity, types::MessageEntityBold},
};

use {
  anyhow::Result,
  dotenv::dotenv,
  groq::ChatMessage,
  regex::Regex,
  tokio::{task::JoinSet, time::sleep},
};

const SESSION_FILE: &str = "userbot.session";
// TODO: make configurable with config.toml
const DEBOUNCE_SECONDS: u64 = 1;
const HISTORY_LIMIT: usize = 25;

#[derive(Clone, Debug)]
struct TargetConfig {
  name: String,
  system_prompt: String,
}

struct BotState {
  pending_tasks: HashMap<PeerId, tokio::task::AbortHandle>,
  targets: HashMap<PeerId, TargetConfig>,
}

#[tokio::main]
async fn main() -> Result<()> {
  dotenv().ok();
  run_client().await
}

async fn run_client() -> Result<()> {
  let api_id = env::var("TG_API_ID")?.parse().expect("TG_API_ID is NaN");
  let api_hash = env::var("TG_API_HASH")?;
  let groq_api_key = env::var("GROQ_API_KEY")?;

  let mut targets = HashMap::new();

  // TODO: parse targets from config.toml
  let target_id_1: i64 =
    env::var("TARGET_USER_ID").unwrap_or("0".to_string()).parse()?;

  let self_id = PeerId::user(926184623);

  if target_id_1 != 0 {
    targets.insert(
      PeerId::chat(target_id_1),
      TargetConfig {
        name: "John Doe".into(),
        system_prompt: "Be more serious as possible".into(),
      },
    );
  }

  println!("Loaded {} target users.", targets.len());

  let state =
    Arc::new(Mutex::new(BotState { pending_tasks: HashMap::new(), targets }));

  println!("Connecting to Telegram...");
  let session = Arc::new(SqliteSession::open(SESSION_FILE)?);
  let pool = SenderPool::new(session.clone(), api_id);
  let client = Client::new(&pool);
  let SenderPool { runner, updates, handle } = pool;

  let pool_task = tokio::spawn(runner.run());

  if !client.is_authorized().await? {
    let phone = prompt("Phone: ");
    let token = client.request_login_code(&phone, &api_hash).await?;
    let code = prompt("Code: ");
    if let Err(e) = client.sign_in(&token, &code).await {
      if let SignInError::PasswordRequired(token) = e {
        let password = rpassword::prompt_password("2FA Password: ")?;
        client.check_password(token, password).await?;
      } else {
        return Err(e.into());
      }
    }
  }
  println!("Signed in!");

  let mut update_stream =
    client.stream_updates(updates, UpdatesConfiguration::default());
  let mut tasks = JoinSet::new();

  loop {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => break,
        update = update_stream.next() => {
            let update = match update {
                Ok(u) => u,
                Err(e) => {
                    eprintln!("Update error: {}", e);
                    continue;
                }
            };

            let client = client.clone();
            let state = state.clone();
            let groq_key = groq_api_key.clone();

            tasks.spawn(handle_update(client, update, state, groq_key, self_id));
        }
    }
  }

  println!("Shutting down...");
  handle.quit();
  let _ = pool_task.await;
  Ok(())
}

async fn handle_update(
  client: Client,
  update: Update,
  state: Arc<Mutex<BotState>>,
  groq_key: String,
  self_id: PeerId,
) -> Result<()> {
  match update {
    Update::NewMessage(message) => {
      let peer = match message.peer() {
        Ok(peer) => PeerRef::from(peer),
        Err(peer) => peer,
      };

      let target_config = {
        let lock = state.lock().unwrap();
        lock.targets.get(&peer.id).cloned()
      };

      if let Some(config) = target_config {
        if !message.outgoing() || true {
          println!("Message from tracked user {}: {}", peer.id, message.text());

          {
            let mut lock = state.lock().unwrap();
            if let Some(handle) = lock.pending_tasks.remove(&peer.id) {
              handle.abort();
            }
          }

          let client_clone = client.clone();
          let state_clone = state.clone();
          let prompt = config.system_prompt.clone();

          let handle = tokio::spawn(async move {
            sleep(Duration::from_secs(DEBOUNCE_SECONDS)).await;

            {
              let mut lock = state_clone.lock().unwrap();
              lock.pending_tasks.remove(&peer.id);
            }

            println!("Silence detected for {}. Generating draft...", peer.id);

            if let Err(e) = process_ai_draft(
              &client_clone,
              peer,
              &prompt,
              message.text(),
              &groq_key,
            )
            .await
            {
              eprintln!("Error processing AI draft: {}", e);
            }
          });

          let mut lock = state.lock().unwrap();
          lock.pending_tasks.insert(peer.id, handle.abort_handle());

          return Ok(());
        }
      }

      println!("{:?} {:?}", peer.id, self_id);
      if peer.id == self_id {
        let text = message.text().trim().to_lowercase();

        println!("{}", text);
        if ["+", "y", "yes", "ok", "да"].contains(&text.as_str()) {
          println!("{:?}", message.get_reply().await);

          if let Some(reply_to) = message.get_reply().await? {
            println!("{:?}", reply_to);
            let reply_text = reply_to.text();

            if reply_text.contains("--- METADATA ---") {
              handle_approval(&client, &message, &reply_text).await?;
            }
          }
        }
      }
    }
    _ => {}
  }
  Ok(())
}

async fn process_ai_draft(
  client: &Client,
  peer: PeerRef,
  system_prompt: &str,
  user_prompt: &str,
  api_key: &str,
) -> Result<()> {
  // let chat_peer = client
  //   .resolve_peer(peer)
  //   .await
  //   .context("Could not resolve peer to fetch history")?;

  // let mut messages_iter = client.iter_messages(peer).limit(HISTORY_LIMIT);

  let mut history_buf: Vec<ChatMessage> = Vec::new();
  history_buf.push(ChatMessage {
    role: "user".into(),
    content: user_prompt.to_string(),
  });

  //while let Some(msg) = messages_iter.next().await? {
  //  let text = msg.text();
  //  if text.is_empty() {
  //    continue;
  //  }
  //
  //  let role = if msg.outgoing() { "assistant" } else { "user" };
  //
  //  history_buf.insert(
  //    0,
  //    ChatMessage { role: role.to_string(), content: text.to_string() },
  //  );
  //}

  // if history_buf.is_empty() {
  //   return Ok(());
  // }

  let response_text =
    groq::generate_reply(api_key, system_prompt, history_buf).await?;

  let draft_message = format!(
    "AI Draft Suggestion\n\n{}\n\n`{}`\n\n--- METADATA ---\nTARGET_ID:{}\n",
    response_text,
    "-".repeat(20),
    peer.id.bare_id()
  );

  client
    .send_message(
      PeerRef { id: PeerId::self_user(), auth: Default::default() },
      InputMessage::new().text(draft_message).fmt_entities([
        MessageEntity::Bold(MessageEntityBold { offset: 0, length: 19 }),
      ]),
    )
    .await?;

  Ok(())
}

async fn handle_approval(
  client: &Client,
  my_approve_msg: &grammers_client::types::Message,
  draft_text: &str,
) -> Result<()> {
  let re_target = Regex::new(r"TARGET_ID:(-?\d+)").unwrap();

  let target_id = if let Some(caps) = re_target.captures(draft_text) {
    caps[1].parse::<i64>().unwrap_or(0)
  } else {
    0
  };

  if target_id == 0 {
    return Ok(());
  }
  let target =
    PeerRef { id: PeerId::user(target_id), auth: Default::default() };

  let content_part = draft_text.split("--- METADATA ---").next().unwrap_or("");

  let clean_text =
    content_part.lines().skip(2).collect::<Vec<&str>>().join("\n");

  let final_text = clean_text.trim_end_matches(&['\n', '`', '-'][..]).trim();

  if final_text.is_empty() {
    return Ok(());
  }

  println!("Approving message to {}: {}", target.id, final_text);

  let target_peer = client.resolve_peer(target).await?;
  client.send_message(target_peer, final_text).await?;

  if let Some(reply_to) = my_approve_msg.get_reply().await? {
    reply_to.edit(format!("**Sent.**\n\n{}", final_text)).await?;
  }

  my_approve_msg.delete().await?;

  Ok(())
}

fn prompt(msg: &str) -> String {
  print!("{}", msg);
  io::stdout().flush().unwrap();
  let mut input = String::new();
  io::stdin().read_line(&mut input).unwrap();
  input.trim().to_string()
}
