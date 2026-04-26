use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct LlmProvider {
    client: Client,
    endpoint: String,
    model: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,  // Must be false — Ollama streams NDJSON by default
}

impl LlmProvider {
    /// `base_url` should be the Ollama base, e.g. `http://localhost:11434`.
    /// The `/api/chat` path is appended automatically.
    pub fn new(base_url: &str, model: &str) -> Self {
        let base = base_url.trim_end_matches('/').trim_end_matches("/api/chat");
        let endpoint = format!("{}/api/chat", base);
        Self {
            client: Client::new(),
            endpoint,
            model: model.to_string(),
        }
    }

    fn parse_json(json_body: serde_json::Value) -> Result<String, Box<dyn std::error::Error>> {
        if let Some(content) = json_body["choices"][0]["message"]["content"].as_str() {
            Ok(content.to_string())
        } else if let Some(content) = json_body["message"]["content"].as_str() {
            Ok(content.to_string())
        } else {
            Err(format!("Failed to parse response. Body: {}", json_body).into())
        }
    }

    /// Simple single-turn chat (no history context).
    pub async fn chat(&self, system_prompt: Option<&str>, user_prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(Message { role: "system".to_string(), content: sys.to_string() });
        }
        messages.push(Message { role: "user".to_string(), content: user_prompt.to_string() });

        let request_body = ChatRequest { model: self.model.clone(), messages, stream: false };
        let res = self.client.post(&self.endpoint).json(&request_body).send().await?;
        Self::parse_json(res.json().await?)
    }

    /// Multi-turn chat with prior conversation history injected as context.
    /// Each tuple is (role, content) — role is "user" or "assistant".
    pub async fn chat_with_context(
        &self,
        system_prompt: &str,
        history: &[(String, String)],
        user_prompt: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let mut messages = vec![Message { role: "system".to_string(), content: system_prompt.to_string() }];
        for (role, content) in history {
            messages.push(Message { role: role.clone(), content: content.clone() });
        }
        messages.push(Message { role: "user".to_string(), content: user_prompt.to_string() });

        let request_body = ChatRequest { model: self.model.clone(), messages, stream: false };
        let res = self.client.post(&self.endpoint).json(&request_body).send().await?;
        Self::parse_json(res.json().await?)
    }
}
