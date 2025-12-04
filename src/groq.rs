use {
  anyhow::{Result, anyhow},
  serde::{Deserialize, Serialize},
  tracing::{debug, trace},
};

#[derive(Serialize, Debug)]
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

pub async fn generate_reply(
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

  debug!("Sending request to Groq API");
  let response = client
    .post(api_url)
    .header("Authorization", format!("Bearer {}", api_key))
    .json(&payload)
    .send()
    .await?;

  if !response.status().is_success() {
    let status = response.status();
    let error_text = response.text().await?;
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
