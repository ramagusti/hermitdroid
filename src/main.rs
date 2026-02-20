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
#[command(name = "hermitdroid", version, about = "Autonomous Android AI agent")]
struct Cli {
    #[arg(short, long, default_value = "config.toml")]
    config: String,
    #[arg(long, help = "Log actions but don't execute")]
    dry_run: bool,
    #[command(subcommand)]
    command: Option<SubCommand>,
}

#[derive(Parser)]
enum SubCommand {
    /// Start the agent (default if no subcommand given)
    Gateway,
    /// Send a command to a running agent
    Chat {
        /// The message or command to send
        message: Vec<String>,
    },
    /// Show agent status
    Status,
    /// Run the bootstrap ritual interactively
    Onboard,
    /// Check workspace and config health
    Doctor,
    /// Install/uninstall as a background service (systemd)
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Show real-time agent logs
    Logs,
    /// Stop a running background agent
    Stop,
    /// Restart the background agent
    Restart,
}

#[derive(Parser)]
enum ServiceAction {
    /// Install systemd service for current user
    Install,
    /// Remove systemd service
    Uninstall,
    /// Show service status
    Status,
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

    // Commands that don't need full config
    match &cli.command {
        Some(SubCommand::Service { action }) => return handle_service(action),
        Some(SubCommand::Logs) => return run_logs(),
        _ => {}
    }

    let config = Config::load(Path::new(&cli.config))?;

    match cli.command {
        Some(SubCommand::Status) => {
            // Try to reach running instance first
            let url = format!("http://127.0.0.1:{}/status", config.server.port);
            match reqwest::Client::new().get(&url).timeout(std::time::Duration::from_secs(2)).send().await {
                Ok(resp) => {
                    let data: serde_json::Value = resp.json().await?;
                    let running = data["data"]["running"].as_bool().unwrap_or(false);
                    let app = data["data"]["current_app"].as_str().unwrap_or("unknown");
                    let pending = data["data"]["pending_confirmations"].as_u64().unwrap_or(0);
                    println!("🤖 Hermitdroid v{}", env!("CARGO_PKG_VERSION"));
                    println!("   Status:  {}", if running { "🟢 Running" } else { "🔴 Paused" });
                    println!("   Model:   {} via {}", config.brain.model, config.brain.backend);
                    println!("   App:     {}", app);
                    if pending > 0 {
                        println!("   Pending: {} action(s) awaiting confirmation", pending);
                    }
                    println!("   Dashboard: http://localhost:{}", config.server.port);
                }
                Err(_) => {
                    println!("🤖 Hermitdroid v{}", env!("CARGO_PKG_VERSION"));
                    println!("   Status:  ⚫ Not running");
                    println!("   Model:   {} via {}", config.brain.model, config.brain.backend);
                    println!("   Start:   hermitdroid  or  systemctl --user start hermitdroid");
                }
            }
            return Ok(());
        }
        Some(SubCommand::Doctor) => {
            return run_doctor(&config);
        }
        Some(SubCommand::Chat { message }) => {
            let msg = message.join(" ");
            if msg.is_empty() {
                println!("Usage: hermitdroid chat <message>");
                return Ok(());
            }
            let url = format!("http://127.0.0.1:{}/chat", config.server.port);
            match reqwest::Client::new().post(&url)
                .json(&serde_json::json!({"message": msg}))
                .timeout(std::time::Duration::from_secs(5))
                .send().await
            {
                Ok(resp) => {
                    let data: serde_json::Value = resp.json().await?;
                    if let Some(d) = data["data"].as_str() {
                        println!("{}", d);
                    } else {
                        println!("✅ Queued: {}", msg);
                    }
                }
                Err(_) => {
                    println!("❌ Agent not running. Start it first with: hermitdroid");
                }
            }
            return Ok(());
        }
        Some(SubCommand::Stop) => {
            let url = format!("http://127.0.0.1:{}/stop", config.server.port);
            match reqwest::Client::new().post(&url)
                .timeout(std::time::Duration::from_secs(2))
                .send().await
            {
                Ok(_) => println!("⏸ Agent paused."),
                Err(_) => println!("❌ Agent not running."),
            }
            return Ok(());
        }
        Some(SubCommand::Restart) => {
            // Stop then start
            let stop_url = format!("http://127.0.0.1:{}/stop", config.server.port);
            let start_url = format!("http://127.0.0.1:{}/start", config.server.port);
            let client = reqwest::Client::new();
            let _ = client.post(&stop_url).timeout(std::time::Duration::from_secs(2)).send().await;
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            match client.post(&start_url).timeout(std::time::Duration::from_secs(2)).send().await {
                Ok(_) => println!("🔄 Agent restarted."),
                Err(_) => println!("❌ Agent not running."),
            }
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
    info!("🌐 Dashboard: http://localhost:{}", config.server.port);

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

    if workspace.needs_bootstrap() {
        info!("📋 BOOTSTRAP.md detected — first-run ritual active");
    }

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

        if last_gateway_heartbeat.elapsed().as_secs() >= gateway_heartbeat {
            info!("🔄 Gateway heartbeat (memory flush)");
            workspace.append_daily_memory("--- gateway heartbeat ---").ok();
            last_gateway_heartbeat = std::time::Instant::now();
        }

        let result = heartbeat_tick(
            &workspace, &brain, &perception, &executor, &sessions, &event_tx, tick_count,
            &config.perception.bridge_mode,
        ).await;

        if let Err(e) = result {
            error!("Tick error: {}", e);
            workspace.append_daily_memory(&format!("ERROR: {}", e)).ok();
        }

        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(heartbeat_interval)) => {}
            event = event_rx.recv() => {
                if let Ok(ev) = event {
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
///
/// KEY DESIGN: For multi-step UI interactions, we execute actions in batches
/// and re-poll the screen between batches. This lets the agent see updated
/// coordinates after each UI transition (e.g., after tapping search, after
/// opening a chat). Without this, the agent guesses coordinates blindly.
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
        let has_priority = perception.poll_notifications_adb().await;
        if has_priority {
            info!("⚡ Priority notification detected");
        }
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

    // 3. Call LLM
    let screenshot = screen.as_ref().and_then(|s| s.screenshot_base64.as_deref());
    let raw = brain.think(&system_prompt, &user_prompt, screenshot).await?;

    // 4. Parse
    let response = brain.parse_response(&raw);

    // 5. HEARTBEAT_OK
    if response.reflection.as_deref() == Some("HEARTBEAT_OK") {
        tracing::debug!("Tick {}: HEARTBEAT_OK", tick);
        return Ok(());
    }

    // 6. Reflection
    if let Some(ref r) = response.reflection {
        if !r.is_empty() && r != "HEARTBEAT_OK" {
            info!("💭 {}", r);
        }
    }

    // 7. Memory write
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

    // 9. Execute actions with screen re-polling
    if response.actions.is_empty() {
        tracing::debug!("Tick {}: no actions", tick);
    } else {
        info!("Tick {}: {} action(s)", tick, response.actions.len());

        // Track actions that significantly change the UI and need a re-poll
        let ui_changing_actions = ["tap", "launch_app", "long_press", "swipe", "back", "home"];

        let mut ui_changed_count = 0;

        for (i, action) in response.actions.iter().enumerate() {
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

                    // After a UI-changing action, re-poll screen so subsequent
                    // actions in a NEW tick can use fresh coordinates.
                    // We count how many UI-changing actions we've done.
                    if ui_changing_actions.contains(&action.action_type.as_str()) {
                        ui_changed_count += 1;
                    }

                    // After every 2-3 UI-changing actions, if there are more
                    // actions remaining, break out and let the next tick re-poll
                    // and re-plan with fresh screen data.
                    if ui_changed_count >= 3 && i + 1 < response.actions.len() {
                        let remaining = response.actions.len() - i - 1;
                        info!("  ⏸ Pausing after {} UI actions — {} remaining, will re-poll screen", ui_changed_count, remaining);

                        // Re-poll screen immediately
                        if bridge_mode == "adb" {
                            perception.poll_screen_adb().await;
                        }

                        // Re-inject remaining actions as a continuation command
                        // so the next tick picks them up with fresh screen data
                        let remaining_descriptions: Vec<String> = response.actions[i+1..]
                            .iter()
                            .map(|a| format!("{}: {}", a.action_type, a.reason))
                            .collect();
                        let continuation = format!(
                            "[CONTINUE] Previous actions paused for screen refresh. Remaining steps: {}",
                            remaining_descriptions.join("; ")
                        );
                        perception.push_user_command(continuation).await;
                        break;
                    }
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

// ================================================================
// Service management (systemd --user)
// ================================================================

fn handle_service(action: &ServiceAction) -> anyhow::Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let service_dir = format!("{}/.config/systemd/user", home);
    let service_file = format!("{}/hermitdroid.service", service_dir);
    let binary = format!("{}/.local/bin/hermitdroid", home);
    let work_dir = format!("{}/.hermitdroid", home);

    match action {
        ServiceAction::Install => {
            // Create systemd user dir
            std::fs::create_dir_all(&service_dir)?;

            // Detect ADB path
            let adb_path = std::process::Command::new("which")
                .arg("adb")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

            let adb_dir = if !adb_path.is_empty() {
                Path::new(&adb_path).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
            } else {
                String::new()
            };

            // Build PATH that includes adb, cargo, and standard paths
            let extra_path = format!("{}/.cargo/bin:{}/.local/bin:{}", home, home,
                if adb_dir.is_empty() { "/usr/bin".to_string() } else { format!("{}:/usr/bin:/usr/local/bin", adb_dir) }
            );

            let unit = format!(r#"[Unit]
Description=Hermitdroid — Autonomous Android AI Agent
After=network.target

[Service]
Type=simple
WorkingDirectory={work_dir}
ExecStart={binary} --config {work_dir}/config.toml
Restart=on-failure
RestartSec=5
Environment="PATH={extra_path}"
Environment="HOME={home}"
# Keep ADB server accessible
Environment="ANDROID_HOME={home}/Android/Sdk"

[Install]
WantedBy=default.target
"#);

            std::fs::write(&service_file, &unit)?;

            // Enable and reload
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status();
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "enable", "hermitdroid"])
                .status();

            // Enable lingering so service runs even when user is not logged in
            let user = std::env::var("USER").unwrap_or_else(|_| "root".into());
            let _ = std::process::Command::new("loginctl")
                .args(["enable-linger", &user])
                .status();

            println!("✅ Service installed: {}", service_file);
            println!();
            println!("Commands:");
            println!("  Start:   systemctl --user start hermitdroid");
            println!("  Stop:    systemctl --user stop hermitdroid");
            println!("  Status:  systemctl --user status hermitdroid");
            println!("  Logs:    journalctl --user -u hermitdroid -f");
            println!("  Restart: systemctl --user restart hermitdroid");
            println!();
            println!("Or use: hermitdroid stop / hermitdroid restart / hermitdroid logs");
        }
        ServiceAction::Uninstall => {
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "stop", "hermitdroid"])
                .status();
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "disable", "hermitdroid"])
                .status();

            if Path::new(&service_file).exists() {
                std::fs::remove_file(&service_file)?;
                let _ = std::process::Command::new("systemctl")
                    .args(["--user", "daemon-reload"])
                    .status();
                println!("✅ Service removed.");
            } else {
                println!("⚠️  Service file not found.");
            }
        }
        ServiceAction::Status => {
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "status", "hermitdroid"])
                .status();
        }
    }

    Ok(())
}

fn run_logs() -> anyhow::Result<()> {
    let _ = std::process::Command::new("journalctl")
        .args(["--user", "-u", "hermitdroid", "-f", "--no-pager"])
        .status();
    Ok(())
}

fn run_doctor(config: &Config) -> anyhow::Result<()> {
    println!("🩺 Hermitdroid Doctor\n");

    let ws_path = Path::new(&config.agent.workspace_path);
    if ws_path.exists() {
        println!("✅ Workspace: {}", config.agent.workspace_path);
    } else {
        println!("❌ Workspace missing: {}", config.agent.workspace_path);
    }

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

    if ws_path.join("BOOTSTRAP.md").exists() {
        println!("\n⚠️  BOOTSTRAP.md exists — first-run ritual not yet completed");
    }

    let skills_dir = ws_path.join("skills");
    if skills_dir.exists() {
        let count = std::fs::read_dir(&skills_dir)
            .map(|d| d.filter(|e| e.as_ref().map(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).unwrap_or(false)).count())
            .unwrap_or(0);
        println!("\n✅ Skills: {} installed", count);
    } else {
        println!("\n⚠️  No skills directory");
    }

    // ADB check
    println!();
    match std::process::Command::new("adb").args(["devices"]).output() {
        Ok(out) => {
            let devices = String::from_utf8_lossy(&out.stdout);
            let connected = devices.lines().filter(|l| l.contains("\tdevice")).count();
            if connected > 0 {
                println!("✅ ADB: {} device(s) connected", connected);
            } else {
                println!("❌ ADB: no devices connected");
            }
        }
        Err(_) => println!("❌ ADB: not found in PATH"),
    }

    // Server reachability
    let port = config.server.port;
    println!();
    match std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        std::time::Duration::from_secs(1),
    ) {
        Ok(_) => println!("✅ Server: listening on port {}", port),
        Err(_) => println!("⚫ Server: not running on port {}", port),
    }

    println!("\n🧠 Brain: {} via {}", config.brain.model, config.brain.backend);
    println!("   Endpoint: {}", config.brain.endpoint);
    println!("   Vision: {}", config.brain.vision_enabled);

    if !config.action.restricted_apps.is_empty() {
        println!("\n🔒 Restricted: {:?}", config.action.restricted_apps);
    }

    if config.action.dry_run {
        println!("\n⚠️  Dry run mode enabled");
    }

    println!("\n✨ Doctor complete.");
    Ok(())
}