use crate::action::ActionExecutor;
use crate::config::Config;
use crate::perception::Perception;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::error;

// ── ANSI colors ────────────────────────────────────────────────────────────
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

// ── Flow schema ────────────────────────────────────────────────────────────

/// A deterministic flow — fixed sequence of actions, no LLM involved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flow {
    /// Display name of this flow.
    pub name: String,

    /// Optional: Android package name to launch first.
    #[serde(default)]
    pub app_id: Option<String>,

    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,
}

/// Individual action in a flow.
/// Each variant maps to a single ADB command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FlowAction {
    /// Simple string command: "launch_app", "back", "home", "screenshot"
    Simple(String),
    /// Map with a single key-value pair
    Keyed(serde_json::Map<String, serde_json::Value>),
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Run a deterministic flow from a YAML file path.
pub async fn run_flow(
    config: &Config,
    path: &str,
    dry_run: bool,
) -> anyhow::Result<()> {
    let (flow, actions) = load_flow(path)?;
    let total = actions.len();
    let dry_run = dry_run || config.action.dry_run;

    // Print header
    println!("\n{CYAN}{BOLD}⚡ Hermitdroid — Flow Mode (no AI){RESET}\n");
    println!("  {BOLD}Flow:{RESET} {}", flow.name);
    if let Some(ref desc) = flow.description {
        println!("  {DIM}{}{RESET}", desc);
    }
    if let Some(ref app) = flow.app_id {
        println!("  {BOLD}App:{RESET} {}", app);
    }
    println!("  {BOLD}Actions:{RESET} {}", total);
    if dry_run {
        println!("  {YELLOW}⚠  DRY RUN — actions logged but not executed{RESET}");
    }
    println!();

    let start = std::time::Instant::now();

    // Initialize executor
    let adb_device = config.perception.adb_device.clone();
    let executor = ActionExecutor::new(
        dry_run,
        adb_device.clone(),
        config.action.restricted_apps.clone(),
    );

    // Optional: launch app first
    if let Some(ref app_id) = flow.app_id {
        let action_start = std::time::Instant::now();
        let _ = executor.execute_raw(&format!("launch {}", app_id), &config.perception.adb_device).await;
        let ms = action_start.elapsed().as_millis();
        println!("  {GREEN}▸{RESET} launch {} {DIM}({}ms){RESET}", app_id, ms);
        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
    }

    // Execute each action in sequence
    for (i, action) in actions.iter().enumerate() {
        let step = i + 1;
        let action_start = std::time::Instant::now();

        let (action_desc, result) = execute_flow_action(&executor, &adb_device, action, dry_run).await;
        let ms = action_start.elapsed().as_millis();

        match result {
            Ok(msg) => {
                if action_desc == "done" {
                    println!(
                        "  [{}/{}] {GREEN}{BOLD}✅ Done{RESET} — {} {DIM}({}ms){RESET}",
                        step, total, msg, ms
                    );
                    break;
                } else {
                    println!(
                        "  [{}/{}] {GREEN}▸{RESET} {} {DIM}({}ms){RESET}",
                        step, total, action_desc, ms
                    );
                }
            }
            Err(e) => {
                println!(
                    "  [{}/{}] {YELLOW}✗{RESET} {} — {}{RESET} {DIM}({}ms){RESET}",
                    step, total, action_desc, e, ms
                );
                error!("Flow action {} failed: {}", action_desc, e);
            }
        }
    }

    let elapsed = start.elapsed();
    println!(
        "\n  {GREEN}{BOLD}⚡ Flow complete{RESET} — {:.1}s\n",
        elapsed.as_secs_f64()
    );

    Ok(())
}

/// List all available flows.
pub fn list_flows() -> Vec<(std::path::PathBuf, Flow)> {
    let mut results = Vec::new();
    let examples_dir = Path::new("examples/flows");
    if examples_dir.exists() {
        collect_flows(examples_dir, &mut results);
    }
    // Also check workspace flows
    let workspace_dir = Path::new("workspace/flows");
    if workspace_dir.exists() {
        collect_flows(workspace_dir, &mut results);
    }
    results
}

// ── Flow action execution ──────────────────────────────────────────────────

/// Execute a single flow action and return (description, result).
async fn execute_flow_action(
    executor: &ActionExecutor,
    adb_device: &Option<String>,
    action: &FlowAction,
    _dry_run: bool,
) -> (String, anyhow::Result<String>) {
    match action {
        FlowAction::Simple(cmd) => {
            let cmd = cmd.trim().to_lowercase();
            match cmd.as_str() {
                "launch_app" | "launchapp" => {
                    // Handled by flow.app_id above, but allow explicit too
                    let result = executor.execute_raw("home", adb_device).await;
                    ("launch_app (use app_id in header)".to_string(), result.map(|_| "ok".to_string()))
                }
                "back" => {
                    let result = executor.execute_raw("back", adb_device).await;
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    ("back".to_string(), result.map(|_| "ok".to_string()))
                }
                "home" => {
                    let result = executor.execute_raw("home", adb_device).await;
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    ("home".to_string(), result.map(|_| "ok".to_string()))
                }
                "screenshot" => {
                    // Use ADB screencap
                    let device_arg = adb_device.as_ref().map(|d| format!("-s {} ", d)).unwrap_or_default();
                    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                    let local_path = format!("workspace/screenshots/flow_{}.png", ts);
                    std::fs::create_dir_all("workspace/screenshots").ok();
                    let cmd = format!(
                        "{}adb {}exec-out screencap -p > {}",
                        "", device_arg, local_path
                    );
                    let output = tokio::process::Command::new("sh")
                        .arg("-c")
                        .arg(&format!("adb {}exec-out screencap -p > {}", device_arg, local_path))
                        .output()
                        .await;
                    match output {
                        Ok(_) => ("screenshot".to_string(), Ok(format!("saved to {}", local_path))),
                        Err(e) => ("screenshot".to_string(), Err(anyhow::anyhow!("{}", e))),
                    }
                }
                other => {
                    ("unknown".to_string(), Err(anyhow::anyhow!("Unknown simple action: {}", other)))
                }
            }
        }
        FlowAction::Keyed(map) => {
            if let Some((key, value)) = map.iter().next() {
                let key = key.trim().to_lowercase();
                match key.as_str() {
                    "wait" => {
                        let secs = value.as_f64().unwrap_or(1.0);
                        tokio::time::sleep(tokio::time::Duration::from_secs_f64(secs)).await;
                        (format!("wait {}s", secs), Ok("ok".to_string()))
                    }
                    "tap" => {
                        // tap: [x, y] — coordinate tap
                        if let Some(arr) = value.as_array() {
                            if arr.len() >= 2 {
                                let x = arr[0].as_i64().unwrap_or(0);
                                let y = arr[1].as_i64().unwrap_or(0);
                                let result = execute_adb_tap(adb_device, x as i32, y as i32).await;
                                tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
                                (format!("tap ({}, {})", x, y), result)
                            } else {
                                ("tap".to_string(), Err(anyhow::anyhow!("tap needs [x, y]")))
                            }
                        } else {
                            ("tap".to_string(), Err(anyhow::anyhow!("tap needs [x, y] array")))
                        }
                    }
                    "tap_text" | "taptext" => {
                        // tap_text: "Wi-Fi" — find and tap element by text
                        // This requires reading the accessibility tree to find coordinates
                        let text = value.as_str().unwrap_or("");
                        let perception = Perception::new(
                            adb_device.clone(),
                            vec![], // no priority apps needed for flows
                        );
                        perception.poll_screen_adb_full(false).await;
                        let screen = perception.get_screen_state().await;

                        // Search through UI elements for matching text
                        if let Some(ref state) = screen {
                            let elements = &state.elements;
                            if !elements.is_empty() {
                                for elem in elements {
                                    let elem_text = elem.text.as_str();
                                    let content_desc = elem.desc.as_str();
                                    if elem_text.contains(text) || content_desc.contains(text) {
                                        // Found it — tap the center of its bounds
                                        let bounds = &elem.bounds;
                                        let cx = (bounds[0] + bounds[2]) / 2;  // (left + right) / 2
                                        let cy = (bounds[1] + bounds[3]) / 2;  // (top + bottom) / 2
                                        let result = execute_adb_tap(adb_device, cx as i32, cy as i32).await;
                                        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
                                        return (format!("tap_text \"{}\" → ({}, {})", text, cx, cy), result);
                                    }
                                }
                            }
                        }
                        (
                            format!("tap_text \"{}\"", text),
                            Err(anyhow::anyhow!("Element with text '{}' not found on screen", text)),
                        )
                    }
                    "type" | "type_text" => {
                        let text = value.as_str().unwrap_or("");
                        let escaped = text.replace(' ', "%s").replace('\n', "%n");
                        let device_arg = adb_device.as_ref().map(|d| format!("-s {} ", d)).unwrap_or_default();
                        let output = tokio::process::Command::new("adb")
                            .args(build_adb_args(adb_device, &["shell", "input", "text", &escaped]))
                            .output()
                            .await;
                        match output {
                            Ok(o) if o.status.success() => {
                                (format!("type \"{}\"", truncate(text, 30)), Ok("ok".to_string()))
                            }
                            Ok(o) => (
                                format!("type \"{}\"", truncate(text, 30)),
                                Err(anyhow::anyhow!("{}", String::from_utf8_lossy(&o.stderr))),
                            ),
                            Err(e) => (format!("type \"{}\"", truncate(text, 30)), Err(anyhow::anyhow!("{}", e))),
                        }
                    }
                    "swipe" => {
                        // swipe: [x1, y1, x2, y2] with optional duration
                        if let Some(arr) = value.as_array() {
                            if arr.len() >= 4 {
                                let x1 = arr[0].as_i64().unwrap_or(0).to_string();
                                let y1 = arr[1].as_i64().unwrap_or(0).to_string();
                                let x2 = arr[2].as_i64().unwrap_or(0).to_string();
                                let y2 = arr[3].as_i64().unwrap_or(0).to_string();
                                let dur = if arr.len() > 4 {
                                    arr[4].as_i64().unwrap_or(300).to_string()
                                } else {
                                    "300".to_string()
                                };
                                let output = tokio::process::Command::new("adb")
                                    .args(build_adb_args(adb_device, &[
                                        "shell", "input", "swipe", &x1, &y1, &x2, &y2, &dur,
                                    ]))
                                    .output()
                                    .await;
                                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                                match output {
                                    Ok(o) if o.status.success() => (
                                        format!("swipe ({},{})→({},{})", x1, y1, x2, y2),
                                        Ok("ok".to_string()),
                                    ),
                                    Ok(o) => (
                                        "swipe".to_string(),
                                        Err(anyhow::anyhow!("{}", String::from_utf8_lossy(&o.stderr))),
                                    ),
                                    Err(e) => ("swipe".to_string(), Err(anyhow::anyhow!("{}", e))),
                                }
                            } else {
                                ("swipe".to_string(), Err(anyhow::anyhow!("swipe needs [x1,y1,x2,y2]")))
                            }
                        } else {
                            ("swipe".to_string(), Err(anyhow::anyhow!("swipe needs array")))
                        }
                    }
                    "key" | "keyevent" => {
                        let keycode = value.as_str().unwrap_or("ENTER");
                        let full_key = if keycode.starts_with("KEYCODE_") {
                            keycode.to_string()
                        } else {
                            format!("KEYCODE_{}", keycode.to_uppercase())
                        };
                        let output = tokio::process::Command::new("adb")
                            .args(build_adb_args(adb_device, &["shell", "input", "keyevent", &full_key]))
                            .output()
                            .await;
                        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                        match output {
                            Ok(o) if o.status.success() => (
                                format!("key {}", keycode),
                                Ok("ok".to_string()),
                            ),
                            Ok(o) => (
                                format!("key {}", keycode),
                                Err(anyhow::anyhow!("{}", String::from_utf8_lossy(&o.stderr))),
                            ),
                            Err(e) => (format!("key {}", keycode), Err(anyhow::anyhow!("{}", e))),
                        }
                    }
                    "done" => {
                        let msg = value.as_str().unwrap_or("Flow complete");
                        ("done".to_string(), Ok(msg.to_string()))
                    }
                    "launch" | "launch_app" => {
                        let pkg = value.as_str().unwrap_or("");
                        let output = tokio::process::Command::new("adb")
                            .args(build_adb_args(adb_device, &[
                                "shell", "monkey", "-p", pkg, "-c",
                                "android.intent.category.LAUNCHER", "1",
                            ]))
                            .output()
                            .await;
                        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
                        match output {
                            Ok(o) if o.status.success() => (
                                format!("launch {}", pkg),
                                Ok("ok".to_string()),
                            ),
                            Ok(o) => (
                                format!("launch {}", pkg),
                                Err(anyhow::anyhow!("{}", String::from_utf8_lossy(&o.stderr))),
                            ),
                            Err(e) => (format!("launch {}", pkg), Err(anyhow::anyhow!("{}", e))),
                        }
                    }
                    other => {
                        (
                            format!("unknown: {}", other),
                            Err(anyhow::anyhow!("Unknown flow action: {}", other)),
                        )
                    }
                }
            } else {
                ("empty".to_string(), Err(anyhow::anyhow!("Empty action map")))
            }
        }
    }
}

// ── ADB helpers ────────────────────────────────────────────────────────────

async fn execute_adb_tap(
    adb_device: &Option<String>,
    x: i32,
    y: i32,
) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("adb")
        .args(build_adb_args(adb_device, &[
            "shell", "input", "tap", &x.to_string(), &y.to_string(),
        ]))
        .output()
        .await?;

    if output.status.success() {
        Ok("ok".to_string())
    } else {
        Err(anyhow::anyhow!(
            "tap failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn build_adb_args<'a>(device: &'a Option<String>, args: &'a [&'a str]) -> Vec<&'a str> {
    let mut result = Vec::new();
    if let Some(ref d) = device {
        result.push("-s");
        result.push(d.as_str());
    }
    result.extend_from_slice(args);
    result
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max])
    } else {
        s.to_string()
    }
}

// ── Flow loading ───────────────────────────────────────────────────────────

/// Load a flow YAML file.
///
/// Format uses YAML front matter (above `---`) for metadata,
/// and a YAML list below for actions:
///
/// ```yaml
/// name: Clear Notifications
/// app_id: com.android.systemui
/// ---
/// - swipe: [540, 50, 540, 800]
/// - wait: 1
/// - tap_text: "Clear all"
/// - done: "Cleared"
/// ```
fn load_flow(path: &str) -> anyhow::Result<(Flow, Vec<FlowAction>)> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Cannot read flow file '{}': {}", path, e))?;

    // Split on --- separator
    let parts: Vec<&str> = content.splitn(2, "\n---").collect();

    let (header_str, actions_str) = if parts.len() == 2 {
        (parts[0], parts[1])
    } else {
        // No separator — entire file is actions, infer name from filename
        let name = Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_string();
        return Ok((
            Flow {
                name,
                app_id: None,
                description: None,
            },
            serde_yaml::from_str(&content)
                .map_err(|e| anyhow::anyhow!("Invalid YAML in '{}': {}", path, e))?,
        ));
    };

    let flow: Flow = serde_yaml::from_str(header_str)
        .map_err(|e| anyhow::anyhow!("Invalid flow header in '{}': {}", path, e))?;

    let actions: Vec<FlowAction> = serde_yaml::from_str(actions_str)
        .map_err(|e| anyhow::anyhow!("Invalid flow actions in '{}': {}", path, e))?;

    if actions.is_empty() {
        anyhow::bail!("Flow '{}' has no actions", flow.name);
    }

    Ok((flow, actions))
}

fn collect_flows(dir: &Path, results: &mut Vec<(std::path::PathBuf, Flow)>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "yaml" || e == "yml").unwrap_or(false) {
                if let Ok((flow, _)) = load_flow(&path.to_string_lossy()) {
                    results.push((path, flow));
                }
            }
        }
    }
}