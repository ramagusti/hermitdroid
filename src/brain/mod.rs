use crate::config::BrainConfig;
use crate::soul::BootstrapContext;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Codex OAuth token data read from ~/.codex/auth.json
#[derive(Debug, Clone, Deserialize)]
struct CodexAuthFile {
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    tokens: Option<CodexTokens>,
    last_refresh: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexTokens {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    account_id: Option<String>,
}

/// Cached token with expiry tracking
#[derive(Debug, Clone)]
struct CachedCodexToken {
    access_token: String,
    loaded_at: std::time::Instant,
}

#[derive(Debug, Clone)]
pub struct Brain {
    config: BrainConfig,
    client: reqwest::Client,
    /// Cached Codex OAuth token (reloaded from disk periodically)
    codex_token: Arc<RwLock<Option<CachedCodexToken>>>,
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

/// Token cache duration — reload from disk every 7 minutes
/// (Codex tokens refresh every ~8 minutes before expiry)
const TOKEN_CACHE_SECS: u64 = 7 * 60;

impl Brain {
    pub fn new(config: &BrainConfig) -> Self {
        let brain = Self {
            config: config.clone(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
            codex_token: Arc::new(RwLock::new(None)),
        };

        // Pre-load token if backend is codex_oauth
        if config.backend == "codex_oauth" {
            if let Some(_token) = Self::load_codex_token_from_disk(&config.codex_auth_path) {
                info!("🔑 Codex OAuth: token loaded from {}", config.codex_auth_path.as_deref().unwrap_or("~/.codex/auth.json"));
            } else {
                warn!("⚠️  Codex OAuth: no token found. Run `codex login` or `npm i -g @openai/codex && codex login`");
            }
        }

        brain
    }

    pub fn model_name(&self) -> &str { &self.config.model }

    /// Load the Codex access token from ~/.codex/auth.json (or custom path)
    fn load_codex_token_from_disk(custom_path: &Option<String>) -> Option<String> {
        let path = custom_path.clone().unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            format!("{}/.codex/auth.json", home)
        });

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                warn!("⚠️  Could not read Codex auth file at {}: {}", path, e);
                return None;
            }
        };

        let auth: CodexAuthFile = match serde_json::from_str(&content) {
            Ok(a) => a,
            Err(e) => {
                warn!("⚠️  Could not parse Codex auth file: {}", e);
                return None;
            }
        };

        // Prefer access_token from tokens object
        if let Some(tokens) = &auth.tokens {
            if let Some(ref token) = tokens.access_token {
                if !token.is_empty() {
                    debug!("Codex OAuth: using access_token from tokens object");
                    return Some(token.clone());
                }
            }
        }

        // Fallback to OPENAI_API_KEY field
        if let Some(ref key) = auth.openai_api_key {
            if !key.is_empty() {
                debug!("Codex OAuth: using OPENAI_API_KEY from auth file");
                return Some(key.clone());
            }
        }

        warn!("⚠️  Codex auth file exists but contains no usable token");
        None
    }

    /// Get a valid Codex token, reloading from disk if cache is stale
    async fn get_codex_token(&self) -> anyhow::Result<String> {
        // Check cache first
        {
            let cached = self.codex_token.read().await;
            if let Some(ref ct) = *cached {
                if ct.loaded_at.elapsed().as_secs() < TOKEN_CACHE_SECS {
                    return Ok(ct.access_token.clone());
                }
                debug!("Codex OAuth: token cache expired, reloading from disk");
            }
        }

        // Reload from disk
        let token = Self::load_codex_token_from_disk(&self.config.codex_auth_path)
            .ok_or_else(|| anyhow::anyhow!(
                "No Codex OAuth token found. Run `codex login` to authenticate with ChatGPT."
            ))?;

        // Update cache
        {
            let mut cached = self.codex_token.write().await;
            *cached = Some(CachedCodexToken {
                access_token: token.clone(),
                loaded_at: std::time::Instant::now(),
            });
        }

        info!("🔑 Codex OAuth: token refreshed from disk");
        Ok(token)
    }

    /// Build the full system prompt from workspace bootstrap context
    pub fn build_system_prompt(&self, ctx: &BootstrapContext) -> String {
        let mut prompt = String::new();

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
        if let Some(bootstrap) = &ctx.bootstrap {
            prompt.push_str(&format!("--- BOOTSTRAP.md (FIRST RUN) ---\n{}\n\n", bootstrap));
        }
        for skill in &ctx.skills {
            prompt.push_str(&format!("--- SKILL: {} ---\n{}\n\n", skill.name, skill.content));
        }

        // Vision instructions (when screenshots are enabled)
        if self.config.vision_enabled {
            prompt.push_str(
                r#"--- VISION INSTRUCTIONS ---
                You have access to a screenshot of the phone screen. When a screenshot is attached:
                1. LOOK at the screenshot to identify exact positions of UI elements (buttons, text fields, icons)
                2. Use the VISIBLE coordinates from the screenshot for all tap/click actions
                3. The screen resolution is 1080x2340. Estimate x,y coordinates based on where elements appear in the image
                4. DO NOT guess coordinates from memory — always derive them from the screenshot
                5. If the screenshot shows a different screen than expected, adjust your plan accordingly
                6. Common WhatsApp elements:
                - Search icon: usually top-right area, look for magnifying glass icon
                - Message input: bottom of chat screen, look for "Type a message" text field
                - Send button: right side of message input field, look for arrow/send icon
                - Chat list items: middle of screen, each chat takes about 80px height
                7. When you see the UI Tree alongside the screenshot, cross-reference both:
                - UI Tree gives exact bounds like @(x,y) — USE THESE when available
                - Screenshot confirms what's actually visible on screen
                "#
            );
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
            "codex_oauth" => {
                self.codex_oauth(system_prompt, user_prompt, image_base64).await
            }
            other => anyhow::bail!("Unknown backend: {}", other),
        }
    }

    /// Parse raw LLM text into structured AgentResponse
    pub fn parse_response(&self, raw: &str) -> AgentResponse {
        let trimmed = raw.trim();
        if trimmed.contains("HEARTBEAT_OK") {
            return AgentResponse {
                reflection: Some("HEARTBEAT_OK".into()),
                ..Default::default()
            };
        }

        let sanitized = sanitize_llm_json(trimmed);

        // Try normal parse
        if let Some(json_str) = extract_json(&sanitized) {
            if let Some(resp) = self.try_parse_json(&json_str) {
                return resp;
            }
        }

        // Try repairing truncated JSON
        let repaired = repair_truncated_json(&sanitized);
        if let Some(json_str) = extract_json(&repaired) {
            if let Some(resp) = self.try_parse_json(&json_str) {
                warn!("Recovered actions from truncated JSON response");
                return resp;
            }
        }

        // Try extracting individual actions from broken JSON
        if let Some(actions) = extract_partial_actions(&sanitized) {
            if !actions.is_empty() {
                warn!("Extracted {} action(s) from malformed JSON", actions.len());
                return AgentResponse {
                    actions,
                    reflection: Some("(partial response recovered)".into()),
                    ..Default::default()
                };
            }
        }

        warn!("Could not parse any JSON from LLM response (len={})", trimmed.len());
        AgentResponse {
            reflection: Some(trimmed.chars().take(500).collect()),
            message: None,
            ..Default::default()
        }
    }

    fn try_parse_json(&self, json_str: &str) -> Option<AgentResponse> {
        if let Ok(resp) = serde_json::from_str::<AgentResponse>(json_str) {
            return Some(resp);
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
            return Some(AgentResponse {
                actions: val.get("actions")
                    .and_then(|a| serde_json::from_value(a.clone()).ok())
                    .unwrap_or_default(),
                reflection: val.get("reflection")
                    .and_then(|v| v.as_str()).map(String::from),
                message: val.get("message")
                    .and_then(|v| v.as_str()).map(String::from),
                memory_write: val.get("memory_write")
                    .and_then(|v| v.as_str()).map(String::from),
            });
        }
        None
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

    /// Codex OAuth backend — uses the Responses API at chatgpt.com/backend-api/codex/responses
    /// This endpoint REQUIRES stream:true and returns Server-Sent Events (SSE).
    /// We collect the text deltas from the stream and return the full text.
    /// Reference: https://simonwillison.net/2025/Nov/9/gpt-5-codex-mini/
    async fn codex_oauth(
        &self,
        system: &str,
        user: &str,
        image: Option<&str>,
    ) -> anyhow::Result<String> {
        let token = self.get_codex_token().await?;

        let url = "https://chatgpt.com/backend-api/codex/responses";

        // Build input array in OpenAI Responses API format
        let mut input = vec![
            serde_json::json!({
                "type": "message",
                "role": "developer",
                "content": [
                    {
                        "type": "input_text",
                        "text": system
                    }
                ]
            }),
        ];

        // User message — with optional image
        if let Some(img) = image {
            input.push(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": user
                    },
                    {
                        "type": "input_image",
                        "image_url": format!("data:image/png;base64,{}", img)
                    }
                ]
            }));
        } else {
            input.push(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": user
                    }
                ]
            }));
        }

        // Build the Responses API request body.
        // stream MUST be true — the Codex backend rejects stream:false.
        let body = serde_json::json!({
            "model": self.config.model,
            "instructions": system,
            "input": input,
            "tools": [],
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "store": false,
            "stream": true,
        });

        debug!("Codex OAuth: POST {} model={}", url, self.config.model);

        let resp = self.client
            .post(url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await?;

        if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
            warn!("🔑 Codex OAuth: token rejected ({}). Clearing cache — will reload on next tick.", resp.status());
            warn!("   If this persists, run `codex login` to re-authenticate.");
            let mut cached = self.codex_token.write().await;
            *cached = None;
            anyhow::bail!("Codex OAuth: authentication failed ({}). Run `codex login` to refresh.", resp.status());
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Codex OAuth API error {} {}: {}", status.as_u16(), status, body_text);
        }

        // Parse the SSE stream to collect the full response text.
        // The stream sends events like:
        //   data: {"type":"response.output_text.delta","delta":"Hello"}
        //   data: {"type":"response.output_text.delta","delta":" world"}
        //   data: {"type":"response.completed","response":{"output_text":"Hello world",...}}
        //   data: [DONE]
        let full_body = resp.text().await?;
        let mut collected_text = String::new();
        let mut got_completed = false;

        for line in full_body.lines() {
            let line = line.trim();

            // Skip empty lines and SSE comments
            if line.is_empty() || line.starts_with(':') {
                continue;
            }

            // Extract the data payload from "data: {...}"
            if let Some(data) = line.strip_prefix("data: ") {
                let data = data.trim();

                // Stream terminator
                if data == "[DONE]" {
                    break;
                }

                // Try to parse the JSON event
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                    let event_type = event["type"].as_str().unwrap_or("");

                    match event_type {
                        // Text delta — accumulate the output
                        "response.output_text.delta" => {
                            if let Some(delta) = event["delta"].as_str() {
                                collected_text.push_str(delta);
                            }
                        }
                        // Response completed — try to grab output_text from the full response
                        "response.completed" => {
                            got_completed = true;
                            if let Some(output_text) = event["response"]["output_text"].as_str() {
                                if !output_text.is_empty() {
                                    // Use the final complete text instead of deltas
                                    collected_text = output_text.to_string();
                                }
                            }
                        }
                        // Ignore other events (response.created, response.in_progress,
                        // response.output_item.added, response.content_part.added,
                        // response.content_part.done, response.output_item.done, etc.)
                        _ => {}
                    }
                }
            }
        }

        if collected_text.is_empty() && !got_completed {
            warn!("Codex OAuth: stream ended but no text collected. Raw body length: {}", full_body.len());
            // Log first 500 chars for debugging
            let preview: String = full_body.chars().take(500).collect();
            warn!("Codex OAuth: stream preview: {}", preview);
            anyhow::bail!("Codex OAuth: received empty response from stream");
        }

        debug!("Codex OAuth: received {} chars", collected_text.len());
        Ok(collected_text)
    }
}

/// Sanitize common LLM JSON issues:
/// - Curly/smart quotes → straight quotes
/// - Em/en dashes → regular dashes
/// - Trailing commas before } or ]
/// - BOM and other invisible chars
fn sanitize_llm_json(text: &str) -> String {
    let mut s = text.to_string();

    // Replace Unicode curly/smart quotes with ASCII equivalents
    // These are the #1 cause of LLM JSON parse failures
    s = s.replace('\u{201c}', "\\\"");  // left double curly quote "
    s = s.replace('\u{201d}', "\\\"");  // right double curly quote "
    s = s.replace('\u{2018}', "'");     // left single curly quote '
    s = s.replace('\u{2019}', "'");     // right single curly quote '
    s = s.replace('\u{00ab}', "\\\"");  // left guillemet «
    s = s.replace('\u{00bb}', "\\\"");  // right guillemet »

    // Replace em/en dashes with regular dashes
    s = s.replace('\u{2014}', "-");     // em dash —
    s = s.replace('\u{2013}', "-");     // en dash –

    // Replace non-breaking spaces with regular spaces
    s = s.replace('\u{00a0}', " ");     // NBSP
    s = s.replace('\u{feff}', "");      // BOM / zero-width no-break space

    // Remove trailing commas before } or ] (common LLM mistake)
    // This is a simple regex-free approach
    let bytes = s.as_bytes().to_vec();
    let mut cleaned = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b',' {
            // Look ahead past whitespace for } or ]
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\n' || bytes[j] == b'\r' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                // Skip the trailing comma
                i += 1;
                continue;
            }
        }
        cleaned.push(bytes[i]);
        i += 1;
    }

    String::from_utf8(cleaned).unwrap_or(s)
}
fn repair_truncated_json(s: &str) -> String {
    let start = match s.find('{') {
        Some(i) => i,
        None => return s.to_string(),
    };
    let json_part = &s[start..];
    let mut result = json_part.to_string();

    // Remove trailing incomplete string (odd number of quotes)
    let quote_count = result.chars().filter(|&c| c == '"').count();
    if quote_count % 2 != 0 {
        if let Some(last_quote) = result.rfind('"') {
            if let Some(last_comma) = result[..last_quote].rfind(',') {
                result = result[..last_comma].to_string();
            } else if let Some(last_brace) = result[..last_quote].rfind('{') {
                result = result[..=last_brace].to_string();
            }
        }
    }

    // Remove trailing comma
    let trimmed = result.trim_end();
    if trimmed.ends_with(',') {
        result = trimmed[..trimmed.len()-1].to_string();
    }

    // Count and close open braces/brackets
    let mut open_braces = 0i32;
    let mut open_brackets = 0i32;
    let mut in_string = false;
    let mut prev_char = ' ';
    for c in result.chars() {
        if c == '"' && prev_char != '\\' { in_string = !in_string; }
        if !in_string {
            match c {
                '{' => open_braces += 1,
                '}' => open_braces -= 1,
                '[' => open_brackets += 1,
                ']' => open_brackets -= 1,
                _ => {}
            }
        }
        prev_char = c;
    }
    for _ in 0..open_brackets { result.push(']'); }
    for _ in 0..open_braces { result.push('}'); }
    result
}

fn extract_partial_actions(s: &str) -> Option<Vec<AgentAction>> {
    let actions_start = s.find("\"actions\"")
        .and_then(|i| s[i..].find('[').map(|j| i + j))?;
    let rest = &s[actions_start..];
    let mut actions: Vec<AgentAction> = Vec::new();
    let mut depth = 0;
    let mut obj_start: Option<usize> = None;
    let mut in_string = false;
    let mut prev = ' ';

    for (i, c) in rest.char_indices() {
        if c == '"' && prev != '\\' { in_string = !in_string; }
        if !in_string {
            if c == '{' {
                if depth == 1 { obj_start = Some(i); }
                depth += 1;
            } else if c == '}' {
                depth -= 1;
                if depth == 1 {
                    if let Some(start) = obj_start {
                        let obj_str = &rest[start..=i];
                        if let Ok(action) = serde_json::from_str::<AgentAction>(obj_str) {
                            actions.push(action);
                        }
                        obj_start = None;
                    }
                }
            } else if c == ']' && depth == 1 {
                break;
            }
        }
        prev = c;
    }
    if actions.is_empty() { None } else { Some(actions) }
}

fn extract_json(text: &str) -> Option<String> {
    if text.starts_with('{') {
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