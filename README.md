# NixOS Docker Images for VS Code Remote 🚀

[![Docker Publish](https://github.com/shaogme/nixos-dockers/actions/workflows/docker-publish.yml/badge.svg)](https://github.com/shaogme/nixos-dockers/actions/workflows/docker-publish.yml)
[![Auto Update Npins](https://github.com/shaogme/nixos-dockers/actions/workflows/auto-update-npins.yml/badge.svg)](https://github.com/shaogme/nixos-dockers/actions/workflows/auto-update-npins.yml)

一套基于 **Nix** 构建的轻量级、高性能 Docker 镜像，专为 **VS Code Remote / Dev Containers** 优化。

## ✨ 特性

-   **Nix-Powered**: 利用 Nix 的声明式管理，确保镜像环境的精确一致性。
-   **VS Code 优化**:
    -   内置 `nix-ld` 支持，完美运行 VS Code Server 及其各类扩展（如 Copilot）。
    -   遵循 FHS 标准的软链接，解决非 Nix 二进制程序的依赖问题。
-   **开箱即用**:
    -   内置 SSH 服务，支持远程连接。
    -   包含 `direnv` 和 `nix-direnv`，实现项目环境自动切换。
    -   集成常用开发工具（gcc, git, curl, vim 等）。
-   **自动化运维**:
    -   **每日更新**: 每天凌晨 3:00 (北京时间) 自动同步 `npins` 依赖。
    -   **持续交付**: 每次代码推送自动构建并发布至 GHCR。

## 📦 镜像列表

| 镜像名称 | 描述 | 主要包含 |
| :--- | :--- | :--- |
| `vscode-npins` | 基础开发镜像 | Nix, npins, direnv, coreutils, SSH |
| `vscode-rust` | Rust 专用开发镜像 | Rust 工具链 (cargo, rustc), rust-analyzer, clippy, gdb |

## 🚀 快速开始

### 1. 使用 Docker 直接运行

```bash
docker run -d \
  --name nix-dev \
  -p 2222:22 \
  -v $(pwd):/root/workspace \
  ghcr.io/shaogme/nixos-dockers/vscode-rust:latest
```

### 2. 使用 Docker Compose

```yaml
services:
  nix-dev:
    image: ghcr.io/shaogme/nixos-dockers/vscode-rust:latest
    ports:
      - "2222:22"
    volumes:
      - .:/root/workspace
    restart: unless-stopped
```

### 3. 连接到开发环境

-   **SSH**: `ssh root@localhost -p 2222` (默认空密码)
-   **VS Code**: 安装 `Remote - SSH` 扩展，添加主机 `localhost:2222` 即可。

> [!TIP]
> **注入公钥**: 你可以将你的 `id_ed25519.pub` 挂载到 `/tmp/id_ed25519.pub`，镜像启动时会自动将其添加到 `/root/.ssh/authorized_keys`。
>
> ```yaml
> volumes:
>   - ~/.ssh/id_ed25519.pub:/tmp/id_ed25519.pub:ro
> ```

## 🛠️ 技术细节

### 为什么选择 Nix 构建镜像？

1.  **极小体积与分层优化**: 使用 `buildLayeredImage` 自动提取依赖图并构建最优分层，避免了传统 Dockerfile 中大量的 `apt-get` 冗余。
2.  **环境一致性**: 所有的依赖版本都由 `npins` (nixpkgs) 锁定，确保在任何机器上构建的结果完全一致。
3.  **内建 nix-ld**: 解决了 VS Code Server 在 Nix 环境下无法直接运行二进制扩展（如 Copilot, C++ Intellisense）的痛点。

### 关键组件

-   `nix-ld`: 动态链接器封装，自动为非 Nix 二进制程序寻找所需的 `.so` 文件。
-   `direnv`: 进入目录时自动加载 `shell.nix` 或 `flake.nix` 环境。
-   `bash-wrapper`: 确保在通过 SSH 登录时，`LD_LIBRARY_PATH` 等环境变量不会丢失。

## 📂 项目结构

-   `.github/workflows/`: GitHub Actions 自动化脚本。
-   `vscode-npins/`: 基础开发镜像定义（包含 Nix, npins, direnv）。
-   `vscode-rust/`: Rust 专用开发镜像（在基础镜像之上增加 Rust 工具链）。
-   `update-npins.sh`: 依赖自动更新脚本。

## ⚙️ 环境变量

镜像内置了以下关键环境变量以确保环境正常运行：
-   `NIX_LD_LIBRARY_PATH`: 提供非 Nix 程序的动态链接库路径。
-   `RUST_SRC_PATH`: Rust 源码路径（针对 `vscode-rust`）。
-   `PATH`: 包含 `/bin`, `/usr/bin`, `/usr/local/bin`。

## 🤝 贡献

欢迎提交 Issue 或 Pull Request 来改进这些镜像！

## 📄 开源协议

[MIT](LICENSE) © [shaogme](https://github.com/shaogme)
