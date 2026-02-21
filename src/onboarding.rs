// src/onboarding.rs — Interactive first-run onboarding wizard
//
// Runs as `hermitdroid onboard`. Walks user through:
//   1. AI provider / model / endpoint / API key
//   2. Vision (screen sharing) toggle
//   3. ADB connection (USB or Wi-Fi)
//   4. Tailscale remote access (optional, only if ADB set up)
//
// Generates a config.toml that matches the existing Config struct layout.

use std::io::{self, BufRead, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

// ── ANSI ────────────────────────────────────────────────────────────────────

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

fn banner() {
    println!(
        r#"
{CYAN}{BOLD}╔══════════════════════════════════════════════════════╗
║          🤖  Hermitdroid — First Run Setup           ║
╚══════════════════════════════════════════════════════╝{RESET}

Welcome! This wizard will walk you through:

  {GREEN}1.{RESET} AI provider & model configuration
  {GREEN}2.{RESET} Screen sharing (vision) preference
  {GREEN}3.{RESET} ADB connection to your Android device
  {GREEN}4.{RESET} Tailscale for remote access {DIM}(optional){RESET}

Press {BOLD}Ctrl+C{RESET} at any time to abort.
"#
    );
}

// ── Input helpers ───────────────────────────────────────────────────────────

fn prompt(msg: &str) -> String {
    print!("{BOLD}{msg}{RESET} ");
    io::stdout().flush().unwrap();
    let mut buf = String::new();
    io::stdin().lock().read_line(&mut buf).unwrap();
    buf.trim().to_string()
}

fn prompt_default(msg: &str, default: &str) -> String {
    print!("{BOLD}{msg}{RESET} {DIM}[{default}]{RESET}: ");
    io::stdout().flush().unwrap();
    let mut buf = String::new();
    io::stdin().lock().read_line(&mut buf).unwrap();
    let val = buf.trim().to_string();
    if val.is_empty() { default.to_string() } else { val }
}

fn prompt_yes_no(msg: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "Y/n" } else { "y/N" };
    let answer = prompt_default(msg, hint);
    match answer.to_lowercase().as_str() {
        "y" | "yes" | "Y/n" => true,
        "n" | "no" | "y/N" => false,
        _ => default_yes,
    }
}

fn prompt_choice(msg: &str, options: &[(&str, &str)]) -> usize {
    println!("\n{BOLD}{msg}{RESET}\n");
    for (i, (label, desc)) in options.iter().enumerate() {
        println!("  {GREEN}{}{RESET}. {BOLD}{label}{RESET} — {desc}", i + 1);
    }
    println!();
    loop {
        let raw = prompt("Choose [number]:");
        if let Ok(n) = raw.parse::<usize>() {
            if n >= 1 && n <= options.len() { return n - 1; }
        }
        println!("{RED}  Invalid. Enter 1–{}.{RESET}", options.len());
    }
}

// ── Step 1 + 2: AI Provider & Vision ────────────────────────────────────────

struct BrainResult {
    backend: String,
    model: String,
    endpoint: String,
    api_key: Option<String>,
    vision_enabled: bool,
}

fn step_ai_and_vision() -> BrainResult {
    println!("\n{CYAN}━━━ Step 1/4: AI Provider & Model ━━━{RESET}\n");

    let providers = &[
        ("Ollama (local)", "Free, runs on your machine. No API key needed."),
        ("OpenAI", "GPT-4o, GPT-4o-mini, etc. Requires API key."),
        ("OpenAI Codex", "Uses `codex` CLI login. No API key needed."),
        ("Anthropic", "Claude Sonnet, Opus. Requires API key."),
        ("Google Gemini", "Gemini Pro / Flash. Requires API key."),
        ("OpenAI-compatible server", "vLLM, LM Studio, text-generation-webui, etc."),
        ("Custom / Other", "Any OpenAI-compatible endpoint."),
    ];

    let choice = prompt_choice("Which AI provider will you use?", providers);

    let (backend, default_endpoint, needs_key, is_codex) = match choice {
        0 => ("ollama", "http://localhost:11434", false, false),
        1 => ("openai_compatible", "https://api.openai.com/v1", true, false),
        2 => ("codex", "https://chatgpt.com/backend-api/codex/responses", false, true),
        3 => ("openai_compatible", "https://api.anthropic.com", true, false),
        4 => ("google", "https://generativelanguage.googleapis.com/v1beta", true, false),
        5 => ("openai_compatible", "http://localhost:8000/v1", false, false),
        6 => ("openai_compatible", "", false, false),
        _ => unreachable!(),
    };

    // Handle Codex login
    if is_codex {
        println!("\n  {BOLD}OpenAI Codex uses browser-based login (no API key).{RESET}");
        println!("  {DIM}This runs `codex login` which opens your browser to authenticate.{RESET}\n");

        // Check if codex CLI exists
        let codex_installed = std::process::Command::new("codex")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if !codex_installed {
            println!("  {YELLOW}⚠  `codex` CLI not found.{RESET}");
            println!("  Install it first:");
            println!("    {DIM}npm install -g @openai/codex{RESET}");
            println!("  Then re-run: {BOLD}hermitdroid onboard{RESET}\n");

            if !prompt_yes_no("  Continue anyway? (you can run `codex login` later)", true) {
                std::process::exit(0);
            }
        } else {
            println!("  {GREEN}✓  `codex` CLI found.{RESET}");

            if prompt_yes_no("  Run `codex login` now?", true) {
                println!("  {DIM}Opening browser for authentication...{RESET}\n");
                let status = std::process::Command::new("codex")
                    .arg("login")
                    .status();
                match status {
                    Ok(s) if s.success() => println!("\n  {GREEN}✓  Codex login successful!{RESET}"),
                    Ok(_) => println!("\n  {YELLOW}⚠  Login may not have completed. Run `codex login` manually.{RESET}"),
                    Err(e) => println!("\n  {RED}✗  Failed to run codex login: {e}{RESET}"),
                }
            } else {
                println!("  {DIM}Run `codex login` before starting the agent.{RESET}");
            }
        }
    }

    let default_model = match choice {
        0 => "yeahdongcn/AutoGLM-Phone-9B",
        1 => "gpt-4o",
        2 => "codex-mini",
        3 => "claude-sonnet-4-20250514",
        4 => "gemini-2.0-flash",
        _ => "default",
    };

    if choice == 0 {
        println!("\n  {DIM}Popular Ollama models for Android control:{RESET}");
        println!("    • yeahdongcn/AutoGLM-Phone-9B {DIM}(phone UI specialist, recommended){RESET}");
        println!("    • qwen2.5-vl:7b               {DIM}(vision + reasoning){RESET}");
        println!("    • llama3.1:8b                  {DIM}(text-only, fast){RESET}");
        println!("    • phi3.5:latest                {DIM}(lightweight){RESET}");
    }

    if is_codex {
        println!("\n  {DIM}Codex models:{RESET}");
        println!("    • gpt-5.1-codex-mini  {DIM}(fast, lightweight){RESET}");
        println!("    • gpt-5.2-codex     {DIM}(more powerful){RESET}");
        println!("    • gpt-5.3      {DIM}(newest){RESET}");
    }

    let model = prompt_default("\n  Model name:", default_model);

    let endpoint = if default_endpoint.is_empty() {
        prompt("  Endpoint URL (e.g. http://myserver:8000/v1):")
    } else {
        prompt_default("  Endpoint URL:", default_endpoint)
    };

    let api_key = if is_codex {
        // Codex uses its own auth token from `codex login`, not an API key
        println!("\n  {DIM}Codex uses its own auth from `codex login` — no API key needed.{RESET}");
        None
    } else if needs_key {
        let key = prompt("  API key:");
        if key.is_empty() {
            println!("  {YELLOW}⚠  No key. Set later in config.toml or HERMITDROID_API_KEY env.{RESET}");
            None
        } else { Some(key) }
    } else {
        let want = prompt_yes_no("  Set an API key? (some local servers need auth)", false);
        if want { let k = prompt("  API key:"); if k.is_empty() { None } else { Some(k) } }
        else { None }
    };

    // ── Vision ──────────────────────────────────────────────────────────
    println!("\n{CYAN}━━━ Step 2/4: Screen Sharing (Vision) ━━━{RESET}\n");
    println!("  Vision lets the AI see your phone screen for smarter decisions.");
    println!("  {DIM}Requires a vision-capable model (e.g. AutoGLM, qwen2.5-vl, gpt-4o).{RESET}");
    println!("  {DIM}Uses more bandwidth/tokens. Toggle later in config.toml.{RESET}\n");

    let vision_enabled = prompt_yes_no("  Enable screen sharing to AI?", true);
    println!(
        "  {}",
        if vision_enabled {
            format!("{GREEN}✓  Vision enabled.{RESET}")
        } else {
            format!("{YELLOW}✓  Vision disabled. Text-only context.{RESET}")
        }
    );

    BrainResult { backend: backend.into(), model, endpoint, api_key, vision_enabled }
}

// ── Step 3: ADB ─────────────────────────────────────────────────────────────

struct AdbResult {
    bridge_mode: String,
    adb_device: String,
}

fn step_adb() -> Option<AdbResult> {
    println!("\n{CYAN}━━━ Step 3/4: ADB Connection ━━━{RESET}\n");

    // Check adb binary
    let adb_ok = Command::new("adb").arg("version").output()
        .map(|o| o.status.success()).unwrap_or(false);

    if !adb_ok {
        println!("  {RED}✗  'adb' not found in PATH.{RESET}");
        println!("    Linux:   sudo apt install adb");
        println!("    macOS:   brew install android-platform-tools");
        println!("    Windows: scoop install adb");
        println!();
        if prompt_yes_no("  Continue without ADB? (configure later)", true) {
            return None;
        }
        std::process::exit(0);
    }
    println!("  {GREEN}✓  ADB found.{RESET}");

    let methods = &[
        ("USB", "Phone connected via USB cable (simplest)"),
        ("Wi-Fi (local network)", "Wireless ADB, same Wi-Fi"),
    ];
    let choice = prompt_choice("How is your phone connected?", methods);

    match choice {
        0 => {
            println!("\n  Make sure:");
            println!("    1. USB Debugging enabled on phone");
            println!("    2. Phone plugged in via USB");
            println!("    3. RSA key prompt accepted on phone\n");

            // Show connected devices
            if let Ok(out) = Command::new("adb").arg("devices").output() {
                let text = String::from_utf8_lossy(&out.stdout);
                let devs: Vec<_> = text.lines()
                    .filter(|l| l.contains("\tdevice") || l.contains("\tunauthorized"))
                    .collect();
                if devs.is_empty() {
                    println!("  {YELLOW}⚠  No devices detected. Plug in and retry.{RESET}");
                } else {
                    println!("  {GREEN}Detected:{RESET}");
                    for d in &devs { println!("    • {d}"); }
                }
            }

            Some(AdbResult { bridge_mode: "adb".into(), adb_device: String::new() })
        }
        1 => {
            println!("\n  Wi-Fi ADB setup:");
            println!("    1. Connect phone via USB first");
            println!("    2. Run: adb tcpip 5555");
            println!("    3. Find phone IP: Settings → Wi-Fi → your network");
            println!("    4. Disconnect USB\n");

            let ip = prompt_default("  Phone IP (or blank to set later):", "");

            let addr = if ip.is_empty() {
                println!("  {YELLOW}⚠  Set perception.adb_device in config.toml later.{RESET}");
                "192.168.1.100:5555".into()
            } else {
                let a = if ip.contains(':') { ip } else { format!("{ip}:5555") };
                println!("  Connecting to {a}...");
                if let Ok(out) = Command::new("adb").args(["connect", &a]).output() {
                    let s = String::from_utf8_lossy(&out.stdout);
                    if s.contains("connected") {
                        println!("  {GREEN}✓  Connected!{RESET}");
                    } else {
                        println!("  {YELLOW}⚠  {}{RESET}", s.trim());
                    }
                }
                a
            };

            Some(AdbResult { bridge_mode: "adb".into(), adb_device: addr })
        }
        _ => unreachable!(),
    }
}

// ── Step 4: Tailscale ───────────────────────────────────────────────────────

struct TailscaleResult {
    enabled: bool,
    phone_hostname: String,
    adb_port: u16,
    auto_connect: bool,
    health_check_interval_secs: u64,
    max_failures_before_reconnect: u32,
}

fn step_tailscale(adb_configured: bool) -> Option<TailscaleResult> {
    println!("\n{CYAN}━━━ Step 4/4: Tailscale Remote Access (Optional) ━━━{RESET}\n");

    if !adb_configured {
        println!("  {DIM}Tailscale setup requires ADB to be configured first.{RESET}");
        println!("  {DIM}Set up ADB, then re-run: hermitdroid onboard{RESET}");
        return None;
    }

    println!("  Tailscale creates a secure WireGuard mesh VPN so you can control");
    println!("  your phone from anywhere — no USB or same Wi-Fi needed.\n");
    println!("  {DIM}Requirements:{RESET}");
    println!("    • Tailscale on this machine  ({BOLD}tailscale{RESET} CLI)");
    println!("    • Tailscale app on your phone (Play Store)");
    println!("    • Both signed into the same account / tailnet");
    println!("    • ADB over TCP on phone (adb tcpip 5555)\n");

    if !prompt_yes_no("  Set up Tailscale for remote ADB?", true) {
        println!("  {DIM}Skipped. Enable later in config.toml [tailscale].{RESET}");
        return None;
    }

    // Check binary
    let ts_ok = Command::new("tailscale").arg("version").output()
        .map(|o| o.status.success()).unwrap_or(false);

    if !ts_ok {
        println!("\n  {RED}✗  'tailscale' not found.{RESET}");
        println!("    Install: curl -fsSL https://tailscale.com/install.sh | sh");
        println!("    Then: sudo tailscale up");
        println!("    Then re-run: {BOLD}hermitdroid onboard{RESET}");
        return None;
    }
    println!("  {GREEN}✓  Tailscale CLI found.{RESET}");

    // Check connected
    let status_ok = Command::new("tailscale").arg("status").output()
        .map(|o| o.status.success()).unwrap_or(false);

    if !status_ok {
        println!("  {YELLOW}⚠  Tailscale not connected.{RESET}");
        if prompt_yes_no("  Run 'sudo tailscale up' now?", true) {
            let _ = Command::new("sudo").args(["tailscale", "up"]).status();
        }
        let recheck = Command::new("tailscale").arg("status").output()
            .map(|o| o.status.success()).unwrap_or(false);
        if !recheck {
            println!("  {RED}✗  Still not connected. Configure manually later.{RESET}");
            return None;
        }
    }

    // Show self IP
    if let Ok(out) = Command::new("tailscale").args(["ip", "-4"]).output() {
        if out.status.success() {
            let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("  {GREEN}✓  Tailscale connected. Your IP: {ip}{RESET}");
        }
    }

    // Show peers
    println!("\n  {BOLD}Devices on your tailnet:{RESET}");
    if let Ok(out) = Command::new("tailscale").arg("status").output() {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines().take(15) {
            if !line.trim().is_empty() {
                println!("    {line}");
            }
        }
    }

    println!("\n  {DIM}Find your Android phone in the list above.{RESET}");
    println!("  {DIM}It should show a hostname like 'pixel-7' or a 100.x.y.z IP.{RESET}\n");

    let phone_hostname = prompt(
        "  Phone's Tailscale hostname or IP (e.g. 'my-pixel' or '100.64.1.2'):"
    );

    if phone_hostname.is_empty() {
        println!("  {YELLOW}⚠  No hostname. Set [tailscale] phone_hostname in config.toml later.{RESET}");
        return Some(TailscaleResult {
            enabled: false,
            phone_hostname: "my-android-phone".into(),
            adb_port: 5555,
            auto_connect: true,
            health_check_interval_secs: 60,
            max_failures_before_reconnect: 3,
        });
    }

    let adb_port: u16 = prompt_default("  ADB TCP port on phone:", "5555")
        .parse().unwrap_or(5555);

    // Try connecting
    println!("\n  Testing connection to {phone_hostname}:{adb_port}...");

    // Resolve hostname → IP
    let test_ip = if phone_hostname.starts_with("100.") || phone_hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        phone_hostname.clone()
    } else {
        Command::new("tailscale").args(["ip", "-4", &phone_hostname]).output().ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| phone_hostname.clone())
    };

    let test_addr = format!("{test_ip}:{adb_port}");
    if let Ok(parsed) = test_addr.parse::<std::net::SocketAddr>() {
        match TcpStream::connect_timeout(&parsed, Duration::from_secs(5)) {
            Ok(_) => {
                println!("  {GREEN}✓  TCP OK!{RESET}");
                // Try adb connect
                if let Ok(out) = Command::new("adb").args(["connect", &test_addr]).output() {
                    let s = String::from_utf8_lossy(&out.stdout);
                    if s.contains("connected") {
                        println!("  {GREEN}✓  ADB connected via Tailscale!{RESET} 🎉");
                    } else {
                        println!("  {YELLOW}⚠  ADB: {}{RESET}", s.trim());
                        println!("  {DIM}Ensure ADB TCP is enabled: adb tcpip 5555{RESET}");
                    }
                }
            }
            Err(e) => {
                println!("  {YELLOW}⚠  TCP failed: {e}{RESET}");
                println!("  {DIM}• Phone Tailscale app running?{RESET}");
                println!("  {DIM}• ADB TCP enabled? (adb tcpip 5555){RESET}");
                println!("  {DIM}• Wrong hostname/IP?{RESET}");
            }
        }
    }

    let auto_connect = prompt_yes_no("\n  Auto-connect via Tailscale on startup?", true);

    println!("\n  {GREEN}✓  Tailscale configured!{RESET}");

    Some(TailscaleResult {
        enabled: true,
        phone_hostname,
        adb_port,
        auto_connect,
        health_check_interval_secs: 60,
        max_failures_before_reconnect: 3,
    })
}

// ── Config generation (matches existing config.toml structure) ──────────────

fn generate_config(
    brain: &BrainResult,
    adb: &Option<AdbResult>,
    ts: &Option<TailscaleResult>,
    config_path: &Path,
) -> String {
    let mut c = String::new();

    c.push_str("# Hermitdroid — Configuration\n");
    c.push_str("# Generated by `hermitdroid onboard`\n\n");

    // [agent] — keep defaults
    c.push_str("[agent]\n");
    c.push_str("name = \"Hermitdroid\"\n");
    c.push_str("heartbeat_interval_secs = 30\n");
    c.push_str("gateway_heartbeat_interval_secs = 1800\n");
    // Use absolute workspace_path relative to config file location
    let ws_abs = std::fs::canonicalize(config_path.parent().unwrap_or(Path::new(".")))
        .unwrap_or_else(|_| config_path.parent().unwrap_or(Path::new(".")).to_path_buf())
        .join("workspace");
    c.push_str(&format!("workspace_path = \"{}\"\n", ws_abs.display()));
    c.push_str("bootstrap_max_chars = 20000\n\n");

    // [brain]
    c.push_str("[brain]\n");
    c.push_str(&format!("backend = \"{}\"\n", brain.backend));
    c.push_str(&format!("model = \"{}\"\n", brain.model));
    c.push_str(&format!("endpoint = \"{}\"\n", brain.endpoint));
    c.push_str(&format!("vision_enabled = {}\n", brain.vision_enabled));
    c.push_str("max_tokens = 4096\n");
    c.push_str("temperature = 0.7\n");
    c.push_str("thinking = \"medium\"\n");
    if let Some(ref key) = brain.api_key {
        c.push_str(&format!("api_key = \"{key}\"\n"));
    } else {
        c.push_str("# api_key = \"\"  # or set HERMITDROID_API_KEY env var\n");
    }
    c.push_str("\n");

    // [perception]
    c.push_str("[perception]\n");
    let bridge = adb.as_ref().map(|a| a.bridge_mode.as_str()).unwrap_or("adb");
    c.push_str(&format!("bridge_mode = \"{bridge}\"\n"));

    // adb_device: Tailscale overrides Wi-Fi local
    let adb_device = if let Some(ref ts_cfg) = ts {
        if ts_cfg.enabled {
            // Will be resolved at runtime by TailscaleManager
            format!("# adb_device managed by Tailscale ({}:{})\n", ts_cfg.phone_hostname, ts_cfg.adb_port)
        } else {
            adb.as_ref().and_then(|a| if a.adb_device.is_empty() { None } else { Some(format!("adb_device = \"{}\"\n", a.adb_device)) })
                .unwrap_or_else(|| "# adb_device = \"\"  # set for Wi-Fi ADB or multiple devices\n".into())
        }
    } else {
        adb.as_ref().and_then(|a| if a.adb_device.is_empty() { None } else { Some(format!("adb_device = \"{}\"\n", a.adb_device)) })
            .unwrap_or_else(|| "# adb_device = \"\"  # set for Wi-Fi ADB or multiple devices\n".into())
    };
    c.push_str(&adb_device);

    c.push_str("android_ws_address = \"ws://192.168.1.100:9090\"\n");
    c.push_str("screen_capture_interval_secs = 0\n");
    c.push_str("notifications_enabled = true\n");
    c.push_str("accessibility_enabled = true\n");
    c.push_str("priority_apps = [\"whatsapp\", \"telegram\", \"gmail\", \"calendar\"]\n\n");

    // [action]
    c.push_str("[action]\n");
    c.push_str("dry_run = false\n");
    c.push_str("confirmation_timeout_secs = 60\n");
    c.push_str("restricted_apps = [\"banking\", \"finance\", \"pay\", \"wallet\", \"grab.driver\"]\n\n");

    // [server]
    c.push_str("[server]\n");
    // When Tailscale enabled, keep 0.0.0.0 so dashboard is reachable from tailnet
    c.push_str("host = \"0.0.0.0\"\n");
    c.push_str("port = 8420\n");
    c.push_str("# auth_token = \"\"  # set for production\n\n");

    // [tailscale]
    c.push_str("[tailscale]\n");
    match ts {
        Some(ref t) => {
            c.push_str(&format!("enabled = {}\n", t.enabled));
            c.push_str(&format!("phone_hostname = \"{}\"\n", t.phone_hostname));
            c.push_str(&format!("adb_port = {}\n", t.adb_port));
            c.push_str(&format!("auto_connect = {}\n", t.auto_connect));
            c.push_str(&format!("health_check_interval_secs = {}\n", t.health_check_interval_secs));
            c.push_str(&format!("max_failures_before_reconnect = {}\n", t.max_failures_before_reconnect));
        }
        None => {
            c.push_str("enabled = false\n");
            c.push_str("# phone_hostname = \"my-android-phone\"\n");
            c.push_str("# adb_port = 5555\n");
            c.push_str("# auto_connect = true\n");
            c.push_str("# health_check_interval_secs = 60\n");
            c.push_str("# max_failures_before_reconnect = 3\n");
        }
    }
    c.push_str("\n");

    // [hooks]
    c.push_str("[hooks]\n");
    c.push_str("# on_boot = \"BOOT.md\"\n");
    c.push_str("# on_session_new = \"\"\n");
    c.push_str("# on_unlock = \"\"\n");

    c
}

// ── Summary ─────────────────────────────────────────────────────────────────

fn print_summary(
    brain: &BrainResult,
    adb: &Option<AdbResult>,
    ts: &Option<TailscaleResult>,
    config_path: &Path,
) {
    println!(
        "\n{CYAN}{BOLD}╔══════════════════════════════════════════════════════╗
║               ✅  Setup Complete!                    ║
╚══════════════════════════════════════════════════════╝{RESET}\n"
    );

    println!("  {BOLD}AI Provider:{RESET}   {} / {}", brain.backend, brain.model);
    println!("  {BOLD}Endpoint:{RESET}      {}", brain.endpoint);
    println!("  {BOLD}Vision:{RESET}        {}", if brain.vision_enabled { "✓ Enabled" } else { "✗ Disabled" });
    println!("  {BOLD}ADB:{RESET}           {}",
        adb.as_ref().map(|a| {
            if a.adb_device.is_empty() { "USB (auto-detect)".into() }
            else { format!("Wi-Fi — {}", a.adb_device) }
        }).unwrap_or_else(|| "Not configured".into())
    );
    println!("  {BOLD}Tailscale:{RESET}     {}",
        ts.as_ref().filter(|t| t.enabled)
            .map(|t| format!("✓ {} (port {})", t.phone_hostname, t.adb_port))
            .unwrap_or_else(|| "Not configured".into())
    );
    println!("\n  Config saved to: {BOLD}{}{RESET}\n", config_path.display());

    println!("  {BOLD}Next steps:{RESET}");
    println!("    1. Review config:     {DIM}cat {}{RESET}", config_path.display());
    println!("    2. Health check:      {DIM}hermitdroid doctor{RESET}");
    println!("    3. Test (dry run):    {DIM}hermitdroid --dry-run{RESET}");
    println!("    4. Start the agent:   {DIM}hermitdroid{RESET}");

    if ts.as_ref().map_or(false, |t| t.enabled) {
        if let Ok(out) = Command::new("tailscale").args(["ip", "-4"]).output() {
            if out.status.success() {
                let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
                println!("\n  {BOLD}Remote access (Tailscale):{RESET}");
                println!("    Dashboard:  {DIM}http://{ip}:8420{RESET}");
                println!("    WebSocket:  {DIM}ws://{ip}:8420/ws/user{RESET}");
            }
        }
    }
    println!();
}

// ── Public entry point ──────────────────────────────────────────────────────

pub fn run_onboarding(config_path: &Path) -> anyhow::Result<()> {
    banner();

    // Check existing config
    if config_path.exists() {
        println!("  {YELLOW}⚠  Config already exists at {}{RESET}", config_path.display());
        if !prompt_yes_no("  Overwrite with new configuration?", false) {
            println!("  Keeping existing config.");
            return Ok(());
        }
    }

    let brain = step_ai_and_vision();
    let adb = step_adb();
    let adb_configured = adb.is_some();
    let ts = step_tailscale(adb_configured);

    let content = generate_config(&brain, &adb, &ts, config_path);

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(config_path, &content)?;
    println!("\n  {GREEN}✓  Config written to {}{RESET}", config_path.display());

    print_summary(&brain, &adb, &ts, config_path);
    Ok(())
}