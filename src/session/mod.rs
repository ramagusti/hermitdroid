use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Session manager — tracks conversation sessions.
/// OpenClaw has main session + per-channel/group sessions.
/// For Android, we have: main (direct), and per-channel sessions.
#[derive(Debug, Clone)]
pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_active: String,
    /// Conversation history for this session
    pub messages: Vec<SessionMessage>,
    /// Session-specific config overrides
    #[serde(default)]
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub model_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String, // "user", "assistant", "system"
    pub content: String,
    pub timestamp: String,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get or create the main session
    pub async fn main_session(&self) -> Session {
        let mut sessions = self.sessions.lock().await;
        sessions.entry("main".to_string()).or_insert_with(|| {
            Session {
                id: "main".into(),
                name: "Main".into(),
                created_at: Utc::now().to_rfc3339(),
                last_active: Utc::now().to_rfc3339(),
                messages: Vec::new(),
                thinking_level: None,
                model_override: None,
            }
        }).clone()
    }

    /// Append message to a session
    pub async fn append_message(&self, session_id: &str, role: &str, content: &str) {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages.push(SessionMessage {
                role: role.into(),
                content: content.into(),
                timestamp: Utc::now().to_rfc3339(),
            });
            session.last_active = Utc::now().to_rfc3339();

            // Keep last 50 messages (context window management)
            if session.messages.len() > 50 {
                let drain_count = session.messages.len() - 50;
                session.messages.drain(..drain_count);
            }
        }
    }

    /// Reset a session (/new command)
    pub async fn reset_session(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.messages.clear();
            session.last_active = Utc::now().to_rfc3339();
        }
    }

    /// List all sessions
    pub async fn list_sessions(&self) -> Vec<Session> {
        self.sessions.lock().await.values().cloned().collect()
    }

    /// Get a session
    pub async fn get_session(&self, id: &str) -> Option<Session> {
        self.sessions.lock().await.get(id).cloned()
    }
}
