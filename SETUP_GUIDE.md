# Hermitdroid — ADB Mode Setup Guide

No companion app needed. Just your phone, a computer, and a USB cable (or WiFi ADB).

---

## What You Need

| Thing | Why |
|-------|-----|
| Android phone (Android 9+) | The device the agent controls |
| Computer (Linux/macOS/Windows WSL2) | Runs the agent server + LLM |
| USB cable or same WiFi network | Connects phone to computer via ADB |
| ~8GB RAM on computer | For running a local LLM |

---

## Quick Start (5 commands)

```bash
# 1. Run the install script
chmod +x install.sh && ./install.sh

# 2. Connect your phone
adb devices  # should show your device

# 3. Edit your config
nano config.toml  # set your model, preferences

# 4. Test (dry run, no real actions)
./target/release/hermitdroid --dry-run

# 5. Run for real
./target/release/hermitdroid
```

---

## Step-by-Step

### 1. Install Everything

Run the install script (see `install.sh` in the project):

```bash
./install.sh
```

This installs Rust, Ollama, pulls a model, and builds the project.

Or do it manually:

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Ollama
curl -fsSL https://ollama.com/install.sh | sh

# Pull a model (pick one)
ollama pull yeahdongcn/AutoGLM-Phone-9B   # recommended: built for phone UI, ~6GB
# ollama pull qwen2.5-vl:7b              # vision + reasoning, ~5GB
# ollama pull llama3.1:8b                # text-only, fast, ~5GB
# ollama pull phi3.5:latest    # lightweight, ~3GB

# Build
cd hermitdroid
cargo build --release
```

### 2. Connect Your Phone via ADB

#### Option A: USB (easiest)

1. On phone: **Settings → About Phone → tap "Build Number" 7 times** to enable Developer Mode
2. **Settings → Developer Options → enable "USB Debugging"**
3. Plug in USB cable
4. On phone: tap "Allow USB debugging" when prompted
5. Verify:

```bash
adb devices
# Should show:
# List of devices attached
# XXXXXXXX    device
```

#### Option B: WiFi ADB (no cable after initial setup)

```bash
# Initial setup (needs USB once)
adb tcpip 5555
adb connect <phone-ip>:5555

# Find your phone's IP:
# Settings → WiFi → tap your network → IP address
# Usually something like 192.168.1.XXX

# Verify
adb devices
# Should show:
# 192.168.1.XXX:5555    device
```

If you have multiple devices, set the device in `config.toml`:
```toml
[perception]
adb_device = "192.168.1.XXX:5555"
```

#### Quick test — can ADB read notifications?

```bash
adb shell dumpsys notification --noredact | head -50
```

You should see notification records. If you see output, you're good.

### 3. Configure

Edit `config.toml`:

```toml
[agent]
name = "Hermitdroid"
heartbeat_interval_secs = 30         # how often the agent "thinks"
workspace_path = "./workspace"

[brain]
backend = "ollama"
model = "yeahdongcn/AutoGLM-Phone-9B"   # purpose-built for phone control
endpoint = "http://localhost:11434"
vision_enabled = false                # AutoGLM is text-based (uses UI tree, not screenshots)

[perception]
bridge_mode = "adb"                  # no companion app needed
# adb_device = "192.168.1.XXX:5555" # uncomment if WiFi ADB
priority_apps = ["whatsapp", "telegram", "gmail", "calendar"]

[action]
dry_run = false
restricted_apps = ["banking", "dana", "gopay", "ovo", "wallet"]

[server]
host = "0.0.0.0"
port = 8420
```

### 4. Fill In Your Profile

Edit `workspace/USER.md` with your info:

```markdown
## Name
Andi

## Location
Jakarta, GMT+7

## Language
Bahasa Indonesia for casual, English for work

## Preferences
- Don't disturb me 11pm-6am
- Summarize WhatsApp group chats
- Remind me about meetings 15 minutes before

## Apps I Use Most
WhatsApp, Telegram, Gmail, Google Calendar, Grab, Tokopedia
```

### 5. Health Check

```bash
./target/release/hermitdroid doctor --config config.toml
```

Should show all green checks. Fix anything marked ❌.

### 6. Dry Run First

```bash
./target/release/hermitdroid --config config.toml --dry-run
```

This runs the full heartbeat loop but **logs actions instead of executing them**. Watch the output to see what the agent would do.

Send a test notification on your phone and watch the terminal — you should see it picked up.

### 7. Run For Real

```bash
./target/release/hermitdroid --config config.toml
```

Output:
```
🤖 Hermitdroid v0.1.0
Agent: Hermitdroid | Model: yeahdongcn/AutoGLM-Phone-9B | Backend: ollama
📡 Bridge mode: adb
✅ ADB: 1 device(s) connected
🌐 Server: http://0.0.0.0:8420
💓 Heartbeat: 30s tick, 1800s gateway
```

### 8. First-Run Bootstrap

On the first run, `BOOTSTRAP.md` is detected. Start the setup ritual:

```bash
curl -X POST http://localhost:8420/chat \
  -H "Content-Type: application/json" \
  -d '{"message": "Hey, lets get you set up."}'
```

The agent will ask you questions to set up its personality, your preferences, and your first goals.

---

## How It Works (ADB Mode)

Every 30 seconds (configurable), the agent:

```
1. adb shell dumpsys notification --noredact
   → Parse all notifications, find NEW ones since last check

2. adb shell dumpsys activity activities
   → Find what app/screen is currently open

3. adb shell uiautomator dump /dev/tty
   → Get the full UI tree (buttons, text fields, labels with coordinates)

4. Send all this context + workspace files (SOUL, MEMORY, GOALS) to the LLM

5. LLM responds with:
   - Actions to take (tap, swipe, type, launch app)
   - Memory to write
   - Message to show user

6. Execute actions via ADB:
   - adb shell input tap X Y
   - adb shell input swipe X1 Y1 X2 Y2
   - adb shell input text "hello"
   - adb shell monkey -p com.whatsapp ... (launch app)

7. Write to memory, notify user, loop.
```

---

## Talking to the Agent

### Via curl

```bash
# Send a message
curl -X POST http://localhost:8420/chat \
  -H "Content-Type: application/json" \
  -d '{"message": "what notifications did I miss?"}'

# Slash commands
curl -X POST http://localhost:8420/chat -d '{"message": "/status"}'
curl -X POST http://localhost:8420/chat -d '{"message": "/goal buy groceries tomorrow"}'
curl -X POST http://localhost:8420/chat -d '{"message": "/goals"}'
curl -X POST http://localhost:8420/chat -d '{"message": "/memory"}'
curl -X POST http://localhost:8420/chat -d '{"message": "/stop"}'
```

### Approve actions

When the agent wants to do something dangerous (send a message, delete something), it queues it for your approval:

```bash
# See pending actions
curl http://localhost:8420/pending

# Approve
curl -X POST http://localhost:8420/confirm/ACTION_ID \
  -d '{"approved": true}'

# Deny
curl -X POST http://localhost:8420/confirm/ACTION_ID \
  -d '{"approved": false}'
```

### WebSocket (real-time)

Connect any WebSocket client to `ws://localhost:8420/ws/user` for live events:

```
wscat -c ws://localhost:8420/ws/user
```

You'll see agent messages, action results, and notification events in real time. You can also type messages directly.

---

## Run as Background Service

### Linux (systemd)

```bash
# Create service
sudo tee /etc/systemd/system/hermitdroid.service << EOF
[Unit]
Description=Hermitdroid Agent
After=network.target

[Service]
Type=simple
User=$USER
WorkingDirectory=$(pwd)
ExecStart=$(pwd)/target/release/hermitdroid --config config.toml
Restart=always
RestartSec=10
Environment=RUST_LOG=hermitdroid=info

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable hermitdroid
sudo systemctl start hermitdroid

# View logs
journalctl -u hermitdroid -f
```

### macOS (launchd)

```bash
cat > ~/Library/LaunchAgents/com.androidsoul.agent.plist << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.androidsoul.agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>$(pwd)/target/release/hermitdroid</string>
        <string>--config</string>
        <string>$(pwd)/config.toml</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>WorkingDirectory</key>
    <string>$(pwd)</string>
</dict>
</plist>
EOF

launchctl load ~/Library/LaunchAgents/com.androidsoul.agent.plist
```

---

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `adb: command not found` | Install Android SDK platform-tools: `sudo apt install adb` or download from developer.android.com |
| `adb devices` shows nothing | Enable USB Debugging on phone. Try different USB cable. |
| `adb devices` shows "unauthorized" | Tap "Allow USB debugging" on phone screen |
| WiFi ADB keeps disconnecting | Phone went to sleep. Run `adb tcpip 5555` again via USB. Some phones need "Stay awake" in Developer Options. |
| `dumpsys notification` is empty | Some ROMs restrict this. Try: `adb shell cmd notification list` as alternative |
| Ollama not responding | Check `ollama serve` is running. Check `curl http://localhost:11434/api/tags` |
| Agent uses too many tokens | Increase `heartbeat_interval_secs` to 60 or 120. The idle-skip logic already saves tokens when nothing happens. |
| Actions don't work | Check `dry_run` is `false` in config. Check ADB is connected. |
| Agent too aggressive | Edit `workspace/SOUL.md` — add "Only act when explicitly asked or when something is clearly urgent" |
| Want to see what agent sees | `curl http://localhost:8420/status` and check the action log at `curl http://localhost:8420/actions/log` |

---

## API Reference (Quick)

| Endpoint | Method | What |
|----------|--------|------|
| `/status` | GET | Agent status, pending actions |
| `/start` | POST | Start agent |
| `/stop` | POST | Stop agent |
| `/chat` | POST | Send message or /command |
| `/workspace/{file}` | GET/POST | Read/write workspace files |
| `/memory` | GET/POST | Long-term memory |
| `/memory/daily` | GET | Last 7 days of daily logs |
| `/goals` | GET/POST | Goals |
| `/goals/{id}/complete` | POST | Complete a goal |
| `/pending` | GET | Pending RED actions |
| `/confirm/{id}` | POST | Approve/deny action |
| `/actions/log` | GET | Full action audit trail |
| `/sessions` | GET | List sessions |
| `/ws/user` | WS | Real-time event stream |
| `/ws/android` | WS | Companion app (if using WebSocket mode) |
