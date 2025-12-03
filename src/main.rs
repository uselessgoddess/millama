mod groq;

use std::{
  env,
  io::{self, Write},
  sync::Arc,
};

use {
  grammers_client::{Client, SignInError, Update, UpdatesConfiguration},
  grammers_mtsender::SenderPool,
  grammers_session::{
    defs::{PeerId, PeerRef},
    storages::SqliteSession,
  },
};

use {anyhow::Result, dotenv::dotenv, tokio::task::JoinSet};

const SESSION_FILE: &str = "userbot.session";
const DRAFT_HEADER: &str = "**AI Draft Response**";

#[tokio::main]
async fn main() -> Result<()> {
  run_client().await
}

async fn handle_update(
  client: Client,
  update: Update,
  target: PeerId,
  groq_api: String,
) -> Result<()> {
  match update {
    Update::NewMessage(message) => {
      let peer = match message.peer() {
        Ok(peer) => PeerRef::from(peer),
        Err(peer) => peer,
      };

      if !message.outgoing() && peer.id.bare_id() == target.bare_id() {
        let text = message.text();
        let split = "=".repeat(35);
        println!("recv message from {:?}", peer);
        println!("\n{split}\n{}\n{split}", text);

        let ai_response = match groq::generate_reply(&groq_api, text).await {
          Ok(resp) => resp,
          Err(e) => {
            eprintln!("Groq error: {}", e);
            format!("Error generating response: {}", e)
          }
        };

        if let Err(e) =
          client.send_message(PeerRef { id: target, ..peer }, ai_response).await
        {
          println!("Failed to respond! {e}");
        };
        return Ok(());
      }

      if message.outgoing() && peer.id == PeerId::self_user() {
        let text = message.text().trim().to_lowercase();

        if ["+", "y", "yes", "ok"].contains(&text.as_str()) {
          if let Some(reply_msg) = message.get_reply().await? {
            let reply_text = reply_msg.text();

            if reply_text.starts_with(DRAFT_HEADER) {
              let mut target_chat_id: i64 = 0;
              let mut target_msg_id: i32 = 0;
              let mut response_text = String::new();
              let mut header_ended = false;

              for line in reply_text.lines() {
                if header_ended {
                  response_text.push_str(line);
                  response_text.push('\n');
                  continue;
                }

                if line.starts_with("ChatID: ") {
                  target_chat_id =
                    line["ChatID: ".len()..].parse().unwrap_or(0);
                } else if line.starts_with("MsgID: ") {
                  target_msg_id = line["MsgID: ".len()..].parse().unwrap_or(0);
                } else if line.is_empty() && target_chat_id != 0 {
                  header_ended = true;
                }
              }

              if target_chat_id != 0 {
                println!("Approving and sending to {}", target_chat_id);

                // client
                //   .send_message(target_chat_id, response_text.trim())
                //   .reply_to(Some(target_msg_id))
                //   .await?;

                reply_msg
                  .edit(format!("Sent.\n\n{}", response_text.trim()))
                  .await?;
                message.delete().await?;
              }
            }
          }
        }
      }
    }
    _ => {}
  }
  Ok(())
}

async fn run_client() -> Result<()> {
  dotenv().ok();

  let api_id =
    env::var("TG_API_ID")?.parse().expect("TG_API_ID must be a number");
  let api_hash = env::var("TG_API_HASH")?;
  let groq_api_key = env::var("GROQ_API_KEY")?;

  let target_user_id: i64 = env::var("TARGET_USER_ID")
    .unwrap_or_else(|_| "0".to_string())
    .parse()
    .unwrap_or(0);

  println!("Connecting to Telegram...");

  let session = Arc::new(SqliteSession::open(SESSION_FILE)?);
  let pool = SenderPool::new(session.clone(), api_id);

  let client = Client::new(&pool);
  let SenderPool { runner, updates, handle } = pool;
  let pool_task = tokio::spawn(runner.run()); // run this sender

  if !client.is_authorized().await? {
    let phone = prompt("Enter your phone number: ");
    println!("prhone: {phone}");
    let token = client.request_login_code(&phone, &api_hash).await.unwrap();
    println!("SOSAL");

    let code = prompt("Enter the code you received: ");
    if let Err(e) = client.sign_in(&token, &code).await {
      if let SignInError::PasswordRequired(password_token) = e {
        let password = rpassword::prompt_password("Enter your 2FA password: ")?;
        client.check_password(password_token, password).await?;
      } else {
        return Err(e.into());
      }
    }
  }

  println!("Successfully signed in!");

  println!("Bot started! Waiting for messages...");
  if target_user_id == 0 {
    println!("TARGET_USER_ID is not set");
  }

  let mut handler_tasks = JoinSet::new();
  let mut updates = client.stream_updates(
    updates,
    UpdatesConfiguration { catch_up: true, ..Default::default() },
  );

  let target = PeerId::user(target_user_id);

  loop {
    while let Some(_) = handler_tasks.try_join_next() {}

    let groq_api = groq_api_key.clone();
    tokio::select! {
      _ = tokio::signal::ctrl_c() => break,
      update = updates.next() => {
          let update = update?;
          let handle = client.clone();
          handler_tasks.spawn(handle_update(handle, update, target, groq_api));
      }
    }
  }
  println!("Saving session file...");
  updates.sync_update_state();

  println!("Gracefully closing connection to notify all pending handlers...");
  handle.quit();

  let _ = pool_task.await;
  println!("Waiting for any slow handlers to finish...");
  while let Some(_) = handler_tasks.join_next().await {}

  Ok(())
}

fn prompt(msg: &str) -> String {
  print!("{}", msg);
  io::stdout().flush().unwrap();
  let mut input = String::new();
  io::stdin().read_line(&mut input).unwrap();
  input.trim().to_string()
}
