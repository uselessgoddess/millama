use {
  anyhow::{Result, anyhow},
  serde::{Deserialize, Serialize},
  tracing::{debug, trace, warn},
};

#[derive(Serialize, Debug, Clone)]
pub struct ChatMessage {
  pub role: String,
  pub content: String,
}

#[derive(Serialize)]
struct CompletionRequest {
  model: String,
  messages: Vec<ChatMessage>,
  temperature: f32,
}

#[derive(Deserialize)]
struct CompletionResponse {
  choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
  message: MessageContent,
}

#[derive(Deserialize)]
struct MessageContent {
  content: String,
}

#[allow(dead_code)]
pub async fn generate_reply(
  api_key: &str,
  api_url: &str,
  model: &str,
  temperature: f32,
  system_prompt: &str,
  history: Vec<ChatMessage>,
) -> Result<String> {
  generate_reply_with_model(
    api_key,
    api_url,
    model,
    temperature,
    system_prompt,
    history,
  )
  .await
}

pub async fn generate_reply_with_fallback(
  api_key: &str,
  api_url: &str,
  models: Vec<String>,
  temperature: f32,
  system_prompt: &str,
  history: Vec<ChatMessage>,
) -> Result<String> {
  if models.is_empty() {
    return Err(anyhow!("No models configured"));
  }

  let mut last_error = None;

  for (idx, model) in models.iter().enumerate() {
    debug!("Trying model {} of {}: {}", idx + 1, models.len(), model);

    match generate_reply_with_model(
      api_key,
      api_url,
      model,
      temperature,
      system_prompt,
      history.clone(),
    )
    .await
    {
      Ok(response) => {
        if idx > 0 {
          debug!("Successfully generated reply with fallback model: {}", model);
        }
        return Ok(response);
      }
      Err(e) => {
        warn!("Model {} failed: {}", model, e);
        last_error = Some(e);
      }
    }
  }

  Err(last_error.unwrap_or_else(|| anyhow!("All models failed")))
}

async fn generate_reply_with_model(
  api_key: &str,
  api_url: &str,
  model: &str,
  temperature: f32,
  system_prompt: &str,
  history: Vec<ChatMessage>,
) -> Result<String> {
  debug!("Generating reply with model: {}", model);
  trace!("System prompt: {}", system_prompt);
  trace!("History length: {}", history.len());

  let client = reqwest::Client::new();

  let mut messages =
    vec![ChatMessage { role: "system".into(), content: system_prompt.into() }];
  messages.extend(history);

  let payload =
    CompletionRequest { model: model.to_string(), messages, temperature };

  debug!("Sending request to OpenAI-compatible API");
  let response = client
    .post(api_url)
    .header("Authorization", format!("Bearer {}", api_key))
    .json(&payload)
    .send()
    .await?;

  let status = response.status();

  if !status.is_success() {
    let error_text = response.text().await?;

    // Check for rate limiting (429) specifically
    if status.as_u16() == 429 {
      warn!("Rate limit (429) reached for model: {}", model);
      return Err(anyhow!("Rate limit (429): {}", error_text));
    }

    return Err(anyhow!("API Error {}: {}", status, error_text));
  }

  let resp_json = response.json::<CompletionResponse>().await?;

  if let Some(choice) = resp_json.choices.first() {
    debug!("Successfully generated reply");
    trace!("Reply content: {}", choice.message.content);
    Ok(choice.message.content.clone())
  } else {
    Err(anyhow!("No choices in response"))
  }
}
