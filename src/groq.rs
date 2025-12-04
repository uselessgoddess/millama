use {
  anyhow::{Result, anyhow},
  serde::{Deserialize, Serialize},
};

// TODO: make configurable with config.toml 
const GROQ_API_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const MODEL: &str = "meta-llama/llama-4-maverick-17b-128e-instruct"; 

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
  system_prompt: &str,
  history: Vec<ChatMessage>,
) -> Result<String> {
  let client = reqwest::Client::new();

  let mut messages =
    vec![ChatMessage { role: "system".into(), content: system_prompt.into() }];
  messages.extend(history);

  let payload =
    CompletionRequest { model: MODEL.to_string(), messages, temperature: 1.5 };

  let response = client
    .post(GROQ_API_URL)
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
    Ok(choice.message.content.clone())
  } else {
    Err(anyhow!("No choices in response"))
  }
}
