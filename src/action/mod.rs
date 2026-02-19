use crate::brain::AgentAction;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingConfirmation {
    pub action_id: String,
    pub action: AgentAction,
    pub timestamp: String,
    pub confirmed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceAction {
    pub id: String,
    pub action_type: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ActionExecutor {
    dry_run: bool,
    adb_device: Option<String>,
    restricted_apps: Vec<String>,
    pending: Arc<Mutex<Vec<PendingConfirmation>>>,
    outgoing: Arc<Mutex<Vec<DeviceAction>>>,
    action_log: Arc<Mutex<Vec<ActionLogEntry>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionLogEntry {
    pub timestamp: String,
    pub action_type: String,
    pub classification: String,
    pub result: String,
}

impl ActionExecutor {
    pub fn new(dry_run: bool, adb_device: Option<String>, restricted_apps: Vec<String>) -> Self {
        Self {
            dry_run,
            adb_device,
            restricted_apps,
            pending: Arc::new(Mutex::new(Vec::new())),
            outgoing: Arc::new(Mutex::new(Vec::new())),
            action_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn pending(&self) -> Arc<Mutex<Vec<PendingConfirmation>>> { self.pending.clone() }
    pub fn outgoing(&self) -> Arc<Mutex<Vec<DeviceAction>>> { self.outgoing.clone() }
    pub fn action_log(&self) -> Arc<Mutex<Vec<ActionLogEntry>>> { self.action_log.clone() }

    /// Execute an action with guardrail enforcement
    pub async fn execute(&self, action: &AgentAction) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let classification = self.effective_classification(action);

        match classification.as_str() {
            "RED" => {
                self.pending.lock().await.push(PendingConfirmation {
                    action_id: id.clone(),
                    action: action.clone(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    confirmed: None,
                });
                info!("[RED] Queued for confirmation: {} ({})", action.action_type, id);
                Ok(format!("PENDING:{}", id))
            }
            "YELLOW" => {
                info!("[YELLOW] {}: {}", action.action_type, action.reason);
                if self.dry_run {
                    return self.log_dry_run(action, &classification).await;
                }
                let result = self.do_action(action, &id).await?;
                self.log_action(action, &classification, &result).await;
                Ok(result)
            }
            "GREEN" => {
                if self.dry_run {
                    return self.log_dry_run(action, &classification).await;
                }
                let result = self.do_action(action, &id).await?;
                self.log_action(action, &classification, &result).await;
                Ok(result)
            }
            _ => {
                warn!("Unknown classification '{}', treating as RED", classification);
                Ok("BLOCKED".into())
            }
        }
    }

    /// Confirm a pending RED action
    pub async fn confirm(&self, action_id: &str, approved: bool) -> anyhow::Result<String> {
        let mut pending = self.pending.lock().await;
        if let Some(p) = pending.iter_mut().find(|p| p.action_id == action_id) {
            p.confirmed = Some(approved);
            if approved {
                let action = p.action.clone();
                drop(pending);
                let id = action_id.to_string();
                let result = self.do_action(&action, &id).await?;
                self.log_action(&action, "RED-CONFIRMED", &result).await;
                Ok(result)
            } else {
                Ok("DENIED".into())
            }
        } else {
            anyhow::bail!("No pending action: {}", action_id)
        }
    }

    /// Determine effective classification (may upgrade to RED based on restricted apps)
    fn effective_classification(&self, action: &AgentAction) -> String {
        let base = action.classification.to_uppercase();
        // Force RED for restricted apps
        if let Some(pkg) = action.params.get("package").and_then(|v| v.as_str()) {
            if self.restricted_apps.iter().any(|a| pkg.contains(a)) {
                return "RED".into();
            }
        }
        base
    }

    async fn log_dry_run(&self, action: &AgentAction, class: &str) -> anyhow::Result<String> {
        let msg = format!("[DRY_RUN] {} ({})", action.action_type, class);
        info!("{}", msg);
        self.log_action(action, class, "DRY_RUN").await;
        Ok(msg)
    }

    async fn log_action(&self, action: &AgentAction, class: &str, result: &str) {
        self.action_log.lock().await.push(ActionLogEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            action_type: action.action_type.clone(),
            classification: class.to_string(),
            result: result.to_string(),
        });
    }

    /// Route action to the correct executor
    async fn do_action(&self, action: &AgentAction, id: &str) -> anyhow::Result<String> {
        let p = &action.params;
        match action.action_type.as_str() {
            "tap" => self.adb(&["shell", "input", "tap",
                &p["x"].as_f64().unwrap_or(0.0).to_string(),
                &p["y"].as_f64().unwrap_or(0.0).to_string()]),
            "swipe" => self.adb(&["shell", "input", "swipe",
                &p["x1"].as_f64().unwrap_or(0.0).to_string(),
                &p["y1"].as_f64().unwrap_or(0.0).to_string(),
                &p["x2"].as_f64().unwrap_or(0.0).to_string(),
                &p["y2"].as_f64().unwrap_or(0.0).to_string(),
                &p["duration_ms"].as_u64().unwrap_or(300).to_string()]),
            "type_text" => {
                let text = p["text"].as_str().unwrap_or("");
                let escaped = text.replace(' ', "%s");
                self.adb(&["shell", "input", "text", &escaped])
            }
            "press_key" => {
                let key = p["key"].as_str().unwrap_or("KEYCODE_HOME");
                self.adb(&["shell", "input", "keyevent", key])
            }
            "launch_app" => {
                let pkg = p["package"].as_str().unwrap_or("");
                self.adb(&["shell", "monkey", "-p", pkg, "-c", "android.intent.category.LAUNCHER", "1"])
            }
            "open_notifications" => self.adb(&["shell", "cmd", "statusbar", "expand-notifications"]),
            "go_home" => self.adb(&["shell", "input", "keyevent", "KEYCODE_HOME"]),
            "go_back" => self.adb(&["shell", "input", "keyevent", "KEYCODE_BACK"]),
            "scroll_down" => self.adb(&["shell", "input", "swipe", "540", "1500", "540", "500", "300"]),
            "scroll_up" => self.adb(&["shell", "input", "swipe", "540", "500", "540", "1500", "300"]),
            "wait" => {
                let ms = p["ms"].as_u64().unwrap_or(1000);
                tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
                Ok(format!("waited {}ms", ms))
            }
            "notify_user" => {
                let msg = p["message"].as_str().unwrap_or("");
                info!("[NOTIFY_USER] {}", msg);
                Ok(format!("notified: {}", msg))
            }
            _ => {
                // Send to companion app as generic action
                self.outgoing.lock().await.push(DeviceAction {
                    id: id.to_string(),
                    action_type: action.action_type.clone(),
                    params: action.params.clone(),
                });
                Ok(format!("sent_to_companion: {}", action.action_type))
            }
        }
    }

    fn adb(&self, args: &[&str]) -> anyhow::Result<String> {
        let mut cmd = Command::new("adb");
        if let Some(dev) = &self.adb_device {
            cmd.args(["-s", dev]);
        }
        cmd.args(args);
        let out = cmd.output()?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            anyhow::bail!("adb error: {}", String::from_utf8_lossy(&out.stderr))
        }
    }
}
