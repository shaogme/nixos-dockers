# NixOS Docker Images for VS Code Remote

[![Docker Publish](https://github.com/shaogme/nixos-dockers/actions/workflows/docker-publish.yml/badge.svg)](https://github.com/shaogme/nixos-dockers/actions/workflows/docker-publish.yml)
[![Auto Update Npins](https://github.com/shaogme/nixos-dockers/actions/workflows/auto-update-npins.yml/badge.svg)](https://github.com/shaogme/nixos-dockers/actions/workflows/auto-update-npins.yml)

一套基于 **Nix** 构建的轻量级、高性能 Docker 镜像，专为 **VS Code Remote / Dev Containers** 优化。

## 特性

- **Nix-Powered**: 利用 Nix 的声明式管理，确保镜像环境的精确一致性。
- **VS Code 优化**:
  - 内置 `nix-ld` 支持，完美运行 VS Code Server 及其各类扩展（如 Copilot）。
  - 遵循 FHS 标准的软链接，解决非 Nix 二进制程序的依赖问题。
- **自适应 UID/GID 映射**: 挂载宿主机目录时自动探测或支持通过 `HOST_UID:HOST_GID` 动态匹配宿主机用户权限，使用 `gosu` 切换至匹配的本地普通用户（`dev`），彻底解决容器构建产物与宿主机权限冲突问题。
- **开箱即用**:
  - 内置 SSH 服务，支持远程连接。
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
> **注入公钥**: 将本地公钥 `id_ed25519.pub` 挂载到容器内 `/tmp/id_ed25519.pub`，SSH 镜像启动时会自动将其添加到 `/root/.ssh/authorized_keys` 与 `/home/dev/.ssh/authorized_keys`。
>
> ```yaml
> volumes:
>   - ~/.ssh/id_ed25519.pub:/tmp/id_ed25519.pub:ro
> ```

## 基于当前镜像制作自定义 Dockerfile

你可以将本仓库的镜像作为基础镜像（Base Image）构建自己的开发镜像。

### 注意事项：Entrypoint 机制

镜像内置的 `/bin/entrypoint.sh`（由 Nix 动态生成）负责处理以下关键初始化逻辑：

1. **自动修复配置文件只读权限**（如 `/etc/passwd`, `/etc/group`, `/etc/shadow`）。
2. **自适应 UID/GID 探测与映射**：
   - **环境变量输入**：支持通过 `HOST_UID:HOST_GID`（例如 `-e HOST_UID=$(id -u):$(id -g)`）或分别传入 `HOST_UID`、`HOST_GID`。
   - **运行时智能探测**：若未显式指定 `HOST_UID`，启动时自动检测挂载工作区（`/workspace`）的所有者 UID/GID。若属于非 root 宿主机用户，将自动匹配其 UID/GID。
   - **动态调整本地普通用户**：启动时动态调整容器内普通用户（默认 `dev`）的 UID/GID，自动创建与配置 `$HOME`、`~/.nix-defexpr` 与 Nix 状态目录权限，并通过 `gosu` 切换至该用户执行后续操作，确保生成的构建产物权限与宿主机保持完全一致。
   - **root 权限安全封闭**：`/root` 目录默认保持严格的 `700` 私有权限，敏感文件对普通用户完全隔离不可见。
   - **root 运行控制**：若需要以 root 权限运行，传入 `HOST_UID=0` 或 `RUN_AS_ROOT=1` 即可。
3. **SSH 自动初始化**（当 `services.openssh.enable = true` 时）：
   - 自动生成 SSH Host Key（若缺失）。
   - 公钥注入：自动读取 `/tmp/id_ed25519.pub` 并配置为 `/root/.ssh/authorized_keys` 与 `/home/dev/.ssh/authorized_keys`。
   - 环境变量导出：将容器环境变量写入 `/root/.ssh/environment` 与普通用户对应目录，确保通过 SSH 登录时环境变量不丢失。
4. **服务与命令分发**：
   - 带有参数时：非 root 用户通过 `gosu` 切换权限执行传入命令（`exec gosu $TARGET_USER "$@"`，若显式启动 sshd 则保持 root 权限）。
   - 无参数且启用 SSH 时：默认前台以 root 启动 `sshd -D -e`（支持多用户登录）。
   - 无参数且未启用 SSH 时：默认启动交互式 Bash（非 root 用户通过 `gosu` 切换至对应本地用户）。

### 编写自定义 Dockerfile 示例

在派生镜像中，建议**保留 `/bin/entrypoint.sh` 作为 Entrypoint**，通过 `CMD` 或传入命令来扩展容器行为：

```dockerfile
FROM ghcr.io/shaogme/nixos-dockers/vscode-rust:latest

# 1. 设置自定义环境变量
ENV MY_CUSTOM_ENV="value"

# 2. 安装额外依赖或复制配置（可利用内置的 nix 安装工具）
RUN nix-env -iA nixpkgs.bun

# 3. 必须确保 Entrypoint 依然使用 /bin/entrypoint.sh
ENTRYPOINT ["/bin/entrypoint.sh"]

# 默认启动参数：留空则根据镜像类型自适应启动
CMD []
```

如果需要编写自定义的前置初始化脚本（例如 `custom-init.sh`），请在自定义脚本末尾通过 `exec /bin/entrypoint.sh "$@"` 将控制权移交给原入口脚本：

```bash
#!/usr/bin/env bash
set -e

# 执行你的前置初始化操作
echo "Running custom setup..."

# 移交给内置的 entrypoint.sh
exec /bin/entrypoint.sh "$@"
```

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
- `bash-wrapper`: 确保在通过 SSH 登录或交互终端时，`LD_LIBRARY_PATH` 等环境变量不会丢失。

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

## 项目结构

```text
.
├── images/                # Docker 镜像定义目录 (每个定义同时产出 CLI 与 VS Code 镜像)
│   ├── npins/             # 基础通用镜像 (npins, vscode-npins)
│   ├── rust/              # Rust 专用镜像 (rust, vscode-rust)
│   └── mise/              # Mise 专用镜像 (mise, vscode-mise) -> 详见 [Mise 文档](images/mise/README.md)
│       └── example/       # 生产级派生开发容器示例 (Dockerfile, compose, entrypoint)
├── modules/               # 统一 NixOS 模块系统
│   ├── core/              # 核心构建器、系统配置与动态 entrypoint
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
