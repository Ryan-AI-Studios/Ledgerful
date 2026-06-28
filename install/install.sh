#!/usr/bin/env sh
set -eu

REPO="${LEDGERFUL_REPO:-Ryan-AI-Studios/Ledgerful}"
VERSION="${LEDGERFUL_VERSION:-latest}"
INSTALL_DIR="${LEDGERFUL_INSTALL_DIR:-$HOME/.local}"
NO_PATH_UPDATE="${LEDGERFUL_NO_PATH_UPDATE:-0}"
BUILD_FROM_SOURCE="${LEDGERFUL_BUILD_FROM_SOURCE:-0}"
FEATURES=""

if [ "${LEDGERFUL_DAEMON:-0}" = "1" ]; then
  FEATURES="--features daemon"
fi

step() {
  printf '==> %s\n' "$1"
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

install_from_release() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os:$arch" in
    Linux:x86_64) asset="ledgerful-x86_64-unknown-linux-gnu.tar.gz" ;;
    Darwin:x86_64) asset="ledgerful-x86_64-apple-darwin.tar.gz" ;;
    Darwin:arm64) asset="ledgerful-aarch64-apple-darwin.tar.gz" ;;
    *) return 1 ;;
  esac

  tag_path="latest/download"
  if [ "$VERSION" != "latest" ]; then
    tag_path="download/$VERSION"
  fi

  url="https://github.com/$REPO/releases/$tag_path/$asset"
  tmp_dir="$(mktemp -d)"
  archive="$tmp_dir/$asset"
  mkdir -p "$INSTALL_DIR/bin"

  step "Downloading $url"
  if has_cmd curl; then
    curl -fsSL "$url" -o "$archive"
  elif has_cmd wget; then
    wget -q "$url" -O "$archive"
  else
    return 1
  fi

  tar -xzf "$archive" -C "$tmp_dir"
  bin_path="$(find "$tmp_dir" -type f -name ledgerful | head -n 1)"
  if [ -z "$bin_path" ]; then
    return 1
  fi

  for name in ledgerful ledgerful ldg; do
    cp "$bin_path" "$INSTALL_DIR/bin/$name"
    chmod +x "$INSTALL_DIR/bin/$name"
  done
  rm -rf "$tmp_dir"
}

install_from_cargo() {
  if ! has_cmd cargo; then
    echo "Rust cargo was not found. Install Rust from https://rustup.rs or publish a Ledgerful release asset, then rerun this installer." >&2
    exit 1
  fi

  if [ -f Cargo.toml ] && grep -q 'name = "ledgerful"' Cargo.toml; then
    step "Installing Ledgerful from the current checkout"
    # shellcheck disable=SC2086
    cargo install --path . --locked --root "$INSTALL_DIR" $FEATURES
  else
    step "Installing Ledgerful from https://github.com/$REPO"
    # shellcheck disable=SC2086
    cargo install ledgerful --git "https://github.com/$REPO" --branch main --locked --root "$INSTALL_DIR" $FEATURES
  fi
}

install_default_config() {
  config_dir="$HOME/.ledgerful"
  default_config="$config_dir/default-config.toml"

  mkdir -p "$config_dir"

  if [ -f "$default_config" ]; then
    step "Default init config already exists at $default_config"
    return
  fi

  if [ -f ".ledgerful/config.toml" ]; then
    cp ".ledgerful/config.toml" "$default_config"
    step "Seeded default init config from current checkout: $default_config"
    return
  fi

  if [ -f "docs/examples/config.toml" ]; then
    cp "docs/examples/config.toml" "$default_config"
    step "Seeded default init config from docs example: $default_config"
    return
  fi

  cat > "$default_config" <<'EOF'
[core]
strict = false
auto_fix = false

[watch]
debounce_ms = 1000
ignore_patterns = ["target/**", ".git/**", "node_modules/**"]

[temporal]
max_commits = 1000
max_files_per_commit = 50
coupling_threshold = 0.75
min_shared_commits = 3
min_revisions = 5
decay_half_life = 100

[hotspots]
max_commits = 500
limit = 10

# [verify]
# default_timeout_secs = 300
# Steps to run when `ledgerful verify` is invoked without -c.
# Each step has a description, command, and optional timeout_secs (defaults to 300).
# [[verify.steps]]
# description = "Run project tests"
# command = "cargo test -j 1 -- --test-threads=1"
# timeout_secs = 300
# [[verify.steps]]
# description = "Check formatting"
# command = "cargo fmt --check"

[gemini]
# Prefer GEMINI_API_KEY in the environment or local .env.
# api_key = "..."
# Optional override for every ask mode:
# model = "gemini-3.1-pro"
fast_model = "gemini-3.1-flash-lite"
deep_model = "gemini-3.1-pro"
timeout_secs = 120
context_window = 128000
EOF

  step "Seeded starter default init config: $default_config"
}

if [ "$BUILD_FROM_SOURCE" = "1" ]; then
  install_from_cargo
else
  if ! install_from_release; then
    step "Release install failed; falling back to cargo install"
    install_from_cargo
  fi
fi

install_default_config

if [ "$NO_PATH_UPDATE" != "1" ]; then
  case ":$PATH:" in
    *":$INSTALL_DIR/bin:"*) ;;
    *)
      shell_rc=""
      if [ -n "${SHELL:-}" ]; then
        case "$SHELL" in
          */zsh) shell_rc="$HOME/.zshrc" ;;
          */bash) shell_rc="$HOME/.bashrc" ;;
        esac
      fi
      if [ -n "$shell_rc" ]; then
        touch "$shell_rc"
        if ! grep -q "$INSTALL_DIR/bin" "$shell_rc"; then
          printf '\nexport PATH="$PATH:%s/bin"\n' "$INSTALL_DIR" >> "$shell_rc"
          step "Added $INSTALL_DIR/bin to $shell_rc. Open a new terminal for other sessions."
        fi
      else
        step "Add $INSTALL_DIR/bin to PATH for future shells."
      fi
      export PATH="$PATH:$INSTALL_DIR/bin"
      ;;
  esac
fi

step "Verifying installation"
for name in ledgerful ledgerful ldg; do
  "$INSTALL_DIR/bin/$name" --version | sed -n '1p'
done

printf '\nLedgerful installed. AI agents can now run: ledgerful doctor\n'
printf 'Default init config: %s\n' "$HOME/.ledgerful/default-config.toml"
