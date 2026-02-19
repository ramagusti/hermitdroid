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

## OpenClaw Concepts Adapted for Android

| OpenClaw | Hermitdroid | Purpose |
|----------|-------------|---------|
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
| Hooks | ✅ on_boot, on_unlock | Event-driven actions |
| /status, /new, /reset | ✅ Slash commands | Chat commands |
| Session management | ✅ Sessions | Conversation isolation |
| Doctor | ✅ `doctor` subcommand | Workspace health check |
| HEARTBEAT_OK | ✅ Silent drop | Don't waste tokens on idle |
| Priority apps | ✅ Interrupt sleep | Immediate ticks for important notifs |
| Restricted apps | ✅ Force RED | Banking/finance always need confirmation |

## Quick Start

```bash
# 1. Build
cargo build --release

# 2. Set up your LLM (Ollama example)
ollama pull qwen2.5-vl:7b   # or any supported model

# 3. Edit config
vim config.toml

# 4. Check workspace health
./target/release/hermitdroid doctor

# 5. Run (dry-run first!)
./target/release/hermitdroid --dry-run

# 6. Run for real
./target/release/hermitdroid
```

## CLI (OpenClaw-style)

```bash
hermitdroid gateway          # Start the gateway (default)
hermitdroid doctor           # Check workspace health
hermitdroid status           # Show config summary
hermitdroid chat -m "..."    # Send a message to running agent
```

## API

| Endpoint | Method | Description |
|----------|--------|-------------|
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

### Slash Commands (via /chat)

| Command | Action |
|---------|--------|
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
model = "zai-org/AutoGLM-Phone-9B-Multilingual"  # Vision + phone UI specialist
# model = "qwen2.5-vl:7b"                         # Strong vision + reasoning
# model = "llama3.1:8b"                            # Text-only (fast, no vision)
# model = "phi3.5:latest"                          # Lightweight
vision_enabled = true

# For OpenAI-compatible servers (vLLM, LM Studio):
# backend = "openai_compatible"
# endpoint = "http://localhost:8000/v1"
```

## Safety Model

The agent classifies every action before execution:

- **🟢 GREEN** — Read-only (observe, log). Silent auto-execute.
- **🟡 YELLOW** — Reversible (open app, scroll). Auto-execute, user notified.
- **🔴 RED** — Irreversible (send message, delete, pay). **Always** requires user confirmation.

Additional safety:
- `restricted_apps` in config force RED classification regardless of action type
- `priority_apps` trigger immediate ticks (don't wait for next heartbeat)
- Kill switch: POST `/stop`, or send "stop everything" via chat/WS
- All data stays local. No external API calls except to your configured LLM.
- Full action audit log at `/actions/log`

## License

MIT
