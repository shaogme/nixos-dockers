# NixOS Docker Images for VS Code Remote

[![Docker Publish](https://github.com/shaogme/nixos-dockers/actions/workflows/docker-publish.yml/badge.svg)](https://github.com/shaogme/nixos-dockers/actions/workflows/docker-publish.yml)
[![Auto Update Npins](https://github.com/shaogme/nixos-dockers/actions/workflows/auto-update-npins.yml/badge.svg)](https://github.com/shaogme/nixos-dockers/actions/workflows/auto-update-npins.yml)

一套基于 **Nix** 构建的轻量级、高性能 Docker 镜像，专为 **VS Code Remote / Dev Containers** 优化。

## 特性

- **Nix-Powered**: 利用 Nix 的声明式管理，确保镜像环境的精确一致性。
- **VS Code 优化**:
  - 内置 `nix-ld` 支持，完美运行 VS Code Server 及其各类扩展（如 Copilot）。
  - 遵循 FHS 标准的软链接，解决非 Nix 二进制程序的依赖问题。
- **自适应 UID/GID 映射**: 挂载宿主机目录时自动探测或支持通过 `HOST_UID:HOST_GID` 动态匹配宿主机用户权限，使用 `su-exec` 切换至匹配的本地普通用户（`dev`），彻底解决容器构建产物与宿主机权限冲突问题。
- **开箱即用**:
  - 内置 SSH 服务，支持远程连接。
  - 系统级 Git 安全目录：镜像制作时自动写入 `/etc/gitconfig`（默认将工作区 `/workspace` 设为 `safe.directory`），彻底解决宿主机挂载或跨用户操作时的 Git `dubious ownership` 权限告警。
  - 包含 `direnv` 和 `nix-direnv`，实现项目环境自动切换。
  - 集成常用开发工具（gcc, git, curl, vim 等）。
- **自动化运维**:
  - **每日更新**: 每天凌晨 3:00 (北京时间) 自动同步 `npins` 依赖。
  - **持续交付**: 每次代码推送自动构建并发布至 GHCR。

## 镜像列表与 Tag 规范

### 1. 通用轻量镜像（无 SSH 服务，适合本地容器 / CI / CLI）

| 镜像名称 | 描述 | 主要包含 | 示例 Tag |
| :--- | :--- | :--- | :--- |
| `npins` | 基础开发镜像 | Nix, npins, direnv, coreutils, nix-ld | `latest`, `0.5.0-2026.8.24` |
| `rust` | Rust 专用开发镜像 | Rust 工具链 (cargo, rustc), rust-analyzer, clippy, gdb | `latest`, `1.97.1-2026.8.24` |
| [`mise`](images/mise/README.md) | Mise 多语言环境开发镜像 | Nix, mise (跟踪 main 分支), direnv, coreutils | `latest`, `2026.8.12-2026.8.24` |

### 2. VS Code Remote 专用镜像（内置 SSH 服务与公钥自动注入）

| 镜像名称 | 描述 | 主要包含 | 示例 Tag |
| :--- | :--- | :--- | :--- |
| `vscode-npins` | 基础开发镜像 (SSH) | Nix, npins, direnv, coreutils, SSH | `latest`, `0.5.0-2026.8.24` |
| `vscode-rust` | Rust 专用镜像 (SSH) | Rust 工具链, rust-analyzer, clippy, gdb, SSH | `latest`, `1.97.1-2026.8.24` |
| [`vscode-mise`](images/mise/README.md) | Mise 开发镜像 (SSH) | Nix, mise, direnv, coreutils, SSH | `latest`, `2026.8.12-2026.8.24` |

### Tag 命名规则

每次 CI 构建都会发布两个 Tag：

1. **`latest`**: 指向最新一次构建的镜像。
2. **`<组件版本>-<发布日期>`**: 格式为 `版本-年.月.日`（如 `1.97.1-2026.8.24`）。若同一天内重新触发构建，将自动覆盖并更新当天的 Tag。

## 快速开始

### 1. 使用 Docker 直接运行（VS Code Remote SSH 镜像）

```bash
docker run -d \
  --name nix-dev \
  -p 2222:22 \
  -e HOST_UID=$(id -u):$(id -g) \
  -v $(pwd):/workspace \
  ghcr.io/shaogme/nixos-dockers/vscode-rust:latest
```

### 2. 使用 Docker 运行通用 CLI 镜像（无 SSH）

```bash
docker run -it --rm \
  -e HOST_UID=$(id -u):$(id -g) \
  -v $(pwd):/workspace \
  ghcr.io/shaogme/nixos-dockers/rust:latest
```

### 3. 使用 Docker Compose (VS Code Remote)

```yaml
services:
  nix-dev:
    image: ghcr.io/shaogme/nixos-dockers/vscode-rust:latest
    environment:
      - HOST_UID=${HOST_UID:-1000:1000}
      - CONTAINER_HOME=${CONTAINER_HOME:-/home/dev}
    ports:
      - "2222:22"
    volumes:
      - .:/workspace
      - cargo-cache:${CONTAINER_HOME:-/home/dev}/.cargo
    restart: unless-stopped
```

> [!TIP]
> **多用户家目录挂载**：
> 默认启动时，持久化卷将自动挂载至普通用户家目录（`/home/dev/.xxx`）。
> 若需切换为 root 身份运行，只需在启动时传入环境变量：
>
> ```bash
> HOST_UID=0 CONTAINER_HOME=/root docker compose up -d
> ```
>
> 卷将自动无缝重定向挂载至 `/root/.xxx`，底层脚本 0 硬编码，所见即所得。

### 4. 连接到开发环境

- **SSH**: `ssh dev@localhost -p 2222` 或 `ssh root@localhost -p 2222` (默认空密码)
- **VS Code**: 安装 `Remote - SSH` 扩展，添加主机 `localhost:2222` 即可。

> [!TIP]
> **注入公钥**: 将本地公钥 `id_ed25519.pub` 挂载到容器内 `/tmp/id_ed25519.pub`，SSH 镜像启动时会自动将其配置为 `/etc/ssh/authorized_keys/%u`（同时支持 `root` 与 `dev` 等普通用户登录），无需污染与操作用户家目录。
>
> ```yaml
> volumes:
>   - ~/.ssh/id_ed25519.pub:/tmp/id_ed25519.pub:ro
> ```

## 基于当前镜像制作自定义 Dockerfile

你可以将本仓库的镜像作为基础镜像（Base Image）构建自己的开发镜像。

### 入口机制：container-init 与 dev-env

镜像的 Docker `Entrypoint` 固定为 `/usr/bin/container-init run --`。运行时分为两个独立阶段：

1. `container-init` 执行镜像 profile 声明的 UID/GID、目录、软链接和 SSH action，然后按 handoff 配置交给 `dev-env`。
2. `dev-env` 加载 `/etc/dev-env/profiles.d`，物化 mise、Devbox、Rust 和其他 provider 的环境，并以同一份环境启动命令、shell 或 SSH login shell。

`HOST_UID=uid[:gid]`、`HOST_GID`、`CONTAINER_HOME` 和 `RUN_AS_ROOT=1` 是声明式 runtime input。`/bin/bash` 是兼容 shim，真实 Bash 位于 `/usr/local/libexec/dev-env/real/bash`；直接执行 `dev-env` 或 `docker exec ... dev-env ...` 会重新物化当前工作区环境。镜像不再包含旧的 `/bin/entrypoint.sh`。

### 编写自定义 Dockerfile 示例

派生镜像无需重新声明 Entrypoint 或 CMD；只需安装工具并增加 profile：

```dockerfile
FROM ghcr.io/shaogme/nixos-dockers/vscode-rust:latest

RUN nix profile add nixpkgs#bun
COPY .config/dev-env.toml /etc/dev-env/profiles.d/50-project.toml
```

自定义运行时初始化也应使用 Bootstrap DSL action；不要复制或链式调用旧 entrypoint。需要让新 profile 成为默认 profile 时，显式写入 `/etc/dev-env/default-profile`，并保持其 `extends` 链包含基础 profile。

> [!TIP]
> 完整的派生开发容器最佳实践（包含 BuildKit 缓存加速、构建期多语言工具预装与 Docker Compose 配置），请参考 [Mise 镜像与 Example 详细文档](images/mise/README.md)。

## 技术细节

### 为什么选择 Nix 构建镜像？

1. **极小体积与分层优化**: 使用 `buildLayeredImage` 自动提取依赖图并构建最优分层，避免了传统 Dockerfile 中大量的 `apt-get` 冗余。
2. **环境一致性**: 所有的依赖版本都由 `npins` (nixpkgs) 锁定，确保在任何机器上构建的结果完全一致。
3. **内建 nix-ld**: 解决了 VS Code Server 在 Nix 环境下无法直接运行二进制扩展（如 Copilot, C++ Intellisense）的痛点。

### 关键组件

- `nix-ld`: 动态链接器封装，自动为非 Nix 二进制程序寻找所需的 `.so` 文件。
- `direnv`: 进入目录时自动加载 `shell.nix` 或 `flake.nix` 环境。
- `dev-env` Bash shim：确保通过 SSH 登录、交互终端和 `docker exec` 时使用同一份物化环境。
- `/etc/gitconfig`: 构建期声明 Git `safe.directory`，保障容器工作区跨 UID/GID 权限时正常执行 Git 操作。

## 本地构建镜像

每个镜像目录（如 `images/rust`）均同时支持构建标准 CLI 镜像与 VS Code Remote 镜像：

```bash
# 1. 构建 Rust 通用 CLI 镜像
nix-build images/rust/image.nix -A rust

# 2. 构建 VS Code Remote Rust 镜像 (含 SSH)
nix-build images/rust/image.nix -A vscode-rust

# 3. 构建该目录下所有镜像变体
nix-build images/rust/image.nix
```

构建完成后，使用 `docker load < result` 即可将镜像导入本地 Docker。

### 本地 Docker 集成测试

每个 image 都提供一套 Docker 测试脚本。脚本会构建 CLI 与 VS Code Remote 两个变体，加载镜像，并验证 `container-init` 计划、`dev-env` 环境物化、运行时 handoff、登录 shell shim；VS Code 变体还会验证默认 SSH 服务能够部署并保持运行：

```bash
bash images/rust/tests/docker.sh
bash images/npins/tests/docker.sh
bash images/mise/tests/docker.sh
```

CI 会对 `mise`、`npins`、`rust` 三个 image 运行相同测试。`coding-images` 暂不纳入本次迁移。

## 项目结构

```text
.
├── images/                # Docker 镜像定义目录 (每个定义同时产出 CLI 与 VS Code 镜像)
│   ├── npins/             # 基础通用镜像 (npins, vscode-npins)
│   ├── rust/              # Rust 专用镜像 (rust, vscode-rust)
│   └── mise/              # Mise 专用镜像 (mise, vscode-mise) -> 详见 [Mise 文档](images/mise/README.md)
│       └── example/       # 生产级派生开发容器示例 (Dockerfile, compose, entrypoint)
├── modules/               # 统一 NixOS 模块系统
│   ├── core/              # 核心构建器、系统配置与 container-init/dev-env runtime
│   └── profiles/          # 语言与工具特性 Profile (base, rust, npins, mise)
├── update-npins.sh        # 依赖自动更新脚本
└── .github/workflows/     # CI/CD 自动化构建发布工作流
```

## 环境变量

镜像内置了以下关键环境变量以确保环境正常运行：

- `NIX_PATH`: 设置为 `nixpkgs=${pkgs.path}`，确保 `nix-shell`、`import <nixpkgs>` 等工具能够直接在 Nix 搜索路径中找到 `nixpkgs`。
- `NIX_LD_LIBRARY_PATH`: 提供非 Nix 程序的动态链接库路径。
- `RUST_SRC_PATH`: Rust 源码路径（针对 `vscode-rust`）。
- `PATH`: 包含 `/bin`, `/usr/bin`, `/usr/local/bin`。

## 贡献

欢迎提交 Issue 或 Pull Request 来改进这些镜像！

## 开源协议

[MIT](LICENSE) © [shaogme](https://github.com/shaogme)
