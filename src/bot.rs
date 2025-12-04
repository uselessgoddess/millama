use {
  anyhow::{Context, Result},
  serde::{Deserialize, Serialize},
  tracing::{debug, trace},
};

pub struct BotClient {
  token: String,
  client: reqwest::Client,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
  chat_id: i64,
  text: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  parse_mode: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  reply_markup: Option<InlineKeyboardMarkup>,
}

#[derive(Debug, Serialize)]
struct InlineKeyboardMarkup {
  inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

#[derive(Debug, Serialize)]
struct InlineKeyboardButton {
  text: String,
  callback_data: String,
}

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
  ok: bool,
  #[serde(default)]
  description: Option<String>,
  result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct Message {
  message_id: i64,
  #[allow(dead_code)]
  chat: Chat,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
  pub id: i64,
}

#[derive(Debug, Serialize)]
struct EditMessageTextRequest {
  chat_id: i64,
  message_id: i64,
  text: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  parse_mode: Option<String>,
}

#[derive(Debug, Serialize)]
struct AnswerCallbackQueryRequest {
  callback_query_id: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Update {
  pub update_id: i64,
  #[serde(default)]
  pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
  pub id: String,
  #[allow(dead_code)]
  pub from: User,
  pub message: Option<CallbackMessage>,
  pub data: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct User {
  #[allow(dead_code)]
  pub id: i64,
}

#[derive(Debug, Deserialize)]
pub struct CallbackMessage {
  pub message_id: i64,
  pub chat: Chat,
}

#[derive(Debug, Serialize)]
struct GetUpdatesRequest {
  offset: Option<i64>,
  timeout: u32,
}

impl BotClient {
  pub fn new(token: String) -> Self {
    Self { token, client: reqwest::Client::new() }
  }

  fn api_url(&self, method: &str) -> String {
    format!("https://api.telegram.org/bot{}/{}", self.token, method)
  }

  pub async fn send_message_with_buttons(
    &self,
    chat_id: i64,
    text: String,
    buttons: Vec<Vec<(String, String)>>,
  ) -> Result<i64> {
    let inline_keyboard = buttons
      .into_iter()
      .map(|row| {
        row
          .into_iter()
          .map(|(text, callback_data)| InlineKeyboardButton {
            text,
            callback_data,
          })
          .collect()
      })
      .collect();

    let request = SendMessageRequest {
      chat_id,
      text,
      parse_mode: Some("Markdown".to_string()),
      reply_markup: Some(InlineKeyboardMarkup { inline_keyboard }),
    };

    trace!("Sending message with buttons to chat {}", chat_id);

    let http_response = self
      .client
      .post(self.api_url("sendMessage"))
      .json(&request)
      .send()
      .await
      .context("Failed to send HTTP request")?;

    let status = http_response.status();

    // Handle rate limiting
    if status.as_u16() == 429 {
      let error_text = http_response.text().await.unwrap_or_default();
      debug!("Bot API rate limit (429) reached: {}", error_text);
      anyhow::bail!("Bot API rate limit (429): {}", error_text);
    }

    let response_text =
      http_response.text().await.context("Failed to read response body")?;

    trace!("Bot API response: {}", response_text);

    let response: TelegramResponse<Message> = json::from_str(&response_text)
      .context(format!("Failed to parse response: {}", response_text))?;

    if !response.ok {
      let error_desc =
        response.description.unwrap_or_else(|| "Unknown error".to_string());
      debug!("Telegram API error: {}", error_desc);
      anyhow::bail!("Telegram API error: {}", error_desc);
    }

    let message = response.result.context("Missing result in response")?;

    debug!("Sent message {} to chat {}", message.message_id, chat_id);

    Ok(message.message_id)
  }

  pub async fn edit_message_text(
    &self,
    chat_id: i64,
    message_id: i64,
    text: String,
  ) -> Result<()> {
    let request = EditMessageTextRequest {
      chat_id,
      message_id,
      text,
      parse_mode: Some("Markdown".to_string()),
    };

    trace!("Editing message {} in chat {}", message_id, chat_id);

    let http_response = self
      .client
      .post(self.api_url("editMessageText"))
      .json(&request)
      .send()
      .await
      .context("Failed to send HTTP request")?;

    let status = http_response.status();

    // Handle rate limiting
    if status.as_u16() == 429 {
      let error_text = http_response.text().await.unwrap_or_default();
      debug!("Bot API rate limit (429) reached: {}", error_text);
      anyhow::bail!("Bot API rate limit (429): {}", error_text);
    }

    let response_text =
      http_response.text().await.context("Failed to read response body")?;

    trace!("Bot API response: {}", response_text);

    let response: TelegramResponse<Message> = json::from_str(&response_text)
      .context(format!("Failed to parse response: {}", response_text))?;

    if !response.ok {
      let error_desc =
        response.description.unwrap_or_else(|| "Unknown error".to_string());
      debug!("Telegram API error: {}", error_desc);
      anyhow::bail!("Telegram API error: {}", error_desc);
    }

    debug!("Edited message {} in chat {}", message_id, chat_id);

    Ok(())
  }

  pub async fn answer_callback_query(
    &self,
    callback_query_id: &str,
    text: Option<String>,
  ) -> Result<()> {
    let request = AnswerCallbackQueryRequest {
      callback_query_id: callback_query_id.to_string(),
      text,
    };

    trace!("Answering callback query {}", callback_query_id);

    let response = self
      .client
      .post(self.api_url("answerCallbackQuery"))
      .json(&request)
      .send()
      .await
      .context("Failed to send HTTP request")?;

    let response: TelegramResponse<bool> =
      response.json().await.context("Failed to parse response")?;

    if !response.ok {
      anyhow::bail!(
        "Telegram API error: {}",
        response.description.unwrap_or_else(|| "Unknown error".to_string())
      );
    }

    debug!("Answered callback query {}", callback_query_id);

    Ok(())
  }

  pub async fn get_updates(&self, offset: Option<i64>) -> Result<Vec<Update>> {
    let request = GetUpdatesRequest { offset, timeout: 30 };

    trace!("Getting updates with offset {:?}", offset);

    let response = self
      .client
      .post(self.api_url("getUpdates"))
      .json(&request)
      .send()
      .await
      .context("Failed to send HTTP request")?;

    let response: TelegramResponse<Vec<Update>> =
      response.json().await.context("Failed to parse response")?;

    if !response.ok {
      anyhow::bail!(
        "Telegram API error: {}",
        response.description.unwrap_or_else(|| "Unknown error".to_string())
      );
    }

    let updates = response.result.unwrap_or_default();

    debug!("Received {} updates", updates.len());

    Ok(updates)
  }
}
