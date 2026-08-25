ARG BASE_IMAGE=ghcr.io/shaogme/nixos-dockers/mise:latest
FROM ${BASE_IMAGE}

WORKDIR /root/workspace

# Copy custom entrypoint for automatic mise environment loading
COPY entrypoint.sh /usr/local/bin/mise-entrypoint.sh
RUN chmod +x /usr/local/bin/mise-entrypoint.sh

# Enable non-interactive auto-confirm for mise
ENV MISE_YES=1

# Install system-level binaries via nix-env with BuildKit cache mount & binary mirror
RUN --mount=type=cache,target=/root/.cache/nix \
    --mount=type=cache,target=/tmp \
    echo "sandbox = false" >> /etc/nix/nix.conf && \
    nix-env --option sandbox false -iA nixpkgs.bubblewrap nixpkgs.firefox nixpkgs.geckodriver

# Set the PATH to include nix-env profile
ENV PATH=/root/.nix-profile/bin:$PATH

# Copy workspace / config files into container for pre-installation
COPY . /root/workspace/

# Detect mise configuration at build time and pre-install tools with cache mount
RUN --mount=type=cache,target=/root/.cache/mise \
    set -e; \
    CONFIG_FOUND=0; \
    for cfg in "mise.local.toml" "mise.toml" "mise/config.toml" "docker/mise.toml" ".mise/config.toml" ".config/mise.toml" ".config/mise/config.toml"; do \
        if [ -f "$cfg" ]; then \
            CONFIG_FOUND=1; \
            break; \
        fi; \
    done; \
    if [ $CONFIG_FOUND -eq 0 ]; then \
        for dir in ".mise/conf.d" ".config/mise/conf.d"; do \
            if [ -d "$dir" ] && compgen -G "$dir/*.toml" > /dev/null 2>&1; then \
                CONFIG_FOUND=1; \
                break; \
            fi; \
        done; \
    fi; \
    if [ $CONFIG_FOUND -eq 1 ]; then \
        echo "Pre-installing mise tools during docker build..."; \
        mise trust --all; \
        mise install rust; \
        mise install; \
    fi

# Set the custom entrypoint
ENTRYPOINT ["/usr/local/bin/mise-entrypoint.sh"]

# Default command passed to entrypoint (empty to trigger default behavior: sshd or bash)
CMD []

