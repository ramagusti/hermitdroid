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
    /// If true, RED actions execute immediately (user opted in via SOUL.md boundaries)
    auto_confirm_red: bool,
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
            auto_confirm_red: true, // Default: auto-confirm per SOUL.md boundary rules
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
                // Check if this involves a restricted app → always queue
                if let Some(pkg) = action.params.get("package").and_then(|v| v.as_str()) {
                    if self.restricted_apps.iter().any(|a| pkg.contains(a)) {
                        self.pending.lock().await.push(PendingConfirmation {
                            action_id: id.clone(),
                            action: action.clone(),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            confirmed: None,
                        });
                        info!("[RED-RESTRICTED] Queued for confirmation: {} ({})", action.action_type, id);
                        return Ok(format!("PENDING:{}", id));
                    }
                }

                // Auto-confirm if enabled (SOUL.md says "send messages without confirmation")
                if self.auto_confirm_red {
                    info!("[RED-AUTO] {}: {}", action.action_type, action.reason);
                    if self.dry_run {
                        return self.log_dry_run(action, &classification).await;
                    }
                    let result = self.do_action(action, &id).await?;
                    self.log_action(action, "RED-AUTO", &result).await;
                    return Ok(result);
                }

                // Otherwise queue for manual confirmation
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

    /// Wait for the screen to settle after an action.
    /// Polls the foreground activity — if it changes, the UI transitioned.
    /// Returns early if transition detected, otherwise waits max_ms.
    async fn wait_for_settle(&self, max_ms: u64) {
        // Get current foreground app
        let before = self.adb(&["shell", "dumpsys", "activity", "activities"])
            .ok()
            .and_then(|raw| {
                raw.lines()
                    .find(|l| l.contains("mResumedActivity:") || l.contains("topResumedActivity:"))
                    .map(|l| l.to_string())
            })
            .unwrap_or_default();

        // Poll in 50ms intervals until screen changes or timeout
        let start = std::time::Instant::now();
        let interval = 50;
        let checks = (max_ms / interval).max(1);

        for _ in 0..checks {
            tokio::time::sleep(tokio::time::Duration::from_millis(interval)).await;

            let after = self.adb(&["shell", "dumpsys", "activity", "activities"])
                .ok()
                .and_then(|raw| {
                    raw.lines()
                        .find(|l| l.contains("mResumedActivity:") || l.contains("topResumedActivity:"))
                        .map(|l| l.to_string())
                })
                .unwrap_or_default();

            if after != before {
                // Activity changed — UI transitioned, done waiting
                tracing::debug!("Screen settled in {}ms (activity changed)", start.elapsed().as_millis());
                return;
            }
        }
        tracing::debug!("Screen settle timeout after {}ms", start.elapsed().as_millis());
    }

    /// Route action to the correct executor
    async fn do_action(&self, action: &AgentAction, id: &str) -> anyhow::Result<String> {
        let p = &action.params;
        match action.action_type.as_str() {
            // --- Screen interactions ---
            "tap" => {
                let result = self.adb(&["shell", "input", "tap",
                    &p["x"].as_f64().unwrap_or(0.0).to_string(),
                    &p["y"].as_f64().unwrap_or(0.0).to_string()]);
                // Reactive settle: wait until screen changes or 200ms max
                self.wait_for_settle(200).await;
                result
            }

            "long_press" => {
                let x = p["x"].as_f64().unwrap_or(0.0);
                let y = p["y"].as_f64().unwrap_or(0.0);
                let ms = p["ms"].as_u64().unwrap_or(1000);
                // Long press = swipe from same point to same point with duration
                self.adb(&["shell", "input", "swipe",
                    &x.to_string(), &y.to_string(),
                    &x.to_string(), &y.to_string(),
                    &ms.to_string()])
            }

            "swipe" => self.adb(&["shell", "input", "swipe",
                &p["x1"].as_f64().unwrap_or(0.0).to_string(),
                &p["y1"].as_f64().unwrap_or(0.0).to_string(),
                &p["x2"].as_f64().unwrap_or(0.0).to_string(),
                &p["y2"].as_f64().unwrap_or(0.0).to_string(),
                &p.get("ms").or(p.get("duration_ms"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(300).to_string()]),

            // --- Text input ---
            "type_text" => {
                // Brief settle to ensure field is focused
                self.wait_for_settle(150).await;
                let text = p["text"].as_str().unwrap_or("");
                if text.is_empty() {
                    return Ok("type_text: empty text, skipped".into());
                }

                // Try ADB input text first (works for simple alphanumeric)
                let escaped = text
                    .replace('\\', "\\\\")
                    .replace(' ', "%s")
                    .replace('&', "\\&")
                    .replace('<', "\\<")
                    .replace('>', "\\>")
                    .replace('|', "\\|")
                    .replace(';', "\\;")
                    .replace('(', "\\(")
                    .replace(')', "\\)")
                    .replace('\'', "\\'")
                    .replace('"', "\\\"")
                    .replace('$', "\\$")
                    .replace('`', "\\`");

                match self.adb(&["shell", "input", "text", &escaped]) {
                    Ok(result) => Ok(result),
                    Err(_) => {
                        // Fallback: use ADB broadcast to type via clipboard
                        warn!("input text failed, trying broadcast fallback for: {}", text);
                        // Set clipboard and paste
                        let _ = self.adb(&["shell", "input", "keyevent", "KEYCODE_MOVE_HOME"]);
                        // Use am broadcast with the text
                        self.adb(&["shell", "am", "broadcast", "-a",
                            "ADB_INPUT_TEXT", "--es", "msg", text])
                    }
                }
            }

            // --- Key events ---
            "press_key" => {
                let key = p["key"].as_str().unwrap_or("KEYCODE_HOME");
                self.adb(&["shell", "input", "keyevent", key])
            }

            // --- App management ---
            "launch_app" => {
                let pkg = p["package"].as_str().unwrap_or("");
                let result = self.adb(&["shell", "monkey", "-p", pkg, "-c", "android.intent.category.LAUNCHER", "1"]);
                // Reactive settle: wait for app to load (up to 800ms)
                self.wait_for_settle(800).await;
                result
            }

            // --- Navigation (accept both naming conventions) ---
            "home" | "go_home" =>
                self.adb(&["shell", "input", "keyevent", "KEYCODE_HOME"]),

            "back" | "go_back" =>
                self.adb(&["shell", "input", "keyevent", "KEYCODE_BACK"]),

            "recents" =>
                self.adb(&["shell", "input", "keyevent", "KEYCODE_APP_SWITCH"]),

            "open_notifications" =>
                self.adb(&["shell", "cmd", "statusbar", "expand-notifications"]),

            "scroll_down" =>
                self.adb(&["shell", "input", "swipe", "540", "1500", "540", "500", "300"]),

            "scroll_up" =>
                self.adb(&["shell", "input", "swipe", "540", "500", "540", "1500", "300"]),

            // --- Timing ---
            "wait" => {
                let ms = p["ms"].as_u64().unwrap_or(1000);
                tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
                Ok(format!("waited {}ms", ms))
            }

            // --- Screenshot ---
            "screenshot" => {
                self.adb(&["shell", "screencap", "-p", "/sdcard/hermitdroid_screenshot.png"])?;
                self.adb(&["pull", "/sdcard/hermitdroid_screenshot.png", "/tmp/hermitdroid_screenshot.png"])
            }

            // --- Notifications to user (accept both "text" and "message" params) ---
            "notify_user" => {
                let msg = p.get("text").or(p.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
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
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();

        if out.status.success() {
            if !stdout.is_empty() {
                Ok(stdout)
            } else {
                Ok("ok".into())
            }
        } else {
            // Log stderr but still return stdout if we got some output
            if !stdout.is_empty() {
                warn!("adb warning: {}", stderr);
                Ok(stdout)
            } else {
                anyhow::bail!("adb error: {}", if stderr.is_empty() { "unknown error".into() } else { stderr })
            }
        }
    }
}