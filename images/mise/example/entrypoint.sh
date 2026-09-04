#!/usr/bin/env bash
set -e

# ==========================================
# Mise Development Container Entrypoint
# ==========================================

WORKSPACE="${WORKSPACE_DIR:-/workspace}"

if [ -d "$WORKSPACE" ]; then
    cd "$WORKSPACE"
fi

# Candidate configuration files in order of precedence:
# 1. mise.local.toml
# 2. mise.toml
# 3. mise/config.toml
# 4. .mise/config.toml
# 5. .mise/conf.d/*.toml
# 6. .config/mise.toml
# 7. .config/mise/config.toml
# 8. .config/mise/conf.d/*.toml

CONFIG_FOUND=0
FOUND_PATH=""

CANDIDATE_FILES=(
    "mise.local.toml"
    "mise.toml"
    "mise/config.toml"
    ".mise/config.toml"
    ".config/mise.toml"
    ".config/mise/config.toml"
)

for cfg in "${CANDIDATE_FILES[@]}"; do
    if [ -f "$cfg" ]; then
        CONFIG_FOUND=1
        FOUND_PATH="$cfg"
        break
    fi
done

if [ $CONFIG_FOUND -eq 0 ]; then
    for dir in ".mise/conf.d" ".config/mise/conf.d"; do
        if [ -d "$dir" ] && compgen -G "$dir/*.toml" > /dev/null; then
            CONFIG_FOUND=1
            FOUND_PATH="$dir/*.toml"
            break
        fi
    done
fi

export MISE_YES=1

# Ensure ~/.bashrc hooks mise for interactive subshells and VS Code terminal sessions
if [ ! -f /root/.bashrc ] || ! grep -q "mise activate" /root/.bashrc; then
    echo 'eval "$(mise activate bash)"' >> /root/.bashrc
fi
for home_dir in /home/*; do
    if [ -d "$home_dir" ]; then
        if [ ! -f "$home_dir/.bashrc" ] || ! grep -q "mise activate" "$home_dir/.bashrc"; then
            echo 'eval "$(mise activate bash)"' >> "$home_dir/.bashrc"
        fi
    fi
done

if [ $CONFIG_FOUND -eq 1 ]; then
    echo "[mise-entrypoint] Found mise config ($FOUND_PATH). Initializing environment..."
    mise trust --all 2>/dev/null || true
    echo "[mise-entrypoint] Installing tools via mise..."
    mise install rust || true
    mise install || true
    mise task run postinstall || true
    # Load mise environment variables into current shell so child processes inherit them
    eval "$(mise env -s bash 2>/dev/null || true)"
    echo "[mise-entrypoint] Mise environment ready."
else
    echo "[mise-entrypoint] No mise configuration found in workspace ($WORKSPACE). Skipping pre-installation."
fi

# Hand over execution to the base NixOS container entrypoint
exec /bin/entrypoint.sh "$@"
