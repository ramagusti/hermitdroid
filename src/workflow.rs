use crate::action::ActionExecutor;
use crate::config::Config;
use crate::oneshot;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{error, info};

// ── ANSI colors ────────────────────────────────────────────────────────────
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

// ── Workflow schema ────────────────────────────────────────────────────────

/// A reusable AI-powered workflow definition.
///
/// Format (JSON):
/// ```json
/// {
///   "name": "slack standup",
///   "description": "Post daily standup to #standup channel",
///   "steps": [
///     {
///       "app": "com.Slack",
///       "goal": "open #standup channel, type the message and send it",
///       "form_data": { "message": "yesterday: api work\ntoday: tests\nblockers: none" },
///       "max_steps": 20
///     }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    /// Android package name to launch before this step (optional).
    /// If set, the agent will launch this app before executing the goal.
    #[serde(default)]
    pub app: Option<String>,

    /// The goal in plain English — fed directly to the oneshot loop.
    pub goal: String,

    /// Optional key-value data injected into the goal prompt.
    /// Example: { "message": "hello world" } makes the LLM aware of
    /// specific text to type without the user embedding it in the goal string.
    #[serde(default)]
    pub form_data: Option<serde_json::Map<String, serde_json::Value>>,

    /// Max steps for this specific step (overrides default 30).
    #[serde(default)]
    pub max_steps: Option<u32>,
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Run a workflow from a JSON file path.
pub async fn run_workflow(
    config: &Config,
    path: &str,
    verbose: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    // Load and parse workflow
    let workflow = load_workflow(path)?;
    let total_steps = workflow.steps.len();

    // Print header
    println!("\n{CYAN}{BOLD}🤖 Hermitdroid — Workflow Mode{RESET}\n");
    println!("  {BOLD}Workflow:{RESET} {}", workflow.name);
    if !workflow.description.is_empty() {
        println!("  {DIM}{}{RESET}", workflow.description);
    }
    println!(
        "  {BOLD}Steps:{RESET} {} | {BOLD}Model:{RESET} {} via {}",
        total_steps, config.brain.model, config.brain.backend
    );
    if dry_run {
        println!("  {YELLOW}⚠  DRY RUN — actions logged but not executed{RESET}");
    }
    println!();

    let start = std::time::Instant::now();

    // Execute each step
    for (i, step) in workflow.steps.iter().enumerate() {
        let step_num = i + 1;
        println!(
            "  {CYAN}{BOLD}━━━ Step {}/{}: {}{RESET}",
            step_num, total_steps, step.goal
        );

        // Build the effective goal: goal + form_data context
        let effective_goal = build_effective_goal(step);

        // If an app is specified, prefix the goal with launching it.
        // The oneshot loop will figure out how to handle it.
        let full_goal = if let Some(ref app) = step.app {
            format!(
                "First launch the app {} if it's not already open. Then: {}",
                app, effective_goal
            )
        } else {
            effective_goal
        };

        let max = step.max_steps.unwrap_or(30);

        // Run the oneshot loop for this step
        match oneshot::run_oneshot(config, &full_goal, max, verbose, dry_run).await {
            Ok(()) => {
                info!("Workflow step {}/{} completed: {}", step_num, total_steps, step.goal);
            }
            Err(e) => {
                error!("Workflow step {}/{} failed: {}", step_num, total_steps, e);
                println!(
                    "\n  {YELLOW}⚠  Step {} failed: {}. Continuing to next step...{RESET}\n",
                    step_num, e
                );
            }
        }

        // Between steps: press HOME to reset to a known state
        // (unless this is the last step)
        if step_num < total_steps {
            println!("  {DIM}  ↩ Returning to home screen...{RESET}");
            let adb_device = config.perception.adb_device.clone();
            let executor = ActionExecutor::new(
                dry_run || config.action.dry_run,
                adb_device,
                config.action.restricted_apps.clone(),
            );
            // Press home to get back to a clean state
            let _ = executor.execute_raw("home", &config.perception.adb_device).await;
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
        }
    }

    let elapsed = start.elapsed();
    println!(
        "\n  {GREEN}{BOLD}✅ Workflow complete{RESET} — {} steps in {:.1}s\n",
        total_steps,
        elapsed.as_secs_f64()
    );

    Ok(())
}

/// Save a one-shot goal as a reusable single-step workflow.
///
/// Creates: workspace/workflows/<name>.json
pub fn save_goal_as_workflow(
    workspace_path: &str,
    name: &str,
    goal: &str,
    app: Option<&str>,
) -> anyhow::Result<PathBuf> {
    let workflows_dir = Path::new(workspace_path).join("workflows");
    std::fs::create_dir_all(&workflows_dir)?;

    let workflow = Workflow {
        name: name.to_string(),
        description: format!("Saved from: hermitdroid run \"{}\"", goal),
        steps: vec![WorkflowStep {
            app: app.map(|s| s.to_string()),
            goal: goal.to_string(),
            form_data: None,
            max_steps: None,
        }],
    };

    let filename = sanitize_filename(name);
    let path = workflows_dir.join(format!("{}.json", filename));
    let json = serde_json::to_string_pretty(&workflow)?;
    std::fs::write(&path, &json)?;

    println!(
        "\n  {GREEN}✅ Saved workflow:{RESET} {}",
        path.display()
    );
    println!("  {DIM}Re-run with: hermitdroid workflow {}{RESET}\n", path.display());

    Ok(path)
}

/// List all available workflows in workspace/workflows/ and examples/workflows/.
pub fn list_workflows(workspace_path: &str) -> Vec<(PathBuf, Workflow)> {
    let mut results = Vec::new();

    // Check workspace workflows
    let workspace_dir = Path::new(workspace_path).join("workflows");
    if workspace_dir.exists() {
        collect_workflows(&workspace_dir, &mut results);
    }

    // Check examples
    let examples_dir = Path::new("examples/workflows");
    if examples_dir.exists() {
        collect_workflows(&examples_dir, &mut results);
    }

    results
}

// ── Internal helpers ───────────────────────────────────────────────────────

fn load_workflow(path: &str) -> anyhow::Result<Workflow> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Cannot read workflow file '{}': {}", path, e))?;

    let workflow: Workflow = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("Invalid workflow JSON in '{}': {}", path, e))?;

    if workflow.steps.is_empty() {
        anyhow::bail!("Workflow '{}' has no steps", workflow.name);
    }

    Ok(workflow)
}

/// Build the effective goal string by injecting form_data into the goal.
fn build_effective_goal(step: &WorkflowStep) -> String {
    match &step.form_data {
        Some(data) if !data.is_empty() => {
            let mut parts = vec![step.goal.clone()];
            parts.push("\n\nContext data to use:".to_string());
            for (key, value) in data {
                let val_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                parts.push(format!("  {}: {}", key, val_str));
            }
            parts.join("\n")
        }
        _ => step.goal.clone(),
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_lowercase()
}

fn collect_workflows(dir: &Path, results: &mut Vec<(PathBuf, Workflow)>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_workflows(&path, results);
            } else if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(w) = load_workflow(&path.to_string_lossy()) {
                    results.push((path, w));
                }
            }
        }
    }
}