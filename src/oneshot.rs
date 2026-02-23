use crate::action::ActionExecutor;
use crate::brain::Brain;
use crate::config::Config;
use crate::perception::Perception;
use crate::sanitizer;
use crate::soul::Workspace;
use crate::stuck::{StuckDetector, StuckStatus, RecoveryAction, action_target_key};
use std::time::{Instant, Duration};
use tracing::{error, info};

// ── ANSI colors for terminal output ─────────────────────────────────────────

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

// ── Configuration ───────────────────────────────────────────────────────────

/// Default max steps before giving up
const DEFAULT_MAX_STEPS: u32 = 30;

/// How many consecutive identical screen states before stuck recovery
const DEFAULT_STUCK_THRESHOLD: u32 = 3;

/// Settle times (ms) after different action types
const SETTLE_HEAVY_MS: u64 = 800;  // launch_app, back, home
const SETTLE_LIGHT_MS: u64 = 300;  // tap, long_press, swipe
const SETTLE_NONE_MS: u64 = 50;    // type_text, wait, etc.

/// Fast hash for screen change detection (same as main.rs)
fn simple_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

// ── Public entry point ──────────────────────────────────────────────────────

pub async fn run_oneshot(
    config: &Config,
    goal: &str,
    max_steps: u32,
    verbose: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let max_steps = if max_steps == 0 { DEFAULT_MAX_STEPS } else { max_steps };
    let dry_run = dry_run || config.action.dry_run;

    // ── Initialize components (lightweight — no server, no sessions) ────
    let workspace = Workspace::new(&config.agent.workspace_path, config.agent.bootstrap_max_chars);
    let brain = Brain::new(&config.brain);

    // Resolve ADB device (Tailscale handled at higher level if needed)
    let adb_device = config.perception.adb_device.clone();
    let _perception = Perception::new(
        adb_device.clone(),
        config.perception.priority_apps.clone(),
    );
    let executor = ActionExecutor::new(
        dry_run,
        adb_device,
        config.action.restricted_apps.clone(),
    );

    // ── Print header ────────────────────────────────────────────────────
    println!("\n{CYAN}{BOLD}🤖 Hermitdroid — One-Shot Mode{RESET}\n");
    println!("  {BOLD}Goal:{RESET} {goal}");
    println!(
        "  {BOLD}Model:{RESET} {} via {}",
        config.brain.model, config.brain.backend
    );
    if dry_run {
        println!("  {YELLOW}⚠  DRY RUN — actions logged but not executed{RESET}");
    }
    println!(
        "  {DIM}Max steps: {} | Vision: {}{RESET}\n",
        max_steps,
        if config.brain.vision_enabled { "on" } else { "off" }
    );

    // ── Assemble system prompt with workspace context ───────────────────
    // The one-shot system prompt includes SOUL/TOOLS/AGENTS context but
    // frames the task as a single goal to complete, not an ongoing daemon.
    let workspace_ctx = workspace.assemble_bootstrap();
    let system_prompt = build_oneshot_system_prompt(&brain, &workspace_ctx, goal);

    // ── State tracking ──────────────────────────────────────────────────
    let start = Instant::now();
    let mut stuck = StuckDetector::new(config.stuck.clone());
    let mut total_actions: u32 = 0;
    let mut user_prompt_suffix: Option<String> = None;

    // ── Main loop ───────────────────────────────────────────────────────
    for step in 1..=max_steps {
        // 1. Perceive — get current screen state
        let vision_mode = if config.brain.vision_enabled {
            crate::sanitizer::VisionMode::Fallback
        } else {
            crate::sanitizer::VisionMode::Off
        };
        let perception_result = Some(sanitizer::perceive_screen(
            &config.perception.adb_device,
            vision_mode,
            config.perception.max_elements,
        ).await);
        let screen_text = perception_result
            .as_ref()
            .map(|s| s.formatted_text.clone())
            .unwrap_or_else(|| "[No screen data available]".to_string());

        // For verbose output, show perception mode:
        if verbose {
            if let Some(ref pr) = perception_result {
                let mode_str = if pr.used_vision { "a11y + vision" } else { "a11y only" };
                println!(
                    "  {DIM}[perception: {} | {} elements]{RESET}",
                    mode_str,
                    pr.screen.elements.len(),
                );
            }
        }

        // 2. Stuck detection (replaces old hash comparison)
        let screen_hash = simple_hash(&screen_text);
        match stuck.check_screen(screen_hash) {
            StuckStatus::Ok => {}
            StuckStatus::Hint(hint) => {
                // Inject hint into prompt — LLM will self-correct
                println!(
                    "  {YELLOW}⚠  Stuck detected: injecting recovery hint{RESET}"
                );
                // Append hint to user_prompt below
                user_prompt_suffix = Some(hint.message);
            }
            StuckStatus::Recover(action) => {
                println!(
                    "  {YELLOW}⚠  Stuck — executing recovery action{RESET}"
                );
                match action {
                    RecoveryAction::Back => {
                        let _ = executor.execute_raw("back", &config.perception.adb_device).await;
                        tokio::time::sleep(Duration::from_millis(800)).await;
                    }
                    RecoveryAction::HomeAndRelaunch { .. } => {
                        let _ = executor.execute_raw("home", &config.perception.adb_device).await;
                        tokio::time::sleep(Duration::from_millis(1000)).await;
                        // Optionally relaunch the target app
                    }
                    RecoveryAction::ForceStopAndRelaunch { app_package } => {
                        let _ = executor.execute_raw(&format!("am force-stop {}", app_package), &config.perception.adb_device).await;
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        let _ = executor.execute_raw(&format!("monkey -p {} 1", app_package), &config.perception.adb_device).await;
                    }
                }
                continue; // Re-perceive after recovery
            }
            StuckStatus::GiveUp(msg) => {
                println!("  {RED}❌ {msg}{RESET}");
                break;
            }
        }

        // 3. Build step prompt
        let now = chrono::Utc::now().format("%H:%M:%S UTC").to_string();
        let user_prompt = build_oneshot_step_prompt(
            &screen_text, goal, step, max_steps, &now,
        );

        // 4. Call LLM
        let screenshot = perception_result
            .as_ref()
            .and_then(|s| s.screenshot_base64.as_deref());

        let final_user_prompt = if let Some(ref suffix) = user_prompt_suffix {
            format!("{}\n{}", user_prompt, suffix)
        } else {
            user_prompt.clone()
        };

        let raw = match brain.think(&system_prompt, &final_user_prompt, screenshot).await {
            Ok(r) => r,
            Err(e) => {
                println!("  {RED}[{step}/{max_steps}] ❌ LLM error: {e}{RESET}");
                error!("LLM error at step {}: {}", step, e);
                // Wait and retry on next step
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                continue;
            }
        };

        let response = brain.parse_response(&raw);

        // 5. Check if done
        if is_done(&response) {
            let reason = response
                .actions
                .iter()
                .find(|a| a.action_type == "done")
                .map(|a| a.reason.as_str())
                .or(response.reflection.as_deref())
                .unwrap_or("Goal completed");

            println!(
                "  {GREEN}[{step}/{max_steps}] ✅ Done — {reason}{RESET}"
            );
            workspace
                .append_daily_memory(&format!("[run] Goal completed: {}", goal))
                .ok();
            break;
        }

        // 6. Check HEARTBEAT_OK (nothing to do)
        if response.reflection.as_deref() == Some("HEARTBEAT_OK") {
            if verbose {
                println!(
                    "  {DIM}[{step}/{max_steps}] (idle — no action needed){RESET}"
                );
            }
            continue;
        }

        // 7. Show thinking (verbose mode)
        if verbose {
            if let Some(ref r) = response.reflection {
                if !r.is_empty() && r != "HEARTBEAT_OK" {
                    println!(
                        "  {CYAN}[{step}/{max_steps}] 🧠 {r}{RESET}"
                    );
                }
            }
        }

        // 8. Execute actions
        if response.actions.is_empty() {
            if verbose {
                println!(
                    "  {DIM}[{step}/{max_steps}] (no actions){RESET}"
                );
            }
            continue;
        }

        for action in &response.actions {
            // Skip "done" actions (already handled above)
            if action.action_type == "done" {
                continue;
            }

            let action_start = Instant::now();
            match executor.execute(action).await {
                Ok(result) => {
                    let ms = action_start.elapsed().as_millis();
                    total_actions += 1;

                    // Format output: show step number on first action, indent rest
                    let prefix = if !verbose {
                        format!("{BOLD}[{step}/{max_steps}]{RESET} ")
                    } else {
                        "       ".to_string()
                    };

                    // Build action description
                    let action_desc = format_action_desc(action);
                    let class_icon = match action.classification.as_str() {
                        "RED" => format!("{RED}🔴{RESET}"),
                        "YELLOW" => format!("{YELLOW}🟡{RESET}"),
                        _ => format!("{GREEN}🟢{RESET}"),
                    };

                    println!(
                        "  {prefix}{class_icon} ▸ {action_desc} {DIM}({ms}ms){RESET}"
                    );

                    info!(
                        "Step {}: {} ({}) → {} [{}ms]",
                        step, action.action_type, action.reason, result, ms
                    );
                }
                Err(e) => {
                    let ms = action_start.elapsed().as_millis();
                    println!(
                        "  {RED}[{step}/{max_steps}] ❌ {} failed: {e} ({ms}ms){RESET}",
                        action.action_type
                    );
                    error!("Step {} action {} failed: {}", step, action.action_type, e);
                    // Don't continue blindly after a failure
                    break;
                }
            }

            // Adaptive settle wait
            let settle_ms = match action.action_type.as_str() {
                "launch_app" | "back" | "home" => SETTLE_HEAVY_MS,
                "tap" | "long_press" | "swipe" => SETTLE_LIGHT_MS,
                _ => SETTLE_NONE_MS,
            };
            tokio::time::sleep(tokio::time::Duration::from_millis(settle_ms)).await;
            
            // Record for repetition/drift detection
            let target = action_target_key(
                &action.action_type,
                action.x,
                action.y,
                action.text.as_deref(),
                action.app.as_deref(),
            );
            match stuck.record_action(&action.action_type, &target) {
                StuckStatus::Hint(hint) => {
                    println!("  {YELLOW}⚠  {}{RESET}", hint.message.lines().next().unwrap_or(""));
                    // Will inject hint on next LLM call
                }
                StuckStatus::Recover(_recovery) => {
                    // Execute recovery and break action loop
                    break;
                }
                StuckStatus::GiveUp(msg) => {
                    println!("  {RED}❌ {msg}{RESET}");
                    break;
                }
                StuckStatus::Ok => {}
            }
        }
    }

    // ── Summary ─────────────────────────────────────────────────────────
    let elapsed = start.elapsed();
    println!(
        "\n  {DIM}Total: {:.1}s ({} actions){RESET}\n",
        elapsed.as_secs_f64(),
        total_actions
    );

    workspace
        .append_daily_memory(&format!(
            "[run] \"{}\" — {} actions in {:.1}s",
            goal, total_actions, elapsed.as_secs_f64()
        ))
        .ok();

    Ok(())
}

// ── Prompt builders ─────────────────────────────────────────────────────────

/// Build the system prompt for one-shot mode.
/// Includes workspace context (SOUL, TOOLS, AGENTS) but frames it as a
/// single-goal task runner, not a persistent daemon.
fn build_oneshot_system_prompt(brain: &Brain, workspace_ctx: &crate::soul::BootstrapContext, goal: &str) -> String {
    // Get the base system prompt from the brain (includes SOUL, TOOLS, etc.)
    let base = brain.build_system_prompt(workspace_ctx);

    format!(
        r#"{base}

=== ONE-SHOT MODE ===
You are running in ONE-SHOT MODE. Your single goal is:

  "{goal}"

Rules for one-shot mode:
1. Focus ONLY on completing this goal. Do not check notifications or do other tasks.
2. After EACH step, you will see the updated screen. Plan one step at a time.
3. When the goal is FULLY COMPLETE, respond with action type "done" and explain what was accomplished.
4. If the goal is IMPOSSIBLE (app not installed, permission denied, etc.), respond with "done" and explain why.
5. Use the fewest steps possible. Be efficient.
6. ALWAYS use the @(x,y) coordinates from the UI elements list for tap actions. Never guess coordinates.
7. When you need to type text, first tap the input field, then use type_text action.
8. If the screen hasn't changed after your action, try a different approach."#
    )
}

/// Build the per-step user prompt with current screen state.
fn build_oneshot_step_prompt(
    screen_text: &str,
    goal: &str,
    step: u32,
    max_steps: u32,
    time: &str,
) -> String {
    let urgency = if step > max_steps * 3 / 4 {
        "\n⚠️ Running low on steps! Prioritize completing the goal quickly."
    } else {
        ""
    };

    format!(
        r#"Step {step}/{max_steps} | {time}
Goal: "{goal}"
{urgency}

=== CURRENT SCREEN ===
{screen_text}

What is your next action to achieve the goal? Respond in the standard action format.
If the goal is complete, use action type "done"."#
    )
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Check if the LLM response indicates the goal is done.
fn is_done(response: &crate::brain::AgentResponse) -> bool {
    // Explicit "done" action
    if response
        .actions
        .iter()
        .any(|a| a.action_type == "done" || a.action_type == "DONE")
    {
        return true;
    }

    // Reflection says done (some models do this instead of an action)
    if let Some(ref r) = response.reflection {
        let r_lower = r.to_lowercase();
        if r_lower.contains("goal is complete")
            || r_lower.contains("goal completed")
            || r_lower.contains("task is done")
            || r_lower.contains("task complete")
            || r_lower.starts_with("done:")
            || r_lower.starts_with("done -")
        {
            return true;
        }
    }

    false
}

/// Format an action for terminal display.
fn format_action_desc(action: &crate::brain::AgentAction) -> String {
    match action.action_type.as_str() {
        "tap" => {
            if let (Some(x), Some(y)) = (action.x, action.y) {
                format!("tap @({},{}) {}", x, y, truncate(&action.reason, 50))
            } else {
                format!("tap {}", truncate(&action.reason, 60))
            }
        }
        "type_text" => {
            let text = action
                .text
                .as_deref()
                .unwrap_or(&action.reason);
            format!("type \"{}\"", truncate(text, 40))
        }
        "launch_app" => {
            let app = action
                .app
                .as_deref()
                .unwrap_or(&action.reason);
            format!("launch {}", app)
        }
        "swipe" => format!("swipe {}", truncate(&action.reason, 50)),
        "back" => "back".to_string(),
        "home" => "home".to_string(),
        "long_press" => {
            if let (Some(x), Some(y)) = (action.x, action.y) {
                format!("long_press @({},{}) {}", x, y, truncate(&action.reason, 40))
            } else {
                format!("long_press {}", truncate(&action.reason, 50))
            }
        }
        "key" => {
            let key = action
                .text
                .as_deref()
                .unwrap_or(&action.reason);
            format!("key {}", key)
        }
        "wait" => "wait".to_string(),
        other => format!("{} {}", other, truncate(&action.reason, 50)),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

/// Build ADB device args for raw commands
fn adb_device_args(config: &Config) -> Option<String> {
    config.perception.adb_device.clone()
}