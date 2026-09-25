# NixOS Docker Images for VS Code Remote

[![Docker Publish](https://github.com/shaogme/nixos-dockers/actions/workflows/docker-publish.yml/badge.svg)](https://github.com/shaogme/nixos-dockers/actions/workflows/docker-publish.yml)
[![Auto Update Npins](https://github.com/shaogme/nixos-dockers/actions/workflows/auto-update-npins.yml/badge.svg)](https://github.com/shaogme/nixos-dockers/actions/workflows/auto-update-npins.yml)

一套基于 **Nix** 构建的轻量级、高性能 Docker 镜像，专为 **VS Code Remote / Dev Containers** 优化。

## 特性

- **Nix-Powered**: 利用 Nix 的声明式管理，确保镜像环境的精确一致性。
- **VS Code 优化**:
  - 内置 `nix-ld` 支持，完美运行 VS Code Server 及其各类扩展（如 Copilot）。
  - 遵循 FHS 标准的软链接，解决非 Nix 二进制程序的依赖问题。
- **自适应 UID/GID 映射**: 仅在 mountinfo 证明工作区确实为挂载点时探测挂载视角下的属主；普通 rootfs 目录回退到 profile 默认身份。`HOST_UID`/`HOST_GID` 显式输入会按声明的 host namespace 转换，兼容 Rootless Podman/Docker，并由 `container-init` 声明式降权。
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

### 3. Podman 引擎镜像（单一 engine 产物）

| 镜像名称 | 描述 | 主要包含 | 示例 Tag |
| :--- | :--- | :--- | :--- |
| `podman` | Compose 使用的 rootful Podman 服务 | Podman, crun, conmon, fuse-overlayfs, netavark | `latest`, `5.8.7-2026.9.25` |

`nixos-dockers/podman` 不包含 SSH、`container-init`、`dev-env` 或开发工具，只运行
Podman API。它通过 `/run/podman/podman.sock` 提供 Unix socket，存储目录固定在
`/var/lib/containers`；应当与开发工具容器共享 socket、数据卷和 `/workspace` 路径。
引擎只通过 Compose 的 `podman` 服务启动，不监听 TCP；`PODMAN_SOCKET_GID` 必须与
开发容器的有效 GID 一致。
引擎以 UID/GID `0:0` 运行 rootful Podman，但宿主容器必须使用私有 cgroup namespace、
保持 cgroups enabled，并授予 `SYS_ADMIN`、`MKNOD`、网络和 `/dev/fuse` 等测试所需的 capability。
镜像入口会在私有 namespace 内将 Docker 默认的只读 cgroup2 挂载重新挂载为可写，以便
crun 启用 controller 并创建子 cgroup。不要使用特权模式或宿主 cgroup namespace。
镜像自身包含 `/etc/ssl/certs/ca-bundle.crt`，并通过 `SSL_CERT_FILE` 与
`NIX_SSL_CERT_FILE` 指向该 bundle；registry TLS 校验不依赖宿主机证书挂载，也不会关闭
证书验证。

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
  -v $(pwd):/workspace \
  ghcr.io/shaogme/nixos-dockers/vscode-rust:latest
```

### 2. 使用 Docker 运行通用 CLI 镜像（无 SSH）

```bash
# 运行 mise 镜像
docker run -it --rm \
  -v $(pwd):/workspace \
  ghcr.io/shaogme/nixos-dockers/mise:latest

# 或运行 rust 镜像
docker run -it --rm \
  -v $(pwd):/workspace \
ghcr.io/shaogme/nixos-dockers/rust:latest
```

### 3. 使用独立 Podman engine

`coding-images/podman` 及其 Rust/QEMU 派生镜像必须使用双服务 Compose：

```bash
cd images/podman
PODMAN_SOCKET_GID=$(id -g) docker compose up -d podman dev
docker compose exec dev bash
```

`dev` 与 `podman` 都挂载 `/workspace`，并通过 `podman-socket` 共享
`CONTAINER_HOST=unix:///run/podman/podman.sock` 和 `DOCKER_HOST`。`podman-data` 只挂载
到 engine；旧的 `/var/lib/containers` 工具容器卷不会自动复用。

### 3. 使用 Docker Compose (VS Code Remote)

```yaml
services:
  nix-dev:
    image: ghcr.io/shaogme/nixos-dockers/vscode-rust:latest
    environment:
      # 已确认 /workspace 为挂载点时自动探测属主；需要显式覆盖时传入真实宿主 ID：
      # - HOST_UID
    ports:
      - "2222:22"
    volumes:
      - .:/workspace
      - cargo:/data/cargo
    restart: unless-stopped

volumes:
  cargo:
```

> [!TIP]
> **分离用户家目录与自适应权限**：
> 非 root 开发用户使用 `/home/dev`，root 使用 `/root` 作为 `$HOME`。
> 启动时容器引导层（`container-init`）会根据目标身份校准对应家目录及其私有配置目录的所有权和权限。
> 若需切换为 root 身份运行，只需在启动时传入环境变量：
>
> ```bash
> RUN_AS_ROOT=1 docker compose up -d
> ```
>
> Codex、Claude、Gemini 和 OpenCode 配置在两个 HOME 下保持相同的共享链接；Rust Cargo registry 和 git checkout 统一持久化在 `/data/cargo`，并从两个 HOME 的 `.cargo` 目录链接过去。

不要使用 `${HOST_UID:-1000:1000}` 作为通用默认值。`HOST_UID`/`HOST_GID` 按宿主
namespace 映射，rootless 容器可能映射宿主 UID 1000 但未映射 GID 1000；未设置时让
workspace 挂载属主自动解析，需要覆盖时请传入 `$(id -u):$(id -g)`。

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

1. `container-init` backend 加载镜像 profile snapshot，执行声明的 UID/GID、目录、软链接和 SSH action，然后按 handoff 配置交给 `dev-env`。
2. `dev-env` 加载 `/etc/dev-env/profiles.d`，物化 mise、Devbox、Rust 和其他 provider 的环境，并以同一份环境启动命令、shell 或 SSH login shell。

`HOST_UID=uid[:gid]`、`HOST_GID`、`CONTAINER_HOME` 和 `RUN_AS_ROOT=1` 是声明式 runtime input。未设置 `CONTAINER_HOME` 时，普通开发用户使用 `/home/dev`，root 使用 `/root`；`container-init` 会在启动期原生校准对应家目录的所有权和权限。`/bin/bash` 与 `/usr/bin/bash` 是兼容 shim；root 的 `docker exec ... bash` 会通过 backend `container-init exec` 请求身份 reconciliation，非 root 则直接物化环境，真实 Bash 位于 `/usr/local/libexec/dev-env/real/bash`。需要 root 身份时直接传入 `RUN_AS_ROOT=1`。镜像不再包含旧的 `/bin/entrypoint.sh`。

### 编写自定义 Dockerfile 示例

派生镜像无需重新声明 Entrypoint 或 CMD；只需安装工具并增加 profile：

```dockerfile
FROM ghcr.io/shaogme/nixos-dockers/vscode-rust:latest

RUN nix profile add nixpkgs#bun
COPY .config/dev-env.toml /etc/dev-env/profiles.d/50-project.toml
```

若需要在 Docker 构建阶段执行 `mise lock` 或 `mise install`，必须使用独立的
`mise-builder` stage，并只将工具缓存复制到 runtime stage：

```dockerfile
ARG NIXOS_DOCKERS_VERSION

FROM ghcr.io/shaogme/nixos-dockers/mise-builder:${NIXOS_DOCKERS_VERSION} AS mise-tools
COPY .mise.toml /etc/mise/mise.toml
COPY conf.d/ /etc/mise/conf.d/
RUN mise trust --all \
    && mise lock --global --platform linux-x64,linux-arm64 \
    && mise install \
    && chmod -R a+rwX /etc/mise /usr/local/share/mise /data/cache/mise

FROM ghcr.io/shaogme/nixos-dockers/mise:${NIXOS_DOCKERS_VERSION}
COPY --from=mise-tools /etc/mise /etc/mise
COPY --from=mise-tools /usr/local/share/mise /usr/local/share/mise
COPY --from=mise-tools /data/cache/mise /data/cache/mise
```

运行时 stage 不执行工具安装命令，也不复制 builder 的 `/root`、临时目录或构建凭据。
`mise-builder` 与 runtime image 使用相同版本标签，并由 CI 分别发布 amd64 和 arm64
manifest。

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
- `dev-env` Bash shim：root 的 `docker exec` 先复用 `container-init` 身份 Bootstrap，再与 SSH 登录、交互终端共享同一份物化环境。
- `/etc/gitconfig`: 构建期声明 Git `safe.directory`，保障容器工作区跨 UID/GID 权限时正常执行 Git 操作。

## 本地构建镜像

开发镜像目录（如 `images/rust`）同时支持标准 CLI、VS Code Remote 和 builder 产物；
Podman engine 目录只生成一个 `podman` 产物：

```bash
# 1. 构建 Rust 通用 CLI 镜像
nix-build images/rust/image.nix -A rust

# 2. 构建 VS Code Remote Rust 镜像 (含 SSH)
nix-build images/rust/image.nix -A vscode-rust

# 3. 构建该目录下所有镜像变体
nix-build images/rust/image.nix

# Mise 构建阶段镜像（仅用于 Docker build stage）
nix-build images/mise/image.nix -A mise-builder

# Podman engine 服务镜像（无 SSH、无 dev-env）
nix-build images/podman/image.nix -A podman
```

构建完成后，使用 `docker load < result` 即可将镜像导入本地 Docker。

### 本地 Docker 集成测试

每个 image 都提供一套 Docker 测试脚本。脚本会构建 CLI 与 VS Code Remote 两个变体，加载镜像，并验证 `container-init` 计划、`dev-env` 环境物化、运行时 handoff、登录 shell shim、root `docker exec` Bash 身份 Bootstrap；VS Code 变体还会验证默认 SSH 服务能够部署并保持运行：

```bash
bash images/rust/tests/docker.sh
bash images/npins/tests/docker.sh
bash images/mise/tests/docker.sh
bash images/podman/tests/docker.sh
```

CI 对开发镜像运行 container-init/dev-env 测试，对 `podman` 单独运行 socket、远程 API
和持久化数据测试。`coding-images` 暂不纳入本次迁移。

## 项目结构

```text
.
├── images/                # Docker 镜像定义目录 (按 role 产出开发或 engine 镜像)
│   ├── npins/             # 基础通用镜像 (npins, vscode-npins)
│   ├── rust/              # Rust 专用镜像 (rust, vscode-rust)
│   ├── podman/            # 独立 Podman engine 镜像 (仅 podman)
│   └── mise/              # Mise 专用镜像 (mise, vscode-mise, mise-builder) -> 详见 [Mise 文档](images/mise/README.md)
│       └── example/       # 生产级派生开发容器示例 (Dockerfile, compose, entrypoint)
├── modules/               # 统一 NixOS 模块系统
│   ├── core/              # 核心构建器、系统配置与 container-init/dev-env runtime
│   └── profiles/          # 语言、工具与 engine Profile (base, rust, npins, mise, podman)
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
