use crate::action::ActionExecutor;
use crate::perception::{AndroidMessage, Perception};
use crate::session::SessionManager;
use crate::soul::Workspace;
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

#[derive(Clone)]
pub struct AppState {
    pub perception: Arc<Perception>,
    pub executor: Arc<ActionExecutor>,
    pub workspace: Arc<Workspace>,
    pub sessions: Arc<SessionManager>,
    pub running: Arc<Mutex<bool>>,
    pub event_tx: broadcast::Sender<String>,
}

#[derive(Serialize)]
struct R<T: Serialize> { ok: bool, data: Option<T>, error: Option<String> }
impl<T: Serialize> R<T> {
    fn ok(d: T) -> Json<R<T>> { Json(R { ok: true, data: Some(d), error: None }) }
}
fn err(m: &str) -> Json<R<()>> { Json(R { ok: false, data: None, error: Some(m.into()) }) }

pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Agent control
        .route("/status", get(status))
        .route("/start", post(start))
        .route("/stop", post(stop))
        // Workspace files (OpenClaw-style)
        .route("/workspace/{filename}", get(read_workspace_file))
        .route("/workspace/{filename}", post(write_workspace_file))
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
        .layer(CorsLayer::permissive())
        .with_state(state)
}

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

async fn read_workspace_file(State(s): State<AppState>, Path(f): Path<String>) -> impl IntoResponse {
    R::ok(s.workspace.read_file(&f))
}

#[derive(Deserialize)]
struct WriteBody { content: String }

async fn write_workspace_file(State(s): State<AppState>, Path(f): Path<String>, Json(b): Json<WriteBody>) -> impl IntoResponse {
    match s.workspace.write_file(&f, &b.content) {
        Ok(()) => R::ok("written"),
        Err(e) => { err(&e.to_string()); R::ok("error") }
    }
}

async fn read_memory(State(s): State<AppState>) -> impl IntoResponse { R::ok(s.workspace.read_file("MEMORY.md")) }

async fn read_daily_memory(State(s): State<AppState>) -> impl IntoResponse {
    R::ok(s.workspace.get_recent_daily_memory(7))
}

#[derive(Deserialize)]
struct MemoryBody { section: String, entry: String }

async fn write_memory(State(s): State<AppState>, Json(b): Json<MemoryBody>) -> impl IntoResponse {
    s.workspace.append_long_term_memory(&b.section, &b.entry).ok();
    R::ok("written")
}

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
    R::ok("completed")
}

async fn list_sessions(State(s): State<AppState>) -> impl IntoResponse {
    R::ok(s.sessions.list_sessions().await)
}

async fn get_session(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    R::ok(s.sessions.get_session(&id).await)
}

async fn reset_session(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    s.sessions.reset_session(&id).await;
    R::ok("reset")
}

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

#[derive(Deserialize)]
struct ChatBody { message: String }

async fn chat(State(s): State<AppState>, Json(b): Json<ChatBody>) -> impl IntoResponse {
    // Handle slash commands (OpenClaw style)
    let msg = b.message.trim();
    if msg.starts_with('/') {
        let result = handle_slash_command(msg, &s).await;
        return R::ok(result);
    }

    // Regular message → inject as user command for next tick
    s.perception.push_user_command(msg.to_string()).await;
    let _ = s.event_tx.send(serde_json::json!({"type":"user_command","text":msg}).to_string());
    R::ok("queued")
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
        "/stop" => {
            *s.running.lock().await = false;
            "Agent stopped.".into()
        }
        "/start" => {
            *s.running.lock().await = true;
            "Agent started.".into()
        }
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
        _ => format!("Unknown command: {}", parts[0]),
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
