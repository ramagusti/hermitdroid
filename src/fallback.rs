use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

// ── Configuration ───────────────────────────────────────────────────────────

/// A single model provider configuration (used for both primary and fallbacks)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub backend: String,
    pub model: String,
    pub endpoint: String,
    #[serde(default)]
    pub api_key: String,
    /// Optional: whether this model supports vision
    #[serde(default)]
    pub vision_enabled: bool,
}

/// Fallback chain configuration — add to [brain] section
///
/// ```toml
/// [brain]
/// # ... primary model config ...
/// fallback_on_rate_limit = true
/// fallback_on_auth_error = true
/// fallback_on_timeout = true
/// fallback_cooldown_secs = 60
///
/// [[brain.fallbacks]]
/// backend = "groq"
/// model = "llama-3.3-70b-versatile"
/// endpoint = "https://api.groq.com/openai/v1"
/// api_key = "gsk_..."
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackConfig {
    /// Whether to fallback on HTTP 429 (rate limit)
    #[serde(default = "default_true")]
    pub fallback_on_rate_limit: bool,

    /// Whether to fallback on HTTP 401/403 (auth error)
    #[serde(default = "default_true")]
    pub fallback_on_auth_error: bool,

    /// Whether to fallback on timeout
    #[serde(default = "default_true")]
    pub fallback_on_timeout: bool,

    /// How long to avoid a failed provider before retrying (seconds)
    #[serde(default = "default_cooldown")]
    pub fallback_cooldown_secs: u64,

    /// The fallback model chain (tried in order)
    #[serde(default)]
    pub fallbacks: Vec<ModelConfig>,
}

fn default_true() -> bool { true }
fn default_cooldown() -> u64 { 60 }

impl Default for FallbackConfig {
    fn default() -> Self {
        Self {
            fallback_on_rate_limit: true,
            fallback_on_auth_error: true,
            fallback_on_timeout: true,
            fallback_cooldown_secs: 60,
            fallbacks: Vec::new(),
        }
    }
}

// ── Error classification (OpenClaw-style) ───────────────────────────────────

/// Classifies an LLM API error to determine if fallback should trigger.
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorClass {
    /// HTTP 429 — rate limited
    RateLimit,
    /// HTTP 401/403 — auth failure (bad key, expired, no quota)
    AuthError,
    /// Request timed out
    Timeout,
    /// HTTP 5xx — server error (transient)
    ServerError,
    /// HTTP 400 / model not found / invalid request — don't fallback
    ClientError,
    /// Network unreachable, DNS failure, etc.
    NetworkError,
    /// Unknown / unclassifiable
    Unknown,
}

impl ErrorClass {
    /// Classify an error string (from reqwest or similar) into a category.
    /// This is intentionally fuzzy — different providers format errors differently.
    pub fn classify(error: &str) -> Self {
        let lower = error.to_lowercase();

        // Rate limit patterns
        if lower.contains("429")
            || lower.contains("rate limit")
            || lower.contains("rate_limit")
            || lower.contains("too many requests")
            || lower.contains("quota exceeded")
            || lower.contains("tokens per minute")
            || lower.contains("requests per minute")
        {
            return Self::RateLimit;
        }

        // Auth patterns
        if lower.contains("401")
            || lower.contains("403")
            || lower.contains("unauthorized")
            || lower.contains("forbidden")
            || lower.contains("invalid api key")
            || lower.contains("invalid_api_key")
            || lower.contains("authentication")
            || lower.contains("billing")
            || lower.contains("insufficient_quota")
        {
            return Self::AuthError;
        }

        // Timeout patterns
        if lower.contains("timeout")
            || lower.contains("timed out")
            || lower.contains("deadline exceeded")
            || lower.contains("request took too long")
        {
            return Self::Timeout;
        }

        // Server error patterns
        if lower.contains("500")
            || lower.contains("502")
            || lower.contains("503")
            || lower.contains("504")
            || lower.contains("internal server error")
            || lower.contains("bad gateway")
            || lower.contains("service unavailable")
            || lower.contains("overloaded")
        {
            return Self::ServerError;
        }

        // Client error patterns (don't fallback — would fail on all providers)
        if lower.contains("400")
            || lower.contains("invalid request")
            || lower.contains("model not found")
            || lower.contains("context_length_exceeded")
            || lower.contains("max.*token")
        {
            return Self::ClientError;
        }

        // Network
        if lower.contains("connection refused")
            || lower.contains("dns")
            || lower.contains("network")
            || lower.contains("unreachable")
        {
            return Self::NetworkError;
        }

        Self::Unknown
    }

    /// Whether this error class should trigger a model fallback
    pub fn should_fallback(&self, config: &FallbackConfig) -> bool {
        match self {
            Self::RateLimit => config.fallback_on_rate_limit,
            Self::AuthError => config.fallback_on_auth_error,
            Self::Timeout => config.fallback_on_timeout,
            Self::ServerError => true,  // always try fallback on 5xx
            Self::NetworkError => true,  // always try fallback on network errors
            Self::ClientError => false,  // never fallback (would fail everywhere)
            Self::Unknown => false,      // don't fallback on unknown errors
        }
    }
}

// ── Fallback manager ────────────────────────────────────────────────────────

/// Tracks provider health and manages the fallback chain at runtime.
#[derive(Debug, Clone)]
pub struct FallbackManager {
    config: FallbackConfig,
    primary: ModelConfig,

    /// Cooldown tracking: when each provider was last marked as failed
    /// Key = "{backend}/{model}", Value = when the cooldown started
    cooldowns: Vec<(String, Instant)>,

    /// Which model index we're currently using (-1 = primary, 0+ = fallback index)
    current_index: i32,

    /// Total fallback attempts this session (for logging)
    total_fallbacks: u32,
}

impl FallbackManager {
    pub fn new(primary: ModelConfig, config: FallbackConfig) -> Self {
        Self {
            config,
            primary,
            cooldowns: Vec::new(),
            current_index: -1,
            total_fallbacks: 0,
        }
    }

    /// Get the currently active model config.
    /// Returns primary if healthy, or the current fallback.
    pub fn active_model(&self) -> &ModelConfig {
        if self.current_index < 0 {
            &self.primary
        } else {
            self.config
                .fallbacks
                .get(self.current_index as usize)
                .unwrap_or(&self.primary)
        }
    }

    /// Report a successful request — model is healthy.
    /// If we were on a fallback, stay there until primary's cooldown expires.
    pub fn report_success(&mut self) {
        // Success on current model — it's working fine
        debug_log(format!(
            "Model success: {}/{}",
            self.active_model().backend,
            self.active_model().model
        ));
    }

    /// Report a failed request. Returns the next model to try, or None if exhausted.
    pub fn report_failure(&mut self, error: &str) -> Option<ModelConfig> {
        let error_class = ErrorClass::classify(error);

        if !error_class.should_fallback(&self.config) {
            warn!(
                "Error class {:?} — not eligible for fallback",
                error_class
            );
            return None;
        }

        // Put current model on cooldown
        let current = self.active_model().clone();
        let key = format!("{}/{}", current.backend, current.model);
        info!(
            "Model {}/{} failed ({:?}) — cooling down for {}s",
            current.backend, current.model, error_class, self.config.fallback_cooldown_secs
        );
        self.cooldowns.push((key, Instant::now()));

        // Try next model in chain
        self.advance_to_next()
    }

    /// Check if the primary model's cooldown has expired and switch back.
    /// Call this periodically (e.g., every heartbeat tick).
    pub fn check_primary_recovery(&mut self) {
        if self.current_index < 0 {
            return; // Already on primary
        }

        let primary_key = format!("{}/{}", self.primary.backend, self.primary.model);
        let cooldown = Duration::from_secs(self.config.fallback_cooldown_secs);

        let primary_ready = self
            .cooldowns
            .iter()
            .find(|(k, _)| k == &primary_key)
            .map(|(_, when)| when.elapsed() >= cooldown)
            .unwrap_or(true);

        if primary_ready {
            info!(
                "Primary model {}/{} cooldown expired — switching back",
                self.primary.backend, self.primary.model
            );
            self.current_index = -1;
            self.cooldowns.retain(|(k, _)| k != &primary_key);
        }
    }

    /// Get a status summary for logging/display
    pub fn status_summary(&self) -> String {
        let active = self.active_model();
        let is_primary = self.current_index < 0;
        let fallback_count = self.config.fallbacks.len();

        if is_primary {
            format!(
                "{}/{} (primary, {} fallback(s) configured)",
                active.backend, active.model, fallback_count
            )
        } else {
            format!(
                "{}/{} (fallback #{}, {} total attempts)",
                active.backend, active.model, self.current_index + 1, self.total_fallbacks
            )
        }
    }

    /// Check if we have any fallbacks configured
    pub fn has_fallbacks(&self) -> bool {
        !self.config.fallbacks.is_empty()
    }

    // ── Internal ────────────────────────────────────────────────────────

    fn advance_to_next(&mut self) -> Option<ModelConfig> {
        let cooldown = Duration::from_secs(self.config.fallback_cooldown_secs);

        // Try each fallback in order, skipping ones on cooldown
        let start = if self.current_index < 0 { 0 } else { (self.current_index + 1) as usize };

        for i in start..self.config.fallbacks.len() {
            let candidate = &self.config.fallbacks[i];
            let key = format!("{}/{}", candidate.backend, candidate.model);

            let on_cooldown = self
                .cooldowns
                .iter()
                .find(|(k, _)| k == &key)
                .map(|(_, when)| when.elapsed() < cooldown)
                .unwrap_or(false);

            if !on_cooldown {
                self.current_index = i as i32;
                self.total_fallbacks += 1;
                info!(
                    "Falling back to: {}/{} (fallback #{})",
                    candidate.backend, candidate.model, i + 1
                );
                return Some(candidate.clone());
            } else {
                debug_log(format!("{} still on cooldown, skipping", key));
            }
        }

        // Also try wrapping back to primary if its cooldown expired
        let primary_key = format!("{}/{}", self.primary.backend, self.primary.model);
        let primary_ready = self
            .cooldowns
            .iter()
            .find(|(k, _)| k == &primary_key)
            .map(|(_, when)| when.elapsed() >= cooldown)
            .unwrap_or(true);

        if primary_ready && self.current_index >= 0 {
            self.current_index = -1;
            info!("All fallbacks exhausted or cooling down — retrying primary");
            return Some(self.primary.clone());
        }

        // Everything is on cooldown
        error!("All models exhausted (primary + {} fallbacks)", self.config.fallbacks.len());
        None
    }
}

fn debug_log(msg: String) {
    tracing::debug!("{}", msg);
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_primary() -> ModelConfig {
        ModelConfig {
            backend: "openai".to_string(),
            model: "gpt-4o".to_string(),
            endpoint: "https://api.openai.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            vision_enabled: true,
        }
    }

    fn test_fallbacks() -> Vec<ModelConfig> {
        vec![
            ModelConfig {
                backend: "groq".to_string(),
                model: "llama-3.3-70b-versatile".to_string(),
                endpoint: "https://api.groq.com/openai/v1".to_string(),
                api_key: "gsk-test".to_string(),
                vision_enabled: false,
            },
            ModelConfig {
                backend: "ollama".to_string(),
                model: "llama3.2".to_string(),
                endpoint: "http://localhost:11434/v1".to_string(),
                api_key: String::new(),
                vision_enabled: false,
            },
        ]
    }

    #[test]
    fn test_error_classification() {
        assert_eq!(ErrorClass::classify("HTTP 429 Too Many Requests"), ErrorClass::RateLimit);
        assert_eq!(ErrorClass::classify("rate_limit_exceeded"), ErrorClass::RateLimit);
        assert_eq!(ErrorClass::classify("HTTP 401 Unauthorized"), ErrorClass::AuthError);
        assert_eq!(ErrorClass::classify("invalid api key"), ErrorClass::AuthError);
        assert_eq!(ErrorClass::classify("request timed out after 30s"), ErrorClass::Timeout);
        assert_eq!(ErrorClass::classify("HTTP 500 Internal Server Error"), ErrorClass::ServerError);
        assert_eq!(ErrorClass::classify("HTTP 400 model not found"), ErrorClass::ClientError);
        assert_eq!(ErrorClass::classify("something weird happened"), ErrorClass::Unknown);
    }

    #[test]
    fn test_fallback_chain() {
        let config = FallbackConfig {
            fallbacks: test_fallbacks(),
            fallback_cooldown_secs: 1, // short for testing
            ..Default::default()
        };
        let mut mgr = FallbackManager::new(test_primary(), config);

        // Start on primary
        assert_eq!(mgr.active_model().backend, "openai");

        // Primary fails with rate limit → should get groq
        let next = mgr.report_failure("HTTP 429 rate limit");
        assert!(next.is_some());
        assert_eq!(next.unwrap().backend, "groq");
        assert_eq!(mgr.active_model().backend, "groq");

        // Groq fails → should get ollama
        let next = mgr.report_failure("HTTP 429 too many requests");
        assert!(next.is_some());
        assert_eq!(next.unwrap().backend, "ollama");
    }

    #[test]
    fn test_no_fallback_on_client_error() {
        let config = FallbackConfig {
            fallbacks: test_fallbacks(),
            ..Default::default()
        };
        let mut mgr = FallbackManager::new(test_primary(), config);

        // Client error should NOT trigger fallback
        let next = mgr.report_failure("HTTP 400 model not found");
        assert!(next.is_none());
        assert_eq!(mgr.active_model().backend, "openai"); // Still on primary
    }
}