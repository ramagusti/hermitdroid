# 🤖 Hermitdroid

OpenClaw-inspired autonomous Android AI agent, built in Rust.
Give it a goal → it reads the screen → taps and types via ADB → repeats until done.

```
$ hermitdroid run "open youtube and search for lofi hip hop"

🤖 Hermitdroid — One-Shot Mode

  Goal: open youtube and search for lofi hip hop
  Model: gemini-2.5-flash via openai_compatible
  Max steps: 30 | Vision: on

  [1/30] 🧠 Home screen detected. Launching YouTube.
         🟡 ▸ launch com.google.android.youtube (823ms)
  [2/30] 🧠 YouTube open. Tapping search icon.
         🟢 ▸ tap @(645,98) search icon (312ms)
  [3/30] 🟢 ▸ type "lofi hip hop" (189ms)
  [4/30] 🟢 ▸ key ENTER (95ms)
  [5/30] ✅ Done — search results showing

  Total: 1.4s (4 actions)
```

### Why Hermitdroid?

| | Hermitdroid | Droidclaw | OpenClaw |
|---|---|---|---|
| Language | Rust (fast, single binary) | TypeScript/Bun | TypeScript/Node |
| Cold start | < 1s | ~3s | ~5s |
| Memory/Soul | ✅ Persistent identity | ❌ Stateless | ✅ Full workspace |
| Safety | GREEN/YELLOW/RED classification | None | Channel-level |
| Deterministic flows | ✅ YAML (no LLM) | ✅ YAML | ❌ |
| Remote access | ✅ Built-in Tailscale | Brief mention | Tailscale Serve/Funnel |
| Platform | Android only | Android only | Multi-platform |

## Quick Start

```bash
# Install (pre-built binary)
curl -fsSL https://hermitdroid.dev/install.sh | sh

# Or build from source
cargo build --release

# Setup wizard (picks AI provider, connects phone)
hermitdroid onboard

# Run a one-shot goal
hermitdroid run "open settings and turn on wifi"

# Run a workflow (AI-powered, multi-step)
hermitdroid workflow examples/workflows/messaging/slack-standup.json

# Run a deterministic flow (no AI, instant)
hermitdroid flow examples/flows/clear-notifications.yaml

# Start persistent agent mode (OpenClaw-style)
hermitdroid
```

## Three Ways to Automate

### 1. One-Shot Goals (`hermitdroid run`)

Give it a goal in plain English. The agent reads the screen, thinks, acts, and repeats until done.

```bash
hermitdroid run "open whatsapp and send hi to Mom"
hermitdroid run --verbose "check my gmail"
hermitdroid run --max-steps 20 "open settings and check wifi status"
```

Save a successful goal as a reusable workflow:

```bash
hermitdroid run "open youtube and search cats" --save-as youtube-cats
# → Saves to workspace/workflows/youtube-cats.json

hermitdroid workflow workspace/workflows/youtube-cats.json
# → Re-run anytime
```

### 2. AI Workflows (`hermitdroid workflow`)

Multi-step, AI-powered. JSON files with natural language goals. The LLM figures out what to tap, type, and swipe. Great for complex tasks across multiple apps.

```bash
hermitdroid workflow examples/workflows/productivity/morning-briefing.json
```

```json
{
  "name": "Morning Briefing",
  "description": "Check weather, calendar, and messages across apps",
  "steps": [
    {
      "app": "com.google.android.googlequicksearchbox",
      "goal": "Search for today's weather and note the temperature"
    },
    {
      "app": "com.google.android.calendar",
      "goal": "Check today's calendar events and note upcoming meetings"
    },
    {
      "app": "com.whatsapp",
      "goal": "Check for any unread messages and note who sent them"
    }
  ]
}
```

You can inject specific data via `form_data`:

```json
{
  "name": "Slack Standup",
  "steps": [
    {
      "app": "com.Slack",
      "goal": "Open #standup channel, type the message and send it",
      "form_data": {
        "message": "yesterday: api integration\ntoday: tests\nblockers: none"
      }
    }
  ]
}
```

### 3. Deterministic Flows (`hermitdroid flow`)

Fixed sequence of ADB actions. **No LLM calls — instant execution.** Pure Rust speed. For tasks you do exactly the same way every time.

```bash
hermitdroid flow examples/flows/clear-notifications.yaml
```

```yaml
name: Clear All Notifications
description: Pull down notification shade and clear everything
---
- swipe: [540, 50, 540, 800]
- wait: 1.5
- tap_text: "Clear all"
- wait: 0.5
- done: "Notifications cleared"
```

Flow actions: `tap: [x,y]`, `tap_text: "text"`, `type: "text"`, `swipe: [x1,y1,x2,y2]`, `key: ENTER`, `wait: 2`, `back`, `home`, `screenshot`, `launch: com.app.id`, `done: "message"`.

### Quick Comparison

|  | `hermitdroid run` | `hermitdroid workflow` | `hermitdroid flow` |
|---|---|---|---|
| Format | CLI argument | JSON file | YAML file |
| Uses AI | Yes | Yes (per step) | No |
| Multi-step | No | Yes | Yes |
| Handles UI changes | Yes | Yes | No |
| Speed | LLM-dependent | LLM-dependent | Instant |
| Best for | One-off tasks | Complex multi-app | Simple repeatable tasks |
| Saveable | `--save-as` | Already a file | Already a file |

### Example Library

**17 workflow examples** across 5 categories:

**[messaging/](examples/workflows/messaging)** — whatsapp-send, whatsapp-broadcast, slack-standup, telegram-send, email-reply, email-digest

**[productivity/](examples/workflows/productivity)** — morning-briefing, notification-cleanup, calendar-check, github-check-prs

**[research/](examples/workflows/research)** — weather-report, google-search, price-comparison

**[lifestyle/](examples/workflows/lifestyle)** — spotify-play, maps-commute, food-order

**[social/](examples/workflows/social)** — youtube-search

**6 deterministic flows** in [`examples/flows/`](examples/flows): clear-notifications, toggle-wifi, screenshot, go-home, lock-screen, recent-apps

## Persistent Memory

Hermitdroid remembers across sessions. Unlike stateless task runners, it learns your patterns:

```
# workspace/memory/2025-06-15.md
- 09:15 User asked to check Gmail. Found 3 unread from boss@work.com.
- 09:20 User replied to Q3 review email.
- 12:30 WhatsApp notification from Mom — "dinner at 7?"
- 12:31 User confirmed dinner. Sent "See you at 7!"
- 18:00 Evening briefing: 2 missed calls, 5 new emails, calendar clear tomorrow.
```

The agent uses this history to get smarter over time — it knows your contacts, your routines, and your preferences.

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

## CLI

```
hermitdroid                              # Start persistent agent (default)
hermitdroid run "goal"                   # One-shot goal runner
hermitdroid run "goal" --save-as name    # Run and save as reusable workflow
hermitdroid workflow path.json           # Run AI workflow
hermitdroid workflow path.json --verbose # Run with LLM thinking shown
hermitdroid flow path.yaml              # Run deterministic flow (no AI)
hermitdroid workflows                    # List available workflows & flows
hermitdroid onboard                      # Interactive setup wizard
hermitdroid doctor                       # Check workspace health
hermitdroid status                       # Show agent status
hermitdroid chat <message>               # Send message to running agent
hermitdroid stop                         # Pause agent
hermitdroid restart                      # Restart agent
hermitdroid logs                         # Follow agent logs
hermitdroid service install              # Install as systemd user service
hermitdroid service status               # Check service status
```

## Choosing a Model

```toml
[brain]
backend = "ollama"
model = "yeahdongcn/AutoGLM-Phone-9B"  # Vision + phone UI specialist
# model = "qwen2.5-vl:7b"               # Strong vision + reasoning
# model = "llama3.1:8b"                  # Text-only (fast, no vision)
vision_enabled = true

# For cloud providers (OpenAI, Gemini, Groq, OpenRouter):
# backend = "openai_compatible"
# model = "gemini-2.5-flash"
# endpoint = "https://generativelanguage.googleapis.com/v1beta/openai"
# api_key = "..."
```

Run `hermitdroid onboard` to configure interactively.

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

### Setup

1. Install Tailscale on your computer and Android phone (same account)
2. Enable ADB over TCP: `adb tcpip 5555`
3. Run `hermitdroid onboard` and enable Tailscale when prompted

Or edit `config.toml` directly:

```toml
[tailscale]
enabled = true
phone_hostname = "pixel-7"
adb_port = 5555
auto_connect = true
```

Verify: `hermitdroid doctor` shows Tailscale status, ping, connectivity.

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
| `/tailscale/status` | GET | Tailscale connection status |
| `/tailscale/connect` | POST | Reconnect ADB via Tailscale |

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
├── workflows/       # Saved workflows (from --save-as)
├── skills/          # Installed skills
│   └── notification-summarizer/
│       └── SKILL.md
└── canvas/          # (future) visual workspace files
```

All files are plain markdown or JSON/YAML. Edit with any text editor. Back up with git.

## License

MIT

## Acknowledgments

- [OpenClaw](https://github.com/openclaw/openclaw) — inspiration for the architecture
- [scrcpy](https://github.com/Genymobile/scrcpy) — ADB screen mirroring reference
- [Ollama](https://ollama.com) — local LLM runtime