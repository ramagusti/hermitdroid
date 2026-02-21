# 🤖 Hermitdroid

**OpenClaw-inspired autonomous Android AI agent.** Personal assistant that lives on your phone, sees your notifications, understands your screen, and acts on your behalf — with persistent identity, memory, goals, and skills.

Think of it as [OpenClaw](https://github.com/openclaw/openclaw), but purpose-built for Android device control.

## How It Works

```
Android Device (Companion App)
  │  Notifications, Screen, Events
  ▼
┌──────────────────────────────────┐
│        Hermitdroid Gateway       │
│     ws://127.0.0.1:8420           │
│                                   │
│  ┌─────────┐  ┌────────────────┐  │
│  │ Workspace│  │   LLM Brain    │  │
│  │ SOUL.md  │  │ AutoGLM-9B /   │  │
│  │ MEMORY.md│  │ Qwen-VL / any  │  │
│  │ GOALS.md │  │ Ollama model   │  │
│  │ skills/  │  └────────────────┘  │
│  └─────────┘                      │
│  💓 Heartbeat Loop (30s)          │
│  📋 Cron Jobs                     │
│  🔒 Action Guardrails             │
└──────────────────────────────────┘
  │  Tap, Swipe, Type, Launch
  ▼
Android Device (ADB / Accessibility)
```

## Quick Start

```bash
# 1. Build
cargo build --release

# 2. Set up your LLM (Ollama example)
ollama pull yeahdongcn/AutoGLM-Phone-9B

# 3. Run interactive setup
./target/release/hermitdroid onboard

# 4. Check workspace health
./target/release/hermitdroid doctor

# 5. Run (dry-run first!)
./target/release/hermitdroid --dry-run

# 6. Run for real
./target/release/hermitdroid
```

The `onboard` wizard walks you through choosing your AI provider, model, API key, vision preferences, ADB connection, and optional Tailscale remote access.

## CLI

```
hermitdroid                  # Start the gateway (default)
hermitdroid onboard          # Interactive setup wizard
hermitdroid doctor           # Check workspace health
hermitdroid status           # Show agent status
hermitdroid chat <message>   # Send a message to running agent
hermitdroid stop             # Pause agent
hermitdroid restart          # Restart agent
hermitdroid logs             # Follow agent logs
hermitdroid service install  # Install as systemd user service
hermitdroid service status   # Check service status
```

## 🌐 Tailscale — Remote Access

Control your phone from **anywhere** — not just USB or local Wi-Fi.

```
Your Laptop (anywhere)               Your Android Phone
┌─────────────────┐                  ┌──────────────────┐
│  Hermitdroid    │                  │  Tailscale App   │
│  Gateway        │                  │  ADB over TCP    │
│                 │◄── WireGuard ──►│  :5555           │
│  100.64.x.x    │    encrypted     │  100.64.y.y      │
└─────────────────┘                  └──────────────────┘
```

Tailscale creates a peer-to-peer mesh VPN using WireGuard. Both devices get stable `100.x.y.z` IPs that work from any network. All traffic is encrypted. No ports exposed to the public internet.

### Setup

1. **Install Tailscale on your computer:**
   ```bash
   curl -fsSL https://tailscale.com/install.sh | sh
   sudo tailscale up
   ```

2. **Install Tailscale on your Android phone:**
   - Download from Google Play Store or F-Droid
   - Sign in with the **same account** as your computer

3. **Enable ADB over TCP on phone** (USB connect once, then go wireless):
   ```bash
   adb tcpip 5555
   ```

4. **Find your phone's Tailscale hostname:**
   ```bash
   tailscale status
   # Look for your Android device, note its hostname or 100.x.y.z IP
   ```

5. **Run the setup wizard:**
   ```bash
   hermitdroid onboard
   # Choose your AI → set up ADB → enable Tailscale when prompted
   ```

   Or edit `config.toml` directly:
   ```toml
   [tailscale]
   enabled = true
   phone_hostname = "pixel-7"     # Your phone's Tailscale hostname
   adb_port = 5555
   auto_connect = true
   ```

6. **Verify:**
   ```bash
   hermitdroid doctor       # Shows Tailscale status, ping, connectivity
   hermitdroid status       # Shows remote dashboard URL
   ```

### Remote Dashboard

When Tailscale is enabled, the gateway binds on `0.0.0.0` so the dashboard is accessible from any device on your tailnet:

| Endpoint | URL |
| --- | --- |
| Dashboard | `http://<tailscale-ip>:8420` |
| Status API | `http://<tailscale-ip>:8420/status` |
| User WebSocket | `ws://<tailscale-ip>:8420/ws/user` |

### Auto-reconnect

Hermitdroid monitors the Tailscale connection and automatically reconnects:

```toml
[tailscale]
health_check_interval_secs = 60    # Check every 60s
max_failures_before_reconnect = 3  # Reconnect after 3 failures
```

### ADB TCP Persistence Across Reboots

ADB TCP mode (`adb tcpip 5555`) resets on phone reboot. To make it persistent:

- **Rooted phones:** Add `setprop service.adb.tcp.port 5555` to a boot script (Magisk, etc.)
- **Tasker/Automate:** Create a boot automation that runs the shell command after 30s delay
- **Some ROMs:** Have "Wireless debugging" in Developer Options that persists

### Troubleshooting

| Problem | Solution |
| --- | --- |
| "Could not resolve hostname" | Check Tailscale is running on phone, same tailnet |
| "TCP connection failed" | Run `adb tcpip 5555` again (may need USB reconnect) |
| "ADB: connection refused" | Phone may have rebooted; USB + `adb tcpip 5555` |
| High latency | Run `tailscale ping --verbose <phone>` — should say "direct" not "via DERP" |

## OpenClaw Concepts Adapted for Android

| OpenClaw | Hermitdroid | Purpose |
| --- | --- | --- |
| SOUL.md | ✅ SOUL.md | Agent personality & values |
| IDENTITY.md | ✅ IDENTITY.md | Name, emoji, tone |
| AGENTS.md | ✅ AGENTS.md | Runtime instructions |
| TOOLS.md | ✅ TOOLS.md | Available capabilities |
| USER.md | ✅ USER.md | User profile & preferences |
| HEARTBEAT.md | ✅ HEARTBEAT.md | Heartbeat contract |
| MEMORY.md | ✅ MEMORY.md | Long-term curated memory |
| memory/YYYY-MM-DD.md | ✅ Daily memory | Daily logs (auto-flushed) |
| GOALS.md | ✅ GOALS.md | Active goals & tasks |
| BOOTSTRAP.md | ✅ BOOTSTRAP.md | First-run setup ritual |
| skills/ | ✅ skills/ | Extensible skill system |
| Gateway WS | ✅ HTTP + WS server | Control plane |
| Channels (WhatsApp etc.) | Android Companion App | Device bridge |
| Cron jobs | ✅ Cron config | Scheduled tasks |
| Hooks | ✅ on\_boot, on\_unlock | Event-driven actions |
| /status, /new, /reset | ✅ Slash commands | Chat commands |
| Session management | ✅ Sessions | Conversation isolation |
| Doctor | ✅ `doctor` subcommand | Workspace health check |
| HEARTBEAT\_OK | ✅ Silent drop | Don't waste tokens on idle |
| Priority apps | ✅ Interrupt sleep | Immediate ticks for important notifs |
| Restricted apps | ✅ Force RED | Banking/finance always need confirmation |

## API

| Endpoint | Method | Description |
| --- | --- | --- |
| `/status` | GET | Agent status |
| `/start` / `/stop` | POST | Control agent |
| `/workspace/{file}` | GET/POST | Read/write any workspace file |
| `/memory` | GET/POST | Long-term memory |
| `/memory/daily` | GET | Recent daily logs |
| `/goals` | GET/POST | Goals |
| `/goals/{id}/complete` | POST | Complete a goal |
| `/sessions` | GET | List sessions |
| `/sessions/{id}/new` | POST | Reset session |
| `/pending` | GET | Pending RED actions |
| `/confirm/{id}` | POST | Approve/deny RED action |
| `/actions/log` | GET | Action audit log |
| `/chat` | POST | Send message (supports /slash commands) |
| `/ws/android` | WS | Companion app bridge |
| `/ws/user` | WS | Real-time user dashboard |
| `/tailscale/status` | GET | Tailscale connection status & peers |
| `/tailscale/connect` | POST | Reconnect ADB via Tailscale |
| `/tailscale/disconnect` | POST | Disconnect Tailscale ADB |
| `/tailscale/peers` | GET | List Android devices on tailnet |

### Slash Commands (via /chat)

| Command | Action |
| --- | --- |
| `/status` | Show agent status |
| `/new` / `/reset` | Reset main session |
| `/stop` | Pause agent |
| `/start` | Resume agent |
| `/goal <text>` | Add a goal |
| `/goals` | Show all goals |
| `/memory` | Show long-term memory |
| `/soul` | Show current SOUL.md |

## Workspace

```
workspace/
├── SOUL.md          # Who the agent is (philosophy, values, boundaries)
├── IDENTITY.md      # How it presents itself (name, emoji, tone)
├── AGENTS.md        # Runtime instructions (action format, available tools)
├── TOOLS.md         # What it can do
├── USER.md          # About the user (you fill this in)
├── HEARTBEAT.md     # Heartbeat contract
├── MEMORY.md        # Long-term curated memory
├── GOALS.md         # Active goals & tasks
├── BOOTSTRAP.md     # First-run ritual (deleted after setup)
├── memory/          # Daily memory logs (YYYY-MM-DD.md)
├── skills/          # Installed skills
│   └── notification-summarizer/
│       └── SKILL.md
└── canvas/          # (future) visual workspace files
```

All files are plain markdown. Edit with any text editor. Back up with git.

## Choosing a Model

```toml
[brain]
backend = "ollama"
model = "yeahdongcn/AutoGLM-Phone-9B"  # Vision + phone UI specialist
# model = "qwen2.5-vl:7b"               # Strong vision + reasoning
# model = "llama3.1:8b"                  # Text-only (fast, no vision)
# model = "phi3.5:latest"                # Lightweight
vision_enabled = true

# For OpenAI-compatible servers (vLLM, LM Studio):
# backend = "openai_compatible"
# endpoint = "http://localhost:8000/v1"

# For cloud providers:
# backend = "openai"
# model = "gpt-4o"
# endpoint = "https://api.openai.com/v1"
# api_key = "sk-..."   # or set HERMITDROID_API_KEY env var
```

Run `hermitdroid onboard` to configure interactively.

## Safety Model

The agent classifies every action before execution:

* **🟢 GREEN** — Read-only (observe, log). Silent auto-execute.
* **🟡 YELLOW** — Reversible (open app, scroll). Auto-execute, user notified.
* **🔴 RED** — Irreversible (send message, delete, pay). **Always** requires user confirmation.

Additional safety:

* `restricted_apps` in config force RED classification regardless of action type
* `priority_apps` trigger immediate ticks (don't wait for next heartbeat)
* Kill switch: POST `/stop`, or send "stop everything" via chat/WS
* All data stays local. No external API calls except to your configured LLM.
* Full action audit log at `/actions/log`

## License

MIT

## Acknowledgments

- [OpenClaw](https://github.com/openclaw/openclaw) — inspiration for the architecture
- [scrcpy](https://github.com/Genymobile/scrcpy) — ADB screen mirroring reference
- [Ollama](https://ollama.com) — local LLM runtime