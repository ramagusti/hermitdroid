use crate::config::BrainConfig;
use crate::soul::BootstrapContext;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

#[derive(Debug, Clone)]
pub struct Brain {
    config: BrainConfig,
    client: reqwest::Client,
}

/// Structured response from the LLM
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentResponse {
    #[serde(default)]
    pub actions: Vec<AgentAction>,
    #[serde(default)]
    pub reflection: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub memory_write: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAction {
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default = "default_green")]
    pub classification: String,
    #[serde(default)]
    pub reason: String,
}

fn default_green() -> String { "GREEN".into() }

impl Brain {
    pub fn new(config: &BrainConfig) -> Self {
        Self {
            config: config.clone(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
        }
    }

    pub fn model_name(&self) -> &str { &self.config.model }

    /// Build the full system prompt from workspace bootstrap context
    pub fn build_system_prompt(&self, ctx: &BootstrapContext) -> String {
        let mut prompt = String::new();

        // Inject workspace files (same order as OpenClaw)
        if !ctx.soul.is_empty() {
            prompt.push_str(&format!("--- SOUL.md ---\n{}\n\n", ctx.soul));
        }
        if !ctx.identity.is_empty() {
            prompt.push_str(&format!("--- IDENTITY.md ---\n{}\n\n", ctx.identity));
        }
        if !ctx.agents.is_empty() {
            prompt.push_str(&format!("--- AGENTS.md ---\n{}\n\n", ctx.agents));
        }
        if !ctx.tools.is_empty() {
            prompt.push_str(&format!("--- TOOLS.md ---\n{}\n\n", ctx.tools));
        }
        if !ctx.user.is_empty() {
            prompt.push_str(&format!("--- USER.md ---\n{}\n\n", ctx.user));
        }
        if !ctx.heartbeat.is_empty() {
            prompt.push_str(&format!("--- HEARTBEAT.md ---\n{}\n\n", ctx.heartbeat));
        }

        // Bootstrap (first run only)
        if let Some(bootstrap) = &ctx.bootstrap {
            prompt.push_str(&format!("--- BOOTSTRAP.md (FIRST RUN) ---\n{}\n\n", bootstrap));
        }

        // Skills (selectively injected)
        for skill in &ctx.skills {
            prompt.push_str(&format!("--- SKILL: {} ---\n{}\n\n", skill.name, skill.content));
        }

        prompt
    }

    /// Build the user prompt for a heartbeat tick
    pub fn build_tick_prompt(
        &self,
        ctx: &BootstrapContext,
        notifications: &str,
        screen_state: &str,
        user_commands: &[String],
        now: &str,
    ) -> String {
        let mut prompt = String::new();

        prompt.push_str(&format!("Current time: {}\n\n", now));

        if !ctx.goals.is_empty() {
            prompt.push_str(&format!("--- Active Goals ---\n{}\n\n", ctx.goals));
        }

        if !ctx.memory.is_empty() {
            prompt.push_str(&format!("--- Long-term Memory ---\n{}\n\n", ctx.memory));
        }

        prompt.push_str(&format!("--- New Notifications ---\n{}\n\n", notifications));
        prompt.push_str(&format!("--- Screen State ---\n{}\n\n", screen_state));

        if !user_commands.is_empty() {
            prompt.push_str("--- User Commands ---\n");
            for cmd in user_commands {
                prompt.push_str(&format!("- {}\n", cmd));
            }
            prompt.push('\n');
        }

        prompt.push_str("Evaluate the heartbeat checklist. Respond with your JSON action plan, or HEARTBEAT_OK if nothing needs attention.");

        prompt
    }

    /// Chat: direct user message (not a heartbeat tick)
    pub fn build_chat_prompt(&self, ctx: &BootstrapContext, user_message: &str) -> String {
        format!(
            "--- Long-term Memory ---\n{}\n\n--- Goals ---\n{}\n\nUser message: {}",
            ctx.memory, ctx.goals, user_message
        )
    }

    /// Send prompt to LLM and get raw response
    pub async fn think(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        image_base64: Option<&str>,
    ) -> anyhow::Result<String> {
        match self.config.backend.as_str() {
            "ollama" => self.ollama(system_prompt, user_prompt, image_base64).await,
            "openai_compatible" | "llamacpp" => {
                self.openai_compat(system_prompt, user_prompt, image_base64).await
            }
            other => anyhow::bail!("Unknown backend: {}", other),
        }
    }

    /// Parse raw LLM text into structured AgentResponse
    pub fn parse_response(&self, raw: &str) -> AgentResponse {
        let trimmed = raw.trim();

        // Check for HEARTBEAT_OK
        if trimmed.contains("HEARTBEAT_OK") {
            return AgentResponse {
                reflection: Some("HEARTBEAT_OK".into()),
                ..Default::default()
            };
        }

        // Try to extract JSON
        if let Some(json_str) = extract_json(trimmed) {
            if let Ok(resp) = serde_json::from_str::<AgentResponse>(&json_str) {
                return resp;
            }
        }

        // Fallback
        AgentResponse {
            reflection: Some(trimmed.to_string()),
            message: Some(trimmed.to_string()),
            ..Default::default()
        }
    }

    // ---- Backend implementations ----

    async fn ollama(
        &self,
        system: &str,
        user: &str,
        image: Option<&str>,
    ) -> anyhow::Result<String> {
        let url = format!("{}/api/generate", self.config.endpoint);
        let mut body = serde_json::json!({
            "model": self.config.model,
            "system": system,
            "prompt": user,
            "stream": false,
            "options": {
                "temperature": self.config.temperature,
                "num_predict": self.config.max_tokens,
            }
        });
        if let Some(img) = image {
            body["images"] = serde_json::json!([img]);
        }

        let resp = self.client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("Ollama error {}: {}", resp.status(), resp.text().await.unwrap_or_default());
        }
        let result: serde_json::Value = resp.json().await?;
        Ok(result["response"].as_str().unwrap_or("").to_string())
    }

    async fn openai_compat(
        &self,
        system: &str,
        user: &str,
        image: Option<&str>,
    ) -> anyhow::Result<String> {
        let url = format!("{}/chat/completions", self.config.endpoint);
        let user_content = if let Some(img) = image {
            serde_json::json!([
                {"type": "text", "text": user},
                {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{}", img)}}
            ])
        } else {
            serde_json::json!(user)
        };

        let body = serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user_content}
            ],
            "max_tokens": self.config.max_tokens,
            "temperature": self.config.temperature,
        });

        let mut req = self.client.post(&url).json(&body);
        if let Some(key) = &self.config.api_key {
            if !key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("LLM API error {}: {}", resp.status(), resp.text().await.unwrap_or_default());
        }
        let result: serde_json::Value = resp.json().await?;
        Ok(result["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string())
    }
}

fn extract_json(text: &str) -> Option<String> {
    // Direct JSON
    if text.starts_with('{') {
        // Find matching closing brace
        let mut depth = 0;
        for (i, ch) in text.chars().enumerate() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(text[..=i].to_string());
                    }
                }
                _ => {}
            }
        }
    }
    // Code block
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        if let Some(end) = after.find("```") {
            let inner = after[..end].trim();
            if inner.starts_with('{') {
                return Some(inner.to_string());
            }
        }
    }
    // Find embedded JSON
    if let Some(start) = text.find('{') {
        let mut depth = 0;
        for (i, ch) in text[start..].chars().enumerate() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(text[start..start + i + 1].to_string());
                    }
                }
                _ => {}
            }
        }
    }
    None
}
