#!/usr/bin/env bash
set -euo pipefail

# ============================================================
# Hermitdroid — Install Script
# Sets up Rust, Ollama, pulls a model, builds the project.
# Run: chmod +x install.sh && ./install.sh
# ============================================================

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}   $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
fail()  { echo -e "${RED}[FAIL]${NC} $*"; }

echo ""
echo -e "${CYAN}🤖 Hermitdroid — Installer${NC}"
echo "================================================"
echo ""

# ---- Detect OS ----
OS="$(uname -s)"
case "$OS" in
    Linux*)  PLATFORM=linux ;;
    Darwin*) PLATFORM=macos ;;
    *)       fail "Unsupported OS: $OS (need Linux or macOS)"; exit 1 ;;
esac
info "Platform: $PLATFORM"

# ---- Check / Install Rust ----
if command -v rustc &>/dev/null; then
    ok "Rust $(rustc --version | awk '{print $2}') already installed"
else
    info "Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    ok "Rust installed: $(rustc --version)"
fi

# ---- Check / Install ADB ----
if command -v adb &>/dev/null; then
    ok "ADB already installed"
else
    warn "ADB not found. Installing..."
    if [ "$PLATFORM" = "linux" ]; then
        if command -v apt &>/dev/null; then
            sudo apt update && sudo apt install -y android-tools-adb
        elif command -v dnf &>/dev/null; then
            sudo dnf install -y android-tools
        elif command -v pacman &>/dev/null; then
            sudo pacman -S --noconfirm android-tools
        else
            fail "Can't auto-install ADB. Install Android SDK platform-tools manually."
            fail "Download: https://developer.android.com/studio/releases/platform-tools"
        fi
    elif [ "$PLATFORM" = "macos" ]; then
        if command -v brew &>/dev/null; then
            brew install android-platform-tools
        else
            fail "Install Homebrew first (https://brew.sh) or download platform-tools manually."
        fi
    fi

    if command -v adb &>/dev/null; then
        ok "ADB installed"
    else
        warn "ADB installation may have failed. You can install it later."
    fi
fi

# ---- Check / Install Ollama ----
if command -v ollama &>/dev/null; then
    ok "Ollama already installed"
else
    info "Installing Ollama..."
    curl -fsSL https://ollama.com/install.sh | sh
    ok "Ollama installed"
fi

# ---- Start Ollama if not running ----
if curl -s http://localhost:11434/api/tags &>/dev/null; then
    ok "Ollama is running"
else
    info "Starting Ollama..."
    ollama serve &>/dev/null &
    sleep 3
    if curl -s http://localhost:11434/api/tags &>/dev/null; then
        ok "Ollama started"
    else
        warn "Could not start Ollama. Start it manually: ollama serve"
    fi
fi

# ---- Model Selection ----
echo ""
echo -e "${CYAN}Choose a model:${NC}"
echo "  1) yeahdongcn/AutoGLM-Phone-9B  — Built for phone UI control, ~6GB (recommended)"
echo "  2) qwen2.5-vl:7b                — Vision + reasoning, ~5GB"
echo "  3) llama3.1:8b                   — Text-only, fast, ~5GB"
echo "  4) phi3.5:latest                 — Lightweight, ~3GB (for weaker hardware)"
echo "  5) Skip (I'll pull my own model later)"
echo ""
read -rp "Pick [1-5, default=1]: " MODEL_CHOICE
MODEL_CHOICE="${MODEL_CHOICE:-1}"

case "$MODEL_CHOICE" in
    1) MODEL="yeahdongcn/AutoGLM-Phone-9B"; VISION=false ;;
    2) MODEL="qwen2.5-vl:7b";               VISION=true  ;;
    3) MODEL="llama3.1:8b";                  VISION=false ;;
    4) MODEL="phi3.5:latest";                VISION=false ;;
    5) MODEL=""; VISION=false ;;
    *) MODEL="yeahdongcn/AutoGLM-Phone-9B"; VISION=false ;;
esac

if [ -n "$MODEL" ]; then
    info "Pulling $MODEL (this may take a few minutes)..."
    ollama pull "$MODEL"
    ok "Model ready: $MODEL"
fi

# ---- Build ----
echo ""
info "Building Hermitdroid..."
cargo build --release 2>&1 | tail -5
ok "Build complete: ./target/release/hermitdroid"

# ---- Update config.toml with chosen model ----
if [ -n "$MODEL" ]; then
    # Update model name in config
    sed -i.bak "s|^model = .*|model = \"$MODEL\"|" config.toml
    # Update vision setting
    if [ "$VISION" = "true" ]; then
        sed -i.bak "s|^vision_enabled = .*|vision_enabled = true|" config.toml
    else
        sed -i.bak "s|^vision_enabled = .*|vision_enabled = false|" config.toml
    fi
    rm -f config.toml.bak
    ok "config.toml updated with model: $MODEL"
fi

# ---- Check ADB device ----
echo ""
if command -v adb &>/dev/null; then
    DEVICES=$(adb devices 2>/dev/null | grep -c "device$" || true)
    if [ "$DEVICES" -gt 0 ]; then
        ok "ADB: $DEVICES device(s) connected"
        adb devices -l | grep "device " || true
    else
        warn "No ADB device connected."
        echo ""
        echo "  To connect via USB:"
        echo "    1. Enable Developer Options (tap Build Number 7x)"
        echo "    2. Enable USB Debugging in Developer Options"
        echo "    3. Plug in USB cable, tap 'Allow' on phone"
        echo ""
        echo "  To connect via WiFi:"
        echo "    adb tcpip 5555"
        echo "    adb connect <phone-ip>:5555"
    fi
fi

# ---- Run doctor ----
echo ""
info "Running doctor..."
./target/release/hermitdroid doctor --config config.toml
echo ""

# ---- Done ----
echo "================================================"
echo -e "${GREEN}✅ Installation complete!${NC}"
echo ""
echo "Next steps:"
echo "  1. Connect your phone via ADB (if not already)"
echo "  2. Edit workspace/USER.md with your info"
echo "  3. Test:  ./target/release/hermitdroid --config config.toml --dry-run"
echo "  4. Run:   ./target/release/hermitdroid --config config.toml"
echo ""
echo "Full guide: SETUP_GUIDE.md"
echo "================================================"
