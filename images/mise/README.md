# Mise Docker 开发镜像与示例指南

本目录包含基于 NixOS 模块化体系构建的 **Mise (Dev Tools & Environment Manager)** 镜像定义及其实战开发容器示例（`example/`）。

---

## 目录

- [镜像概述](#镜像概述)
- [核心架构与特性](#核心架构与特性)
- [镜像变体](#镜像变体)
- [本地构建镜像](#本地构建镜像)
- [实战示例 (example/) 详解](#实战示例-example-详解)
  - [1. 架构与设计理念](#1-架构与设计理念)
  - [2. Dockerfile 构建机制](#2-dockerfile-构建机制)
  - [3. entrypoint.sh 初始化流程](#3-entrypointsh-初始化流程)
  - [4. docker-compose.yml 运行模式](#4-docker-composeyml-运行模式)
  - [5. mise.toml 工具链配置](#5-misetoml-工具链配置)
- [快速上手开发](#快速上手开发)

---

## 镜像概述

[Mise-en-place (mise)](https://github.com/jdx/mise) 是一个现代化的多语言开发环境与工具链管理工具（类似 asdf、nvm、pyenv、rustup 的统一替代品），同时内置环境变量管理与任务编排能力。

本镜像将 **Nix 的声明式系统底座** 与 **Mise 的灵活多语言管理** 相结合，镜像具有以下优势：

1. **源码级追踪最新 Mise**：通过 `npins` 直接拉取跟踪 `jdx/mise` 主分支源码编译，保持特性最新。
2. **极速多语言环境搭建**：开发者仅需编写一份 `mise.toml`，即可在秒级拉取并使用 Node.js、Python、Rust、Go、Java 等工具链。
3. **完美解决动态链接问题**：容器内置 `nix-ld` 与完整的基础 FHS 运行库，彻底解决 Mise 下载的非 Nix 预编译二进制文件（ELF binaries）在 NixOS 环境下动态链接器（`ld-linux`）缺失的报错。

---

## 核心架构与特性

本镜像基于仓库内的模块化配置构建：

- **系统核心 (`modules/core/`)**：
  - **`nix-ld` 动态链接兼容**：配置 `NIX_LD` 与 `NIX_LD_LIBRARY_PATH`（`stdenv.cc.cc.lib`, `zlib`, `openssl`, `icu`, `libsecret`, `util-linux` 等），确保动态二进制可直接运行。
  - **FHS 标准路径映射**：自动在 `/lib64`, `/usr/lib`, `/usr/lib64`, `/usr/bin` 创建兼容性软链接。
  - **环境变量持久化**：内置 Bash Wrapper，在非交互式 / SSH / 交互式会话中自动导出并保护关键系统路径与 Nix 变量。
- **基础套件 Profile (`modules/profiles/base.nix`)**：
  - **构建与系统核心工具**：`gcc`, `glibc` (包含 `ldd`), `coreutils`, `findutils`, `gnugrep`, `gnused`, `gawk`, `gnutar`, `gzip`, `wget`, `which`, `xz`, `cacert`。
  - **现代 CLI 增强**：`ripgrep`, `fd`, `jq`, `zip`, `unzip`, `p7zip`, `zstd`。
  - **网络与调试诊断**：`curl`, `iproute2`, `iputils`, `dnsutils`, `procps` (`ps`, `pgrep`, `pkill`), `strace`, `lsof`, `vim`。
  - **Nix 生态组件**：`nix` (包管理器), `direnv`, `nix-direnv`, `NIX_PATH`。
- **Mise Profile (`modules/profiles/mise.nix`)**：
  - 引入由 `images/mise/npins` 锁定的最新 Mise 衍生包，并将其注入全局 `PATH`。

---

## 镜像变体

每个版本均发布两种形态的镜像：

| 镜像 Tag | 说明 | 适用场景 |
| :--- | :--- | :--- |
| `ghcr.io/shaogme/nixos-dockers/mise:latest` | 标准通用轻量镜像（无 SSH 服务） | 容器化 CI/CD、本地命令行工具、Dev Containers (直接 exec) |
| `ghcr.io/shaogme/nixos-dockers/vscode-mise:latest` | VS Code Remote 专用镜像（内置 OpenSSH） | VS Code Remote - SSH 远程连接开发、多机器协同 |

---

## 本地构建镜像

在项目根目录或本目录下均可使用 Nix 进行构建：

```bash
# 构建标准 CLI 镜像
nix-build images/mise/image.nix -A mise

# 构建 VS Code SSH 镜像
nix-build images/mise/image.nix -A vscode-mise

# 同时构建全部变体
nix-build images/mise/image.nix
```

构建完成后通过 `docker load < result` 导入本地 Docker 引擎。

---

## 实战示例 (example/) 详解

在 [`example/`](file:///d:/Documents/GitHub/nixos-dockers/images/mise/example) 目录下提供了一个完整的生产级派生开发环境模版，展示了如何基于基础 `mise` 镜像进行二次构建、工具预装与环境初始化。

### 1. 架构与设计理念

```text
images/mise/example/
├── Dockerfile           # 派生镜像构建配置（BuildKit 缓存加速 + 构建期工具预装）
├── entrypoint.sh        # 自定义容器入口脚本（环境探测 + 启动初始化 + 链式调用）
├── docker-compose.yml   # 容器编排（支持 dev 挂载开发与 standalone 独立运行）
└── mise.toml            # 统一开发工具链与自动化任务定义
```

### 2. Dockerfile 构建机制

参考 [`Dockerfile`](file:///d:/Documents/GitHub/nixos-dockers/images/mise/example/Dockerfile)：

1. **基础镜像继承**：以 `ghcr.io/shaogme/nixos-dockers/mise:latest` 为底座。
2. **BuildKit 缓存加速 Nix 安装**：

   ```dockerfile
   RUN --mount=type=cache,target=/root/.cache/nix \
       --mount=type=cache,target=/tmp \
       echo "sandbox = false" >> /etc/nix/nix.conf && \
       nix-env --option sandbox false -iA nixpkgs.bubblewrap nixpkgs.firefox nixpkgs.geckodriver
   ```

   通过 `--mount=type=cache` 缓存 Nix 下载与编译产物，无需重复拉取。
3. **构建期自动预装 Mise 工具**：

   ```dockerfile
   RUN --mount=type=cache,target=/root/.cache/mise \
       set -e; \
       ... \
       if [ $CONFIG_FOUND -eq 1 ]; then \
           mise trust --all; \
           mise install; \
       fi
   ```

   在 `docker build` 阶段智能探测工作区配置并提前安装 `mise.toml` 中定义的工具链，配合 Mise 缓存挂载，使首次容器启动无需等待漫长的下载过程。
4. **自定义 Entrypoint 与链式交接**：
   将入口设置为自定义的 `/usr/local/bin/mise-entrypoint.sh`，CMD 保持空数组以便接收默认启动逻辑。

### 3. entrypoint.sh 初始化流程

参考 [`entrypoint.sh`](file:///d:/Documents/GitHub/nixos-dockers/images/mise/example/entrypoint.sh)：

1. **工作区目录探测**：自动定位到 `/root/workspace`。
2. **多层级配置探测**：按优先级依次检测 `mise.local.toml`、`mise.toml`、`mise/config.toml`、`.mise/config.toml`、`.mise/conf.d/*.toml`、`.config/mise.toml`、`.config/mise/config.toml`、`.config/mise/conf.d/*.toml`。
3. **Shell Hook 自动注入**：向 `/root/.bashrc` 写入 `eval "$(mise activate bash)"`，使得进入终端或 VS Code 新建终端会话时自动激活当前目录的 mise 工具与环境变量。
4. **动态环境初始化与环境变量同步**：
   - 自动执行 `mise trust --all` 授权配置。
   - 运行 `mise install` 确保运行时新挂载的配置依赖已安装。
   - 执行 `eval "$(mise env -s bash)"`，将 mise 导出的环境变量立即注入当前进程，确保后续子命令能直接访问。
5. **平滑链式交接**：

   ```bash
   exec /bin/entrypoint.sh "$@"
   ```

   最终通过 `exec` 交接给底座 NixOS 镜像的入口脚本，完成 SSH 密钥生成、公钥自动注入及进程拉起。

### 4. docker-compose.yml 运行模式

参考 [`docker-compose.yml`](file:///d:/Documents/GitHub/nixos-dockers/images/mise/example/docker-compose.yml)：

- **通用基础配置 (`&app-base`)**：
  - `MISE_YES=1`：全局非交互模式，避免 CLI 提示阻塞。
  - `security_opt: [seccomp:unconfined]` 与 `cap_add: [SYS_ADMIN, SYS_PTRACE, NET_ADMIN]`：为系统调试（如 gdb/lldb/strace）、网络工具与容器内沙箱（bubblewrap）提供必需的内核能力。
- **开发模式 (`dev`)**：
  - 挂载本地代码：`.:/root/workspace`，代码修改即时生效。
  - 默认启动项：直接运行 `docker compose up -d` 即可进入开发。
- **独立模式 (`standalone`)**：
  - 使用镜像内 `COPY` 的文件运行，无需主机挂载，适合离线分发或自动化测试验证。
  - 通过 `docker compose --profile standalone up -d` 启动。

### 5. mise.toml 工具链配置

参考 [`mise.toml`](file:///d:/Documents/GitHub/nixos-dockers/images/mise/example/mise.toml)：

示例中预置了全栈与 WebAssembly 开发环境配置：

```toml
[tools]
codex = "latest"
node = "latest"
python = "latest"
rust = ["stable", "nightly"]

# WebAssembly 工具
"cargo:wasmi_cli" = "latest"
"cargo:wasm-pack" = "latest"
"cargo:wasm-bindgen-cli" = "latest"

# 实用工具
jq = "latest"
ripgrep = "latest"
gh = "latest"

[tasks.postinstall]
description = "为 stable 和 nightly 添加 wasm32 目标"
run = """
rustup target add wasm32-unknown-unknown --toolchain stable 2>/dev/null || true
rustup target add wasm32-unknown-unknown --toolchain nightly 2>/dev/null || true
"""
```

- 支持同时管理多个 Rust 工具链（`stable` 与 `nightly`）。
- 支持 `cargo:*` 插件直接安装 Cargo 衍生二进制工具。
- 利用 `tasks.postinstall` 钩子在安装完成后自动配置 `wasm32-unknown-unknown` 编译目标。

---

## 快速上手开发

### 启动开发容器

进入 `images/mise/example` 目录：

```bash
# 启动开发容器
docker compose up -d dev

# 进入容器交互终端
docker compose exec dev bash
```

在容器内即可直接使用在 `mise.toml` 中定义的全部工具链：

```bash
node -v
python --version
rustc --version
mise list
```
