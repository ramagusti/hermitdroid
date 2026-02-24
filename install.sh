#!/usr/bin/env bash
# ╔══════════════════════════════════════════════════════╗
# ║          Hermitdroid — Installer                     ║
# ╚══════════════════════════════════════════════════════╝
#
# Installs to:
#   ~/.hermitdroid/           workspace, config, logs
#   ~/.local/bin/hermitdroid  binary
#
# No source code, .git history, or build artifacts are kept.

set -euo pipefail

BOLD="\033[1m"
DIM="\033[2m"
GREEN="\033[32m"
YELLOW="\033[33m"
RED="\033[31m"
CYAN="\033[36m"
RESET="\033[0m"

INSTALL_DIR="${HERMITDROID_DIR:-$HOME/.hermitdroid}"
BIN_DIR="${HERMITDROID_BIN:-$HOME/.local/bin}"

info()  { echo -e "  ${GREEN}✓${RESET} $*"; }
warn()  { echo -e "  ${YELLOW}⚠${RESET} $*"; }
fail()  { echo -e "  ${RED}✗${RESET} $*"; exit 1; }
step()  { echo -e "\n${CYAN}━━━ $* ━━━${RESET}\n"; }

command_exists() { command -v "$1" &>/dev/null; }

# ── Banner ───────────────────────────────────────────────────────────────────

echo -e "
${CYAN}${BOLD}╔══════════════════════════════════════════════════════╗
║           🤖 Hermitdroid Installer                   ║
╚══════════════════════════════════════════════════════╝${RESET}
"

# ── Check & install dependencies ─────────────────────────────────────────────

step "Checking dependencies"

# # Rust
# if command_exists cargo; then
#     info "Rust/Cargo found: $(cargo --version)"
# else
#     warn "Rust not found. Installing via rustup..."
#     curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
#     source "$HOME/.cargo/env"
#     info "Rust installed: $(cargo --version)"
# fi

# # Git
# if command_exists git; then
#     info "Git found"
# else
#     fail "Git is required. Install: sudo apt install git"
# fi

# ADB
if command_exists adb; then
    info "ADB found: $(adb version | head -1)"
else
    warn "ADB not found (needed for phone control)"
    echo -e "    ${DIM}Linux: sudo apt install adb${RESET}"
    echo -e "    ${DIM}macOS: brew install android-platform-tools${RESET}"
fi

# Tailscale — auto-install if not present
if command_exists tailscale; then
    info "Tailscale found"
    if tailscale status &>/dev/null; then
        info "Tailscale connected: $(tailscale ip -4 2>/dev/null || echo 'unknown')"
    else
        echo -e "  ${DIM}ℹ  Tailscale installed but not connected. Run: sudo tailscale up${RESET}"
    fi
else
    echo -e "  ${YELLOW}⚠${RESET}  Tailscale not found (needed for remote access)"
    echo ""
    read -p "  Install Tailscale now? [Y/n] " -n 1 -r
    echo ""
    if [[ ! $REPLY =~ ^[Nn]$ ]]; then
        echo -e "  Installing Tailscale..."

        if [ -f /etc/os-release ]; then
            . /etc/os-release
            case "$ID" in
                ubuntu|debian|pop|linuxmint|elementary|zorin|kali)
                    curl -fsSL https://tailscale.com/install.sh | sh
                    ;;
                fedora|rhel|centos|rocky|alma)
                    sudo dnf install -y tailscale 2>/dev/null || curl -fsSL https://tailscale.com/install.sh | sh
                    ;;
                arch|manjaro|endeavouros)
                    sudo pacman -S --noconfirm tailscale 2>/dev/null || curl -fsSL https://tailscale.com/install.sh | sh
                    ;;
                *)
                    curl -fsSL https://tailscale.com/install.sh | sh
                    ;;
            esac
        elif [[ "$(uname)" == "Darwin" ]]; then
            if command_exists brew; then
                brew install tailscale
            else
                echo -e "  ${YELLOW}⚠${RESET}  Install Homebrew first, or get Tailscale from https://tailscale.com/download/mac"
            fi
        else
            curl -fsSL https://tailscale.com/install.sh | sh
        fi

        if command_exists tailscale; then
            info "Tailscale installed: $(tailscale version 2>/dev/null | head -1)"

            # Enable and start the service
            if command_exists systemctl; then
                sudo systemctl enable --now tailscaled 2>/dev/null || true
            fi

            echo ""
            read -p "  Log into Tailscale now? [Y/n] " -n 1 -r
            echo ""
            if [[ ! $REPLY =~ ^[Nn]$ ]]; then
                sudo tailscale up
                if tailscale status &>/dev/null; then
                    info "Tailscale connected: $(tailscale ip -4 2>/dev/null || echo 'unknown')"
                else
                    warn "Tailscale login may not have completed. Run: sudo tailscale up"
                fi
            fi
        else
            warn "Tailscale installation failed. Install manually: https://tailscale.com/download"
        fi
    else
        echo -e "  ${DIM}Skipped. Install later: curl -fsSL https://tailscale.com/install.sh | sh${RESET}"
    fi
fi

# ── Build ────────────────────────────────────────────────────────────────────

step "Downloading"

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

GITHUB_REPO="ramagusti/hermitdroid"
TMPDIR=$(mktemp -d)
trap "rm -rf '$TMPDIR'" EXIT

pick_linux_target() {
    local arch="$1"
    local arch_rust

    case "$arch" in
        x86_64)  arch_rust="x86_64" ;;
        aarch64) arch_rust="aarch64" ;;
        *) fail "Unsupported architecture: $arch" ;;
    esac

    # Check glibc version — if >= 2.35, use gnu (faster); otherwise use musl (static)
    local glibc_ver=""
    if command_exists ldd; then
        glibc_ver=$(ldd --version 2>&1 | head -1 | grep -oP '\d+\.\d+$' || true)
    fi

    if [ -n "$glibc_ver" ]; then
        local major minor
        major=$(echo "$glibc_ver" | cut -d. -f1)
        minor=$(echo "$glibc_ver" | cut -d. -f2)

        if [ "$major" -gt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -ge 35 ]; }; then
            echo "${arch_rust}-unknown-linux-gnu"
            return
        fi
    fi

    # Old glibc or can't detect — use musl (static, works everywhere)
    echo "${arch_rust}-unknown-linux-musl"
}

case "$OS" in
    linux)
        TARGET=$(pick_linux_target "$ARCH")
        ;;
    darwin)
        case "$ARCH" in
            x86_64)  TARGET="x86_64-apple-darwin" ;;
            arm64)   TARGET="aarch64-apple-darwin" ;;
            *) fail "Unsupported macOS architecture: $ARCH" ;;
        esac
        ;;
    *)
        fail "Unsupported OS: $OS"
        ;;
esac

DOWNLOAD_URL="https://github.com/$GITHUB_REPO/releases/latest/download/hermitdroid-$TARGET.tar.gz"

echo -e "  Platform: ${BOLD}$TARGET${RESET}"
if [[ "$TARGET" == *musl* ]]; then
    echo -e "  ${DIM}(static build — compatible with all Linux versions)${RESET}"
fi
echo -e "  Downloading from GitHub Releases..."

if curl -fSL "$DOWNLOAD_URL" -o "$TMPDIR/hermitdroid.tar.gz"; then
    tar xzf "$TMPDIR/hermitdroid.tar.gz" -C "$TMPDIR"
    info "Downloaded hermitdroid for $TARGET"
else
    # If gnu failed, try musl as fallback
    if [[ "$TARGET" == *gnu* ]]; then
        FALLBACK_TARGET="${TARGET/gnu/musl}"
        FALLBACK_URL="https://github.com/$GITHUB_REPO/releases/latest/download/hermitdroid-$FALLBACK_TARGET.tar.gz"
        warn "gnu build not available, trying static (musl) build..."
        if curl -fSL "$FALLBACK_URL" -o "$TMPDIR/hermitdroid.tar.gz"; then
            tar xzf "$TMPDIR/hermitdroid.tar.gz" -C "$TMPDIR"
            info "Downloaded hermitdroid for $FALLBACK_TARGET (static)"
        else
            fail "Download failed. Check https://github.com/$GITHUB_REPO/releases"
        fi
    else
        fail "Download failed. Check https://github.com/$GITHUB_REPO/releases"
    fi
fi

# ── Install binary ───────────────────────────────────────────────────────────

step "Installing"

mkdir -p "$BIN_DIR"
rm -f "$BIN_DIR/hermitdroid"
cp "$TMPDIR/hermitdroid" "$BIN_DIR/hermitdroid"
chmod +x "$BIN_DIR/hermitdroid"
info "Binary → $BIN_DIR/hermitdroid"

if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
    warn "$BIN_DIR is not in your PATH"
    echo -e "    Add to your shell profile (~/.bashrc, ~/.zshrc):"
    echo -e "    ${DIM}export PATH=\"$BIN_DIR:\$PATH\"${RESET}"
    export PATH="$BIN_DIR:$PATH"
fi

# ── Set up ~/.hermitdroid ────────────────────────────────────────────────────

step "Setting up workspace"

mkdir -p "$INSTALL_DIR/workspace/memory"
mkdir -p "$INSTALL_DIR/workspace/skills"
mkdir -p "$INSTALL_DIR/workspace/canvas"

# Copy default workspace files from repo
# Priority: workspace.default/ > workspace/ > create empty
REPO_WS_DEFAULT="$TMPDIR/workspace.default"
REPO_WS="$TMPDIR/workspace"
mkdir -p "$REPO_WS_DEFAULT" "$REPO_WS"

RAW_URL="https://raw.githubusercontent.com/$GITHUB_REPO/main"
for file in SOUL.md IDENTITY.md AGENTS.md TOOLS.md USER.md HEARTBEAT.md MEMORY.md GOALS.md BOOTSTRAP.md; do
    if [ ! -f "$INSTALL_DIR/workspace/$file" ]; then
        if curl -fsSL "$RAW_URL/workspace.default/$file" -o "$INSTALL_DIR/workspace/$file" 2>/dev/null; then
            info "$file (from repo)"
        else
            touch "$INSTALL_DIR/workspace/$file"
            info "$file (created empty)"
        fi
    else
        echo -e "  ${DIM}$file already exists (preserved)${RESET}"
    fi
done

# Copy skills from repo if any
for skill_src in "$REPO_WS_DEFAULT"/skills/*/ "$REPO_WS"/skills/*/; do
    if [ -d "$skill_src" ]; then
        skill_name=$(basename "$skill_src")
        skill_dest="$INSTALL_DIR/workspace/skills/$skill_name"
        if [ ! -d "$skill_dest" ]; then
            cp -r "$skill_src" "$skill_dest"
            info "Skill: $skill_name"
        fi
    fi
done 2>/dev/null || true

# Config — use absolute workspace_path so hermitdroid works from any directory
if [ ! -f "$INSTALL_DIR/config.toml" ]; then
    if curl -fsSL "$RAW_URL/config.default.toml" -o "$TMPDIR/config.toml" 2>/dev/null; then
        sed "s|workspace_path = \"./workspace\"|workspace_path = \"$INSTALL_DIR/workspace\"|g" \
            "$TMPDIR/config.toml" > "$INSTALL_DIR/config.toml"
        info "Config → $INSTALL_DIR/config.toml"
    else
        warn "No config.toml in repo — onboard wizard will create one"
    fi
else
    # Fix workspace_path if it's still relative
    if grep -q 'workspace_path = "\./workspace"' "$INSTALL_DIR/config.toml" 2>/dev/null; then
        sed -i "s|workspace_path = \"./workspace\"|workspace_path = \"$INSTALL_DIR/workspace\"|g" \
            "$INSTALL_DIR/config.toml"
        info "Fixed workspace_path → $INSTALL_DIR/workspace"
    fi
    info "Config already exists (preserved)"
fi

# ── Show what's installed ────────────────────────────────────────────────────

echo ""
echo -e "  ${BOLD}Installed:${RESET}"
echo -e "    ${DIM}$BIN_DIR/hermitdroid${RESET}              (binary)"
echo -e "    ${DIM}$INSTALL_DIR/config.toml${RESET}          (configuration)"
echo -e "    ${DIM}$INSTALL_DIR/workspace/${RESET}            (agent workspace)"

WS_FILES=$(find "$INSTALL_DIR/workspace" -maxdepth 1 -name "*.md" -size +0c | wc -l)
echo -e "    ${DIM}$WS_FILES workspace files with content${RESET}"

# ── Launch onboarding ────────────────────────────────────────────────────────

step "Setup"

CONFIG_FILE="$INSTALL_DIR/config.toml"

if [ -f "$CONFIG_FILE" ]; then
    echo -e "  Config exists at ${BOLD}$CONFIG_FILE${RESET}"
    echo ""
    read -p "  Run the setup wizard? [Y/n] " -n 1 -r
    echo ""
    if [[ ! $REPLY =~ ^[Nn]$ ]]; then
        "$BIN_DIR/hermitdroid" --config "$CONFIG_FILE" onboard
    else
        info "Keeping existing configuration"
    fi
else
    echo -e "  ${BOLD}Launching first-run setup wizard...${RESET}\n"
    "$BIN_DIR/hermitdroid" --config "$CONFIG_FILE" onboard
fi

# ── Verify ───────────────────────────────────────────────────────────────────

step "Verification"

"$BIN_DIR/hermitdroid" --config "$CONFIG_FILE" doctor 2>/dev/null \
    && info "Health check passed" \
    || warn "Health check has issues (see above)"

# ── Done ─────────────────────────────────────────────────────────────────────

echo -e "
${CYAN}${BOLD}╔══════════════════════════════════════════════════════╗
║           ✅  Installation Complete!                  ║
╚══════════════════════════════════════════════════════╝${RESET}

  ${BOLD}Usage:${RESET}
    hermitdroid                    Start the agent (gateway)
    hermitdroid gateway            Start the agent (explicit)
    hermitdroid onboard            Run setup wizard
    hermitdroid doctor             Check workspace & config health
    hermitdroid status             Show agent status
    hermitdroid chat \"message\"     Send a command to running agent
    hermitdroid stop               Pause the agent
    hermitdroid restart            Restart the agent
    hermitdroid logs               Follow agent logs
    hermitdroid service install    Install as systemd background service
    hermitdroid service status     Check service status
    hermitdroid --dry-run          Test without executing actions

  ${BOLD}Files:${RESET}
    Binary:    $BIN_DIR/hermitdroid
    Config:    $CONFIG_FILE
    Workspace: $INSTALL_DIR/workspace/
"

if command_exists tailscale && tailscale status &>/dev/null; then
    TS_IP=$(tailscale ip -4 2>/dev/null || echo "")
    if [ -n "$TS_IP" ]; then
        echo -e "  ${BOLD}Remote Access (Tailscale):${RESET}"
        echo -e "    Dashboard: ${DIM}http://${TS_IP}:8420${RESET}"
        echo ""
    fi
fi
