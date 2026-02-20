# Hermitdroid Makefile
# Usage:
#   make              # build release + install to ~/.hermitdroid/
#   make build        # just build release (no install)
#   make install      # copy binary + config to ~/.hermitdroid/
#   make run          # build, install, and run
#   make clean        # cargo clean
#   make uninstall    # remove ~/.hermitdroid/ and symlink

INSTALL_DIR := $(HOME)/.hermitdroid
BIN_DIR     := $(HOME)/.local/bin
BINARY      := target/release/hermitdroid
WORKSPACE   := workspace

.PHONY: all build install run clean uninstall

# Default: build + install
all: build install

build:
	cargo build --release

install: $(BINARY)
	@echo ""
	@echo "📦 Installing to $(INSTALL_DIR)..."
	@mkdir -p "$(INSTALL_DIR)"
	@cp -f "$(BINARY)" "$(INSTALL_DIR)/hermitdroid"
	@# Copy config.toml only if it doesn't already exist (don't overwrite user edits)
	@if [ ! -f "$(INSTALL_DIR)/config.toml" ] && [ -f config.toml ]; then \
		cp config.toml "$(INSTALL_DIR)/config.toml"; \
		echo "   Copied config.toml (first install)"; \
	else \
		echo "   config.toml already exists, skipping (won't overwrite)"; \
	fi
	@# Copy workspace templates only if workspace dir doesn't exist yet
	@if [ ! -d "$(INSTALL_DIR)/workspace" ] && [ -d "$(WORKSPACE)" ]; then \
		cp -r "$(WORKSPACE)" "$(INSTALL_DIR)/workspace"; \
		echo "   Copied workspace/ templates (first install)"; \
	else \
		echo "   workspace/ already exists, skipping"; \
	fi
	@# Symlink to ~/.local/bin so it's on PATH
	@mkdir -p "$(BIN_DIR)"
	@ln -sf "$(INSTALL_DIR)/hermitdroid" "$(BIN_DIR)/hermitdroid"
	@echo ""
	@echo "✅ Installed!"
	@echo "   Binary:  $(INSTALL_DIR)/hermitdroid"
	@echo "   Symlink: $(BIN_DIR)/hermitdroid"
	@echo "   Config:  $(INSTALL_DIR)/config.toml"
	@echo ""
	@echo "   Make sure ~/.local/bin is in your PATH:"
	@echo '     export PATH="$$HOME/.local/bin:$$PATH"'
	@echo ""

run: all
	@echo "🚀 Starting hermitdroid..."
	@cd "$(INSTALL_DIR)" && ./hermitdroid

clean:
	cargo clean

uninstall:
	@echo "🗑️  Removing hermitdroid..."
	@rm -f "$(BIN_DIR)/hermitdroid"
	@rm -rf "$(INSTALL_DIR)"
	@echo "   Done. Removed $(INSTALL_DIR) and $(BIN_DIR)/hermitdroid"