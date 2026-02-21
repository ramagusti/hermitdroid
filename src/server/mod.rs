use crate::action::ActionExecutor;
use crate::perception::{AndroidMessage, Perception};
use crate::session::SessionManager;
use crate::soul::Workspace;
use crate::tailscale::TailscaleManager;
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Path, State},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tower_http::cors::CorsLayer;
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub perception: Arc<Perception>,
    pub executor: Arc<ActionExecutor>,
    pub workspace: Arc<Workspace>,
    pub sessions: Arc<SessionManager>,
    pub running: Arc<Mutex<bool>>,
    pub event_tx: broadcast::Sender<String>,
    pub tailscale: Arc<Mutex<TailscaleManager>>,
}

#[derive(Serialize)]
struct R { ok: bool, data: Option<serde_json::Value>, error: Option<String> }
impl R {
    fn ok<T: Serialize>(d: T) -> Json<R> { Json(R { ok: true, data: Some(serde_json::to_value(d).unwrap_or_default()), error: None }) }
    fn err(m: &str) -> Json<R> { Json(R { ok: false, data: None, error: Some(m.into()) }) }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Dashboard (root)
        .route("/", get(dashboard))
        // Agent control
        .route("/status", get(status))
        .route("/start", post(start))
        .route("/stop", post(stop))
        // Config (settings UI)
        .route("/config", get(get_config))
        .route("/config", post(set_config))
        // Updates
        .route("/update/check", get(check_update))
        .route("/update/install", post(install_update))
        // Workspace files (OpenClaw-style)
        .route("/workspace/*filename", get(read_workspace_file))
        .route("/workspace/*filename", post(write_workspace_file))
        // Memory
        .route("/memory", get(read_memory))
        .route("/memory/daily", get(read_daily_memory))
        .route("/memory", post(write_memory))
        // Goals
        .route("/goals", get(read_goals))
        .route("/goals", post(add_goal))
        .route("/goals/{id}/complete", post(complete_goal))
        // Sessions
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/new", post(reset_session))
        // Actions
        .route("/pending", get(pending_actions))
        .route("/confirm/{id}", post(confirm_action))
        .route("/actions/log", get(action_log))
        // Chat (slash commands like OpenClaw)
        .route("/chat", post(chat))
        // WebSocket
        .route("/ws/android", get(ws_android))
        .route("/ws/user", get(ws_user))
        .route("/tailscale/status", get(tailscale_status))
        .route("/tailscale/connect", post(tailscale_connect))
        .route("/tailscale/disconnect", post(tailscale_disconnect))
        .route("/tailscale/peers", get(tailscale_peers))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ---- Dashboard ----

async fn dashboard() -> Html<&'static str> {
    Html(include_str!("dashboard.html"))
}

// ---- Status ----

async fn status(State(s): State<AppState>) -> impl IntoResponse {
    let running = *s.running.lock().await;
    let pending = s.executor.pending().lock().await.len();
    let screen = s.perception.get_screen_state().await;
    R::ok(serde_json::json!({
        "running": running,
        "pending_confirmations": pending,
        "current_app": screen.as_ref().map(|s| &s.current_app),
    }))
}

async fn start(State(s): State<AppState>) -> impl IntoResponse { *s.running.lock().await = true; R::ok("started") }
async fn stop(State(s): State<AppState>) -> impl IntoResponse { *s.running.lock().await = false; R::ok("stopped") }

// ---- Config API (read/write config.toml via dashboard) ----

async fn get_config() -> impl IntoResponse {
    // Read config.toml and return as JSON
    let config_path = find_config_path();
    match std::fs::read_to_string(&config_path) {
        Ok(content) => {
            match content.parse::<toml::Table>() {
                Ok(table) => R::ok(serde_json::to_value(table).unwrap_or_default()),
                Err(e) => R::err(&format!("Config parse error: {}", e)),
            }
        }
        Err(e) => R::err(&format!("Could not read config: {}", e)),
    }
}

#[derive(Deserialize)]
struct ConfigUpdate {
    brain: Option<BrainUpdate>,
    agent: Option<AgentUpdate>,
    action: Option<ActionUpdate>,
    perception: Option<PerceptionUpdate>,
}

#[derive(Deserialize)]
struct BrainUpdate {
    backend: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    vision_enabled: Option<bool>,
}

#[derive(Deserialize)]
struct AgentUpdate {
    heartbeat_interval_secs: Option<u64>,
}

#[derive(Deserialize)]
struct ActionUpdate {
    dry_run: Option<bool>,
}

#[derive(Deserialize)]
struct PerceptionUpdate {
    priority_apps: Option<Vec<String>>,
}

async fn set_config(Json(update): Json<ConfigUpdate>) -> impl IntoResponse {
    let config_path = find_config_path();

    // Read existing config
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) => return R::err(&format!("Could not read config: {}", e)),
    };

    let mut table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(e) => return R::err(&format!("Config parse error: {}", e)),
    };

    // Apply updates
    if let Some(brain) = update.brain {
        let section = table.entry("brain").or_insert(toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut t) = section {
            if let Some(v) = brain.backend { t.insert("backend".into(), toml::Value::String(v)); }
            if let Some(v) = brain.model { t.insert("model".into(), toml::Value::String(v)); }
            if let Some(v) = brain.api_key {
                if !v.is_empty() {
                    t.insert("api_key".into(), toml::Value::String(v));
                }
            }
            if let Some(v) = brain.vision_enabled { t.insert("vision_enabled".into(), toml::Value::Boolean(v)); }
        }
    }

    if let Some(agent) = update.agent {
        let section = table.entry("agent").or_insert(toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut t) = section {
            if let Some(v) = agent.heartbeat_interval_secs { t.insert("heartbeat_interval_secs".into(), toml::Value::Integer(v as i64)); }
        }
    }

    if let Some(action) = update.action {
        let section = table.entry("action").or_insert(toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut t) = section {
            if let Some(v) = action.dry_run { t.insert("dry_run".into(), toml::Value::Boolean(v)); }
        }
    }

    if let Some(perception) = update.perception {
        let section = table.entry("perception").or_insert(toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(ref mut t) = section {
            if let Some(apps) = perception.priority_apps {
                let arr: Vec<toml::Value> = apps.into_iter().map(toml::Value::String).collect();
                t.insert("priority_apps".into(), toml::Value::Array(arr));
            }
        }
    }

    // Write back
    let new_content = toml::to_string_pretty(&table).unwrap_or_default();
    match std::fs::write(&config_path, &new_content) {
        Ok(()) => {
            info!("Config updated via dashboard");
            R::ok(serde_json::json!("saved"))
        }
        Err(e) => R::err(&format!("Could not write config: {}", e)),
    }
}

/// Find config.toml — check current dir, then ~/.hermitdroid/
fn find_config_path() -> String {
    if std::path::Path::new("config.toml").exists() {
        "config.toml".into()
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        format!("{}/.hermitdroid/config.toml", home)
    }
}

// ---- Update API ----

async fn check_update() -> impl IntoResponse {
    // Git fetch and check if behind
    let fetch = std::process::Command::new("git")
        .args(["fetch", "origin", "--quiet"])
        .output();

    if fetch.is_err() {
        return R::err("git not available");
    }

    // Check if behind
    let status = std::process::Command::new("git")
        .args(["rev-list", "HEAD..origin/main", "--count"])
        .output();

    let commits_behind = status.ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);

    // Get latest commit message
    let latest = std::process::Command::new("git")
        .args(["log", "origin/main", "-1", "--pretty=format:%s"])
        .output();

    let latest_message = latest.ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    R::ok(serde_json::json!({
        "up_to_date": commits_behind == 0,
        "commits_behind": commits_behind,
        "latest_message": latest_message,
    }))
}

async fn install_update() -> impl IntoResponse {
    // git pull
    let pull = std::process::Command::new("git")
        .args(["pull", "--ff-only"])
        .output();

    match pull {
        Ok(out) if out.status.success() => {
            // Rebuild
            let build = std::process::Command::new("cargo")
                .args(["build", "--release"])
                .output();

            match build {
                Ok(out) if out.status.success() => {
                    info!("Update installed via dashboard");
                    R::ok(serde_json::json!("updated"))
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    R::err(&format!("Build failed: {}", stderr))
                }
                Err(e) => R::err(&format!("Build error: {}", e)),
            }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            R::err(&format!("git pull failed: {}", stderr))
        }
        Err(e) => R::err(&format!("git error: {}", e)),
    }
}

// ---- Workspace ----

async fn read_workspace_file(State(s): State<AppState>, Path(f): Path<String>) -> impl IntoResponse {
    let filename = f.trim_start_matches('/');
    R::ok(s.workspace.read_file(filename))
}

#[derive(Deserialize)]
struct WriteBody { content: String }

async fn write_workspace_file(State(s): State<AppState>, Path(f): Path<String>, Json(b): Json<WriteBody>) -> impl IntoResponse {
    let filename = f.trim_start_matches('/');
    match s.workspace.write_file(filename, &b.content) {
        Ok(()) => R::ok("written".to_string()),
        Err(e) => R::err(&e.to_string()),
    }
}

// ---- Memory ----

async fn read_memory(State(s): State<AppState>) -> impl IntoResponse { R::ok(s.workspace.read_file("MEMORY.md")) }

async fn read_daily_memory(State(s): State<AppState>) -> impl IntoResponse {
    R::ok(s.workspace.get_recent_daily_memory(7))
}

#[derive(Deserialize)]
struct MemoryBody { section: String, entry: String }

async fn write_memory(State(s): State<AppState>, Json(b): Json<MemoryBody>) -> impl IntoResponse {
    s.workspace.append_long_term_memory(&b.section, &b.entry).ok();
    R::ok("written".to_string())
}

// ---- Goals ----

async fn read_goals(State(s): State<AppState>) -> impl IntoResponse { R::ok(s.workspace.read_file("GOALS.md")) }

#[derive(Deserialize)]
struct GoalBody { description: String, due: Option<String> }

async fn add_goal(State(s): State<AppState>, Json(b): Json<GoalBody>) -> impl IntoResponse {
    match s.workspace.add_goal(&b.description, b.due.as_deref()) {
        Ok(id) => R::ok(serde_json::json!({"id": id})),
        Err(e) => R::ok(serde_json::json!({"error": e.to_string()})),
    }
}

async fn complete_goal(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    s.workspace.complete_goal(&id).ok();
    R::ok("completed".to_string())
}

// ---- Sessions ----

async fn list_sessions(State(s): State<AppState>) -> impl IntoResponse {
    R::ok(s.sessions.list_sessions().await)
}

async fn get_session(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    R::ok(s.sessions.get_session(&id).await)
}

async fn reset_session(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    s.sessions.reset_session(&id).await;
    R::ok("reset".to_string())
}

// ---- Actions ----

async fn pending_actions(State(s): State<AppState>) -> impl IntoResponse {
    R::ok(s.executor.pending().lock().await.clone())
}

#[derive(Deserialize)]
struct ConfirmBody { approved: bool }

async fn confirm_action(State(s): State<AppState>, Path(id): Path<String>, Json(b): Json<ConfirmBody>) -> impl IntoResponse {
    match s.executor.confirm(&id, b.approved).await {
        Ok(r) => R::ok(r),
        Err(e) => R::ok(e.to_string()),
    }
}

async fn action_log(State(s): State<AppState>) -> impl IntoResponse {
    R::ok(s.executor.action_log().lock().await.clone())
}

// ---- Chat ----

#[derive(Deserialize)]
struct ChatBody { message: String }

async fn chat(State(s): State<AppState>, Json(b): Json<ChatBody>) -> impl IntoResponse {
    let msg = b.message.trim();
    if msg.starts_with('/') {
        let result = handle_slash_command(msg, &s).await;
        return R::ok(result);
    }

    s.perception.push_user_command(msg.to_string()).await;
    let _ = s.event_tx.send(serde_json::json!({"type":"user_command","text":msg}).to_string());
    R::ok("queued".to_string())
}

async fn handle_slash_command(cmd: &str, s: &AppState) -> String {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    match parts[0] {
        "/status" => {
            let running = *s.running.lock().await;
            let pending = s.executor.pending().lock().await.len();
            format!("Running: {} | Pending confirmations: {}", running, pending)
        }
        "/new" | "/reset" => {
            s.sessions.reset_session("main").await;
            "Session reset.".into()
        }
        "/stop" => { *s.running.lock().await = false; "Agent stopped.".into() }
        "/start" => { *s.running.lock().await = true; "Agent started.".into() }
        "/goal" => {
            if parts.len() > 1 {
                match s.workspace.add_goal(parts[1], None) {
                    Ok(id) => format!("Goal added (id: {})", id),
                    Err(e) => format!("Error: {}", e),
                }
            } else {
                "Usage: /goal <description>".into()
            }
        }
        "/memory" => {
            let mem = s.workspace.read_file("MEMORY.md");
            if mem.is_empty() { "No memory yet.".into() } else { mem }
        }
        "/goals" => s.workspace.read_file("GOALS.md"),
        "/soul" => s.workspace.read_file("SOUL.md"),
        "/help" => {
            "/status — agent status\n/start — start agent\n/stop — stop agent\n/new — reset session\n/goal <text> — add goal\n/goals — list goals\n/memory — show memory\n/soul — show personality\n/help — this message".into()
        }
        _ => format!("Unknown command: {}. Type /help for available commands.", parts[0]),
    }
}

// ---- WebSocket handlers ----

async fn ws_android(ws: WebSocketUpgrade, State(s): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_android(socket, s))
}

async fn handle_android(mut socket: WebSocket, state: AppState) {
    info!("Android companion connected");
    let outgoing = state.executor.outgoing();

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(am) = serde_json::from_str::<AndroidMessage>(&text) {
                            match am {
                                AndroidMessage::Notification(n) => {
                                    let is_priority = state.perception.push_notification(n).await;
                                    if is_priority {
                                        let _ = state.event_tx.send(r#"{"type":"priority_notification"}"#.into());
                                    }
                                }
                                AndroidMessage::ScreenState(s) => { state.perception.update_screen(s).await; }
                                AndroidMessage::UserCommand { text } => {
                                    state.perception.push_user_command(text.clone()).await;
                                    let _ = state.event_tx.send(serde_json::json!({"type":"user_command","text":text}).to_string());
                                }
                                AndroidMessage::DeviceEvent { event } => {
                                    state.perception.push_device_event(event.clone()).await;
                                    let _ = state.event_tx.send(serde_json::json!({"type":"device_event","event":event}).to_string());
                                }
                                AndroidMessage::ActionResult { action_id, success, message } => {
                                    info!("Action result [{}]: {} — {}", action_id, success, message);
                                }
                                AndroidMessage::Heartbeat => {}
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => { info!("Android disconnected"); break; }
                    _ => {}
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                let mut actions = outgoing.lock().await;
                for a in actions.drain(..) {
                    let json = serde_json::to_string(&a).unwrap_or_default();
                    if socket.send(Message::Text(json.into())).await.is_err() { break; }
                }
            }
        }
    }
}

async fn ws_user(ws: WebSocketUpgrade, State(s): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_user(socket, s))
}

async fn handle_user(mut socket: WebSocket, state: AppState) {
    info!("User dashboard connected");
    let mut rx = state.event_tx.subscribe();
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(text) => { if socket.send(Message::Text(text.into())).await.is_err() { break; } }
                    Err(_) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let t = text.to_string();
                        state.perception.push_user_command(t.clone()).await;
                        let _ = state.event_tx.send(serde_json::json!({"type":"user_command","text":t}).to_string());
                    }
                    Some(Ok(Message::Close(_))) | None => { info!("User disconnected"); break; }
                    _ => {}
                }
            }
        }
    }
}

// ---- Tailscale handlers ----

async fn tailscale_status(State(state): State<AppState>) -> Json<Value> {
    let ts = state.tailscale.lock().await;
    Json(json!({"ok": true, "data": ts.api_status()}))
}

async fn tailscale_connect(State(state): State<AppState>) -> Json<Value> {
    let mut ts = state.tailscale.lock().await;
    match ts.connect() {
        Ok(addr) => Json(json!({"ok": true, "data": {"address": addr}})),
        Err(e) => Json(json!({"ok": false, "error": e})),
    }
}

async fn tailscale_disconnect(State(state): State<AppState>) -> Json<Value> {
    let mut ts = state.tailscale.lock().await;
    ts.disconnect();
    Json(json!({"ok": true}))
}

async fn tailscale_peers(State(state): State<AppState>) -> Json<Value> {
    let peers = TailscaleManager::list_peers(true);
    Json(json!({"ok": true, "data": peers}))
}