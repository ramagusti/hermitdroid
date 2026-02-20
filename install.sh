#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="$HOME/.hermitdroid"
REPO_URL="https://github.com/ramagusti/hermitdroid.git"
BIN_DIR="$HOME/.local/bin"

echo "╔══════════════════════════════════════════╗"
echo "║       Hermitdroid Installer              ║"
echo "╚══════════════════════════════════════════╝"
echo ""
echo "[INFO] Install directory: $INSTALL_DIR"
echo ""

# --- Helper: install a package ---
pkg_install() {
    local pkg="$1"
    if command -v apt-get &>/dev/null; then
        sudo apt-get update -qq && sudo apt-get install -y -qq "$pkg"
    elif command -v dnf &>/dev/null; then
        sudo dnf install -y "$pkg"
    elif command -v pacman &>/dev/null; then
        sudo pacman -S --noconfirm "$pkg"
    elif command -v brew &>/dev/null; then
        brew install "$pkg"
    else
        echo "[ERROR] Could not detect package manager. Please install $pkg manually."
        exit 1
    fi
}

# --- Install dependencies ---
for dep in zstd git make; do
    if ! command -v "$dep" &>/dev/null; then
        echo "[INFO] Installing $dep..."
        pkg_install "$dep"
        echo "[OK]   $dep installed"
    else
        echo "[OK]   $dep already installed"
    fi
done

# --- Install Rust & Cargo ---
if ! command -v rustc &>/dev/null || ! command -v cargo &>/dev/null; then
    echo "[INFO] Installing Rust and Cargo via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    echo "[OK]   Rust installed: $(rustc --version)"
else
    echo "[OK]   Rust already installed: $(rustc --version)"
fi

# --- Update Rust (need 1.85+ for edition2024) ---
echo "[INFO] Updating Rust to latest stable..."
rustup update stable 2>/dev/null
source "$HOME/.cargo/env" 2>/dev/null || true
echo "[OK]   Rust version: $(rustc --version)"

RUST_MINOR=$(rustc --version | grep -oP '1\.(\d+)' | head -1 | cut -d. -f2)
if [ "$RUST_MINOR" -lt 85 ]; then
    echo "[ERROR] Rust 1.85+ required (you have $(rustc --version))."
    exit 1
fi

# --- Clone or update repo ---
if [ -d "$INSTALL_DIR/.git" ]; then
    echo "[INFO] Updating existing installation..."
    cd "$INSTALL_DIR"
    git pull --ff-only 2>/dev/null || {
        echo "[WARN] Could not fast-forward. Fetching latest..."
        git fetch origin
        git reset --hard origin/main
    }
else
    if [ -d "$INSTALL_DIR" ]; then
        echo "[WARN] $INSTALL_DIR exists but is not a git repo. Backing up..."
        mv "$INSTALL_DIR" "${INSTALL_DIR}.bak.$(date +%s)"
    fi
    echo "[INFO] Cloning hermitdroid into $INSTALL_DIR..."
    git clone "$REPO_URL" "$INSTALL_DIR"
    cd "$INSTALL_DIR"
fi

# --- Fix known build issue ---
if [ -f "$INSTALL_DIR/src/server/mod.rs" ]; then
    if grep -q 'R::ok("queued")' "$INSTALL_DIR/src/server/mod.rs"; then
        echo "[INFO] Applying type fix in src/server/mod.rs..."
        sed -i 's/R::ok("queued")/R::ok("queued".to_string())/g' "$INSTALL_DIR/src/server/mod.rs"
        echo "[OK]   Fix applied"
    fi
fi

# --- Remove stale lockfile ---
rm -f "$INSTALL_DIR/Cargo.lock"

# --- Build + Install ---
echo "[INFO] Building Hermitdroid..."
cd "$INSTALL_DIR"

if [ -f Makefile ]; then
    make
else
    # Fallback if Makefile not yet in repo
    cargo build --release
    mkdir -p "$BIN_DIR"
    cp -f target/release/hermitdroid "$INSTALL_DIR/hermitdroid"
    ln -sf "$INSTALL_DIR/hermitdroid" "$BIN_DIR/hermitdroid"
fi

# --- Copy default workspace if not exists ---
if [ ! -d "$INSTALL_DIR/workspace" ] && [ -d "$INSTALL_DIR/workspace.default" ]; then
    echo "[INFO] Creating default workspace..."
    cp -r "$INSTALL_DIR/workspace.default" "$INSTALL_DIR/workspace"
    echo "[OK]   Default workspace created at $INSTALL_DIR/workspace/"
elif [ ! -d "$INSTALL_DIR/workspace" ]; then
    echo "[INFO] Creating minimal workspace..."
    mkdir -p "$INSTALL_DIR/workspace/skills"
    for f in SOUL.md TOOLS.md IDENTITY.md HEARTBEAT.md AGENTS.md USER.md GOALS.md MEMORY.md; do
        if [ ! -f "$INSTALL_DIR/workspace/$f" ]; then
            echo "# $f" > "$INSTALL_DIR/workspace/$f"
        fi
    done
    echo "[OK]   Minimal workspace created"
else
    echo "[OK]   Workspace already exists, skipping"
fi

# --- Check PATH ---
echo ""
if echo "$PATH" | grep -q "$BIN_DIR"; then
    echo "[OK]   ~/.local/bin is in your PATH"
else
    echo "[INFO] Add ~/.local/bin to your PATH:"
    echo '         export PATH="$HOME/.local/bin:$PATH"'
    echo ""
    echo "       Add it to your shell profile to make it permanent:"
    echo '         echo '\''export PATH="$HOME/.local/bin:$PATH"'\'' >> ~/.bashrc'
fi

# --- Done ---
echo ""
echo "╔══════════════════════════════════════════╗"
echo "║       ✅ Hermitdroid installed!          ║"
echo "╚══════════════════════════════════════════╝"
echo ""
echo "  Location:  $INSTALL_DIR"
echo "  Binary:    $BIN_DIR/hermitdroid"
echo "  Config:    $INSTALL_DIR/config.toml"
echo ""
echo "  Next steps:"
echo "    1. Edit ~/.hermitdroid/config.toml"
echo "    2. Connect your phone via USB (enable USB Debugging)"
echo "    3. hermitdroid doctor"
echo "    4. hermitdroid --dry-run"
echo "    5. hermitdroid"
echo ""