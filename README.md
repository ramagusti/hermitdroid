# 🤖 Hermitdroid

**An autonomous AI agent that lives on your Android phone.**

Hermitdroid watches your notifications, reads your screen, and takes actions on your behalf — replying to messages, managing apps, and running tasks. It connects to your phone via ADB and thinks using any LLM (local or cloud).

Inspired by [OpenClaw](https://github.com/openclaw/openclaw), but designed specifically for Android and built in Rust for minimal resource usage.

```
┌─────────────┐     ADB      ┌──────────────┐
│  Your Phone │◄────────────►│  Hermitdroid  │
│  (Android)  │  USB / WiFi  │  (your PC)    │
└─────────────┘              └──────┬───────┘
                                    │
                              ┌─────▼─────┐
                              │   LLM     │
                              │ Ollama /  │
                              │ ChatGPT / │
                              │ any API   │
                              └───────────┘
```

## Quick Start

### One-line install

```bash
curl -fsSL https://raw.githubusercontent.com/ramagusti/hermitdroid/main/install.sh | bash
```

This will:
- Install dependencies (Rust 1.85+, git, zstd)
- Clone the repo to `~/.hermitdroid/`
- Build the release binary
- Symlink `hermitdroid` to `~/.local/bin/`

### Manual install

```bash
git clone https://github.com/ramagusti/hermitdroid.git ~/.hermitdroid
cd ~/.hermitdroid
make
```

That's it. `make` builds the binary and installs everything to `~/.hermitdroid/`.

Make sure `~/.local/bin` is in your PATH:

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

## Requirements

- **Rust 1.85+** (installed automatically by `install.sh`)
- **ADB** (Android Debug Bridge) — `sudo apt install adb` or [download from Google](https://developer.android.com/tools/releases/platform-tools)
- **Android phone** with USB Debugging enabled
- **LLM** — one of:
  - [Ollama](https://ollama.com) (free, local) — best for low-RAM systems
  - ChatGPT subscription via Codex OAuth (free with Plus/Pro)
  - Any OpenAI-compatible API

### WSL2 Users (Windows)

ADB can't access USB devices directly in WSL. Use [usbipd-win](https://github.com/dorssel/usbipd-win):

```powershell
# In PowerShell (admin):
winget install usbipd
usbipd list                          # find your phone's BUSID
usbipd bind --busid <BUSID>
usbipd attach --wsl --busid <BUSID>
```

Then in WSL: `adb devices` should show your phone.

## Configuration

Edit `~/.hermitdroid/config.toml`:

### Option A: Local LLM (Ollama) — free, no account needed

```toml
[brain]
backend = "ollama"
model = "qwen2.5:3b"          # or any model you've pulled
endpoint = "http://localhost:11434"
vision_enabled = false
```

```bash
# Install Ollama and pull a model:
curl -fsSL https://ollama.com/install.sh | sh
ollama pull qwen2.5:3b
```

### Option B: ChatGPT subscription (Codex OAuth) — uses your existing Plus/Pro plan

```toml
[brain]
backend = "codex_oauth"
model = "gpt-4o"               # or "gpt-4o-mini", "gpt-5", etc.
vision_enabled = true
```

```bash
# Install Codex CLI and login with your ChatGPT account:
npm install -g @openai/codex
codex login                    # opens browser, authenticates with ChatGPT
```

This reads your token from `~/.codex/auth.json` automatically. No API key needed — it bills to your ChatGPT subscription.

### Option C: OpenAI API key (pay-per-token)

```toml
[brain]
backend = "ollama"
model = "gpt-4o"
endpoint = "https://api.openai.com/v1"
api_key = "sk-..."
vision_enabled = true
```

### Priority apps

Tell Hermitdroid which apps matter to you:

```toml
[perception]
priority_apps = ["com.whatsapp", "com.google.android.gm", "org.telegram.messenger"]
poll_interval_secs = 10
```

## Usage

```bash
# Check everything is set up correctly:
hermitdroid doctor

# Test run (won't take any actions):
hermitdroid --dry-run

# Start the agent:
hermitdroid
```

The agent runs a heartbeat loop:
1. Polls notifications via ADB
2. Reads screen state via UI tree
3. Sends context to LLM
4. Executes actions (tap, type, swipe, open app)
5. Writes to long-term memory

### Web dashboard

Hermitdroid starts a local web server (default port 8420):

```
http://localhost:8420
```

Send commands, view logs, and monitor the agent from your browser.

## Workspace

The agent's personality and behavior are defined by markdown files in `~/.hermitdroid/workspace/`:

| File | Purpose |
|------|---------|
| `SOUL.md` | Core personality and values |
| `IDENTITY.md` | Who the agent is |
| `AGENTS.md` | Multi-agent routing rules |
| `TOOLS.md` | Available tools and how to use them |
| `USER.md` | Info about you (the user) |
| `HEARTBEAT.md` | What to check on each tick |
| `GOALS.md` | Current active goals |
| `MEMORY.md` | Long-term memory (auto-updated) |

Edit these to customize your agent's behavior.

## Updating

```bash
cd ~/.hermitdroid
git pull
make
```

## Project Structure

```
~/.hermitdroid/
├── hermitdroid          # binary
├── config.toml          # your configuration
├── workspace/           # agent personality & memory
│   ├── SOUL.md
│   ├── IDENTITY.md
│   ├── GOALS.md
│   ├── MEMORY.md
│   └── ...
├── src/                 # source code
│   ├── brain/           # LLM backends (Ollama, Codex OAuth, OpenAI)
│   ├── perception/      # ADB bridge, notifications, screen reading
│   ├── server/          # web dashboard
│   ├── soul/            # workspace loader
│   └── config/          # configuration
├── Makefile             # build + install
├── Cargo.toml
└── install.sh
```

## Troubleshooting

**`adb: device unauthorized`** — Approve the USB debugging prompt on your phone.

**`Ollama error: model too large`** — Switch to a smaller model: `ollama pull qwen2.5:3b`

**`Codex OAuth: no token found`** — Run `codex login` to authenticate.

**`Codex OAuth: authentication failed (401)`** — Token expired. Run `codex login` again. Codex CLI auto-refreshes tokens, but if it's been a while since you used it, re-login.

**`insufficient_quota` with OpenAI API** — You need API credits at [platform.openai.com](https://platform.openai.com). Or switch to `codex_oauth` to use your ChatGPT subscription instead.

**Build fails with `edition2024`** — Update Rust: `rustup update stable` (need 1.85+).

## How It Works

Hermitdroid is a **headless Android agent**. It doesn't run on the phone — it runs on your PC/server and controls the phone over ADB.

The heartbeat loop runs every N seconds:
1. **Perceive** — Read notifications (`dumpsys notification`) and screen state (`uiautomator dump`)
2. **Think** — Send the context to an LLM with the system prompt from your workspace
3. **Act** — Execute the LLM's response: tap coordinates, type text, swipe, open apps, or write to memory
4. **Remember** — Update `MEMORY.md` and `GOALS.md` based on what happened

The LLM sees your workspace files as its "soul" and makes decisions based on your configured personality, goals, and the current state of your phone.

## License

MIT

## Acknowledgments

- [OpenClaw](https://github.com/openclaw/openclaw) — inspiration for the architecture
- [scrcpy](https://github.com/Genymobile/scrcpy) — ADB screen mirroring reference
- [Ollama](https://ollama.com) — local LLM runtime