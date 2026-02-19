mod action;
mod brain;
mod config;
mod perception;
mod server;
mod session;
mod soul;

use crate::action::ActionExecutor;
use crate::brain::Brain;
use crate::config::Config;
use crate::perception::Perception;
use crate::server::{build_router, AppState};
use crate::session::SessionManager;
use crate::soul::Workspace;
use clap::Parser;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "hermitdroid", about = "OpenClaw-inspired autonomous Android AI agent")]
struct Cli {
    #[arg(short, long, default_value = "config.toml")]
    config: String,
    #[arg(long, help = "Log actions but don't execute")]
    dry_run: bool,
    /// Subcommands (OpenClaw-style CLI)
    #[command(subcommand)]
    command: Option<SubCommand>,
}

#[derive(Parser)]
enum SubCommand {
    /// Start the gateway + heartbeat (default)
    Gateway,
    /// Send a message to the agent
    Chat {
        #[arg(short, long)]
        message: String,
    },
    /// Show agent status
    Status,
    /// Run the bootstrap ritual interactively
    Onboard,
    /// Check workspace health
    Doctor,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hermitdroid=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = Config::load(Path::new(&cli.config))?;

    match cli.command {
        Some(SubCommand::Status) => {
            println!("Agent: {}", config.agent.name);
            println!("Model: {} via {}", config.brain.model, config.brain.backend);
            println!("Workspace: {}", config.agent.workspace_path);
            return Ok(());
        }
        Some(SubCommand::Doctor) => {
            return run_doctor(&config);
        }
        Some(SubCommand::Chat { message }) => {
            // Send a message to a running instance via HTTP
            let url = format!("http://{}:{}/chat", config.server.host, config.server.port);
            let client = reqwest::Client::new();
            let resp = client.post(&url)
                .json(&serde_json::json!({"message": message}))
                .send().await?;
            println!("{}", resp.text().await?);
            return Ok(());
        }
        _ => {} // Gateway (default) or Onboard
    }

    // ---- Initialize all components ----
    info!("🤖 Hermitdroid v{}", env!("CARGO_PKG_VERSION"));
    info!("Agent: {} | Model: {} | Backend: {}", config.agent.name, config.brain.model, config.brain.backend);

    let workspace = Arc::new(Workspace::new(&config.agent.workspace_path, config.agent.bootstrap_max_chars));
    let brain = Arc::new(Brain::new(&config.brain));
    let perception = Arc::new(Perception::new(
        config.perception.adb_device.clone(),
        config.perception.priority_apps.clone(),
    ));
    let dry_run = cli.dry_run || config.action.dry_run;
    let executor = Arc::new(ActionExecutor::new(
        dry_run,
        config.perception.adb_device.clone(),
        config.action.restricted_apps.clone(),
    ));
    let sessions = Arc::new(SessionManager::new());
    let running = Arc::new(Mutex::new(true));
    let (event_tx, _) = broadcast::channel::<String>(256);

    if dry_run { warn!("⚠️  DRY RUN mode — actions logged but not executed"); }

    // ---- Bridge mode info ----
    info!("📡 Bridge mode: {}", config.perception.bridge_mode);
    if config.perception.bridge_mode == "adb" {
        // Quick ADB check
        match std::process::Command::new("adb").args(["devices"]).output() {
            Ok(out) => {
                let devices = String::from_utf8_lossy(&out.stdout);
                let connected = devices.lines().filter(|l| l.contains("\tdevice")).count();
                if connected > 0 {
                    info!("✅ ADB: {} device(s) connected", connected);
                } else {
                    warn!("⚠️  ADB: no devices found. Run `adb devices` to check.");
                }
            }
            Err(_) => warn!("⚠️  ADB binary not found. Install Android SDK platform-tools."),
        }
    }

    // Ensure main session exists
    sessions.main_session().await;

    // ---- Start HTTP/WS server ----
    let state = AppState {
        perception: perception.clone(),
        executor: executor.clone(),
        workspace: workspace.clone(),
        sessions: sessions.clone(),
        running: running.clone(),
        event_tx: event_tx.clone(),
    };

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("🌐 Server: http://{}", addr);
    info!("📱 Android WS: ws://{}/ws/android", addr);
    info!("👤 User WS: ws://{}/ws/user", addr);

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            error!("Server error: {}", e);
        }
    });

    // ---- Run on_boot hook ----
    if let Some(boot_file) = &config.hooks.on_boot {
        info!("Running on_boot hook: {}", boot_file);
        let content = workspace.read_file(boot_file);
        if !content.is_empty() {
            perception.push_user_command(format!("[BOOT] {}", content)).await;
        }
    }

    // ---- Bootstrap detection ----
    if workspace.needs_bootstrap() {
        info!("📋 BOOTSTRAP.md detected — first-run ritual active");
        info!("   Send a message to the agent to start the bootstrap.");
    }

    // ---- Log soul on startup ----
    let soul_preview: String = workspace.read_file("SOUL.md").chars().take(200).collect();
    if !soul_preview.is_empty() {
        info!("Soul loaded: {}...", soul_preview.trim());
    }
    workspace.append_daily_memory("Agent started").ok();

    // ---- HEARTBEAT LOOP ----
    let heartbeat_interval = config.agent.heartbeat_interval_secs;
    let gateway_heartbeat = config.agent.gateway_heartbeat_interval_secs;
    info!("💓 Heartbeat: {}s tick, {}s gateway", heartbeat_interval, gateway_heartbeat);

    let mut event_rx = event_tx.subscribe();
    let mut last_gateway_heartbeat = std::time::Instant::now();
    let mut tick_count: u64 = 0;

    loop {
        if !*running.lock().await {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            continue;
        }

        tick_count += 1;

        // ---- Gateway heartbeat (deeper check, less frequent) ----
        if last_gateway_heartbeat.elapsed().as_secs() >= gateway_heartbeat {
            info!("🔄 Gateway heartbeat (memory flush)");
            // Auto-flush: this is where daily memory gets curated
            workspace.append_daily_memory("--- gateway heartbeat ---").ok();
            last_gateway_heartbeat = std::time::Instant::now();
        }

        // ---- Main heartbeat tick ----
        let result = heartbeat_tick(
            &workspace, &brain, &perception, &executor, &sessions, &event_tx, tick_count,
            &config.perception.bridge_mode,
        ).await;

        if let Err(e) = result {
            error!("Tick error: {}", e);
            workspace.append_daily_memory(&format!("ERROR: {}", e)).ok();
        }

        // ---- Sleep, but wake on events ----
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(heartbeat_interval)) => {}
            event = event_rx.recv() => {
                if let Ok(ev) = event {
                    // Priority notification or user command → immediate tick
                    if ev.contains("priority_notification") || ev.contains("user_command") {
                        info!("⚡ Event interrupt — immediate tick");
                    }
                    if ev.contains("stop everything") || ev.contains("\"event\":\"kill\"") {
                        *running.lock().await = false;
                        warn!("🛑 KILL SWITCH activated");
                        workspace.append_daily_memory("KILL SWITCH activated").ok();
                    }
                }
            }
        }
    }
}

/// Single heartbeat tick — the core agent loop
async fn heartbeat_tick(
    workspace: &Workspace,
    brain: &Brain,
    perception: &Perception,
    executor: &ActionExecutor,
    sessions: &SessionManager,
    event_tx: &broadcast::Sender<String>,
    tick: u64,
    bridge_mode: &str,
) -> anyhow::Result<()> {
    // 0. ADB polling — pull fresh data from the device
    if bridge_mode == "adb" {
        // Poll notifications via `dumpsys notification --noredact`
        let has_priority = perception.poll_notifications_adb().await;
        if has_priority {
            info!("⚡ Priority notification detected");
        }
        // Poll screen state (foreground app + UI tree)
        perception.poll_screen_adb().await;
    }

    // 1. Gather context
    let ctx = workspace.assemble_bootstrap();
    let notifications = perception.drain_notifications().await;
    let screen = perception.get_screen_state().await;
    let commands = perception.drain_user_commands().await;
    let events = perception.drain_device_events().await;
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();

    let notif_text = Perception::format_notifications(&notifications);
    let screen_text = Perception::format_screen(&screen);

    // If nothing is happening and no commands, skip LLM call (save tokens)
    if notifications.is_empty() && commands.is_empty() && events.is_empty() && tick % 4 != 0 {
        tracing::debug!("Tick {}: idle (skipping LLM)", tick);
        return Ok(());
    }

    // 2. Build prompts
    let system_prompt = brain.build_system_prompt(&ctx);
    let user_prompt = brain.build_tick_prompt(&ctx, &notif_text, &screen_text, &commands, &now);

    // 3. Call LLM (with vision if screenshot available)
    let screenshot = screen.as_ref().and_then(|s| s.screenshot_base64.as_deref());
    let raw = brain.think(&system_prompt, &user_prompt, screenshot).await?;

    // 4. Parse
    let response = brain.parse_response(&raw);

    // 5. HEARTBEAT_OK → silent drop (OpenClaw pattern)
    if response.reflection.as_deref() == Some("HEARTBEAT_OK") {
        tracing::debug!("Tick {}: HEARTBEAT_OK", tick);
        return Ok(());
    }

    // 6. Process reflection
    if let Some(ref r) = response.reflection {
        if !r.is_empty() && r != "HEARTBEAT_OK" {
            info!("💭 {}", r);
        }
    }

    // 7. Memory write (agent self-directed memory)
    if let Some(ref mem) = response.memory_write {
        workspace.append_daily_memory(mem).ok();
        info!("🧠 Memory: {}", mem);
    }

    // 8. Message to user
    if let Some(ref msg) = response.message {
        let _ = event_tx.send(serde_json::json!({
            "type": "agent_message", "message": msg
        }).to_string());
        info!("💬 → User: {}", msg);
    }

    // 9. Execute actions
    if response.actions.is_empty() {
        tracing::debug!("Tick {}: no actions", tick);
    } else {
        info!("Tick {}: {} action(s)", tick, response.actions.len());
        for action in &response.actions {
            match executor.execute(action).await {
                Ok(result) => {
                    info!("  ✅ {} → {}", action.action_type, result);
                    workspace.append_daily_memory(&format!(
                        "Action: {} ({}) → {}", action.action_type, action.reason, result
                    )).ok();
                    let _ = event_tx.send(serde_json::json!({
                        "type": "action",
                        "action": action.action_type,
                        "classification": action.classification,
                        "result": result,
                    }).to_string());
                }
                Err(e) => {
                    error!("  ❌ {} → {}", action.action_type, e);
                    workspace.append_daily_memory(&format!(
                        "FAILED: {} → {}", action.action_type, e
                    )).ok();
                }
            }
        }
    }

    // 10. Track in session
    if !commands.is_empty() || !response.actions.is_empty() {
        for cmd in &commands {
            sessions.append_message("main", "user", cmd).await;
        }
        if let Some(ref msg) = response.message {
            sessions.append_message("main", "assistant", msg).await;
        }
    }

    Ok(())
}

fn run_doctor(config: &Config) -> anyhow::Result<()> {
    println!("🩺 Hermitdroid Doctor\n");

    // Check workspace
    let ws_path = Path::new(&config.agent.workspace_path);
    if ws_path.exists() {
        println!("✅ Workspace: {}", config.agent.workspace_path);
    } else {
        println!("❌ Workspace missing: {}", config.agent.workspace_path);
    }

    // Check required files
    for file in &["SOUL.md", "AGENTS.md", "TOOLS.md", "IDENTITY.md", "USER.md", "HEARTBEAT.md", "MEMORY.md", "GOALS.md"] {
        let p = ws_path.join(file);
        if p.exists() {
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            let status = if size > 10 { "✅" } else { "⚠️  (empty)" };
            println!("  {} {}: {} bytes", status, file, size);
        } else {
            println!("  ❌ {} missing", file);
        }
    }

    // Check bootstrap
    if ws_path.join("BOOTSTRAP.md").exists() {
        println!("\n⚠️  BOOTSTRAP.md exists — first-run ritual not yet completed");
    }

    // Check skills
    let skills_dir = ws_path.join("skills");
    if skills_dir.exists() {
        let count = std::fs::read_dir(&skills_dir)
            .map(|d| d.filter(|e| e.as_ref().map(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).unwrap_or(false)).count())
            .unwrap_or(0);
        println!("\n✅ Skills: {} installed", count);
    } else {
        println!("\n⚠️  No skills directory");
    }

    // Check model config
    println!("\n🧠 Brain: {} via {}", config.brain.model, config.brain.backend);
    println!("   Endpoint: {}", config.brain.endpoint);
    println!("   Vision: {}", config.brain.vision_enabled);

    // Check restricted apps
    if !config.action.restricted_apps.is_empty() {
        println!("\n🔒 Restricted apps: {:?}", config.action.restricted_apps);
    }

    if config.action.dry_run {
        println!("\n⚠️  Dry run mode enabled in config");
    }

    println!("\n✨ Doctor complete.");
    Ok(())
}
