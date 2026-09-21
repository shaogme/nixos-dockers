# dev-env

`dev-env` 是 `nixos-dockers` 使用的开发环境运行时。它用 Rust 读取 TOML profile，按继承关系和来源优先级合并配置，执行声明的 provider，并把同一份最终环境注入到命令、shell、SSH login shell 和兼容 shim 中。

它解决的是“一个进程应该看到什么环境”，不是容器基础设施初始化器。UID/GID、HOME、目录、软链接、SSH host key 和权限切换属于相邻的 [`container-init`](../container-init/README.md)；两者可以读取同一个 profile，但使用不同的顶层命名空间。

## 文档导航

- [配置与文件发现](docs/configuration.md)：镜像、管理员、用户、工作区和运行时配置的路径、优先级与覆盖方式。
- [Environment DSL 参考](docs/dsl.md)：schema v1 的完整 TOML 字段、类型、条件和显式 override 语义。
- [Provider 参考](docs/providers.md)：探测、prepare、shellenv、依赖、锁、输出解析和 provider 示例。
- [运行时与 CLI](docs/runtime.md)：`exec`、`shell`、`login-shell`、`shim`、`print`、`explain`、`doctor` 和 `trust`。
- [开发与测试](docs/development.md)：crate 分层、构建、测试、Docker harness 和源码导航。

## 核心模型

一次会话的主要数据流如下：

```text
profile TOML / overlay / runtime input / CLI
                    │
                    ▼
             ResolvedConfig
                    │
       detect → prepare → shellenv
                    │
                    ▼
             MaterializedEnv
             ┌──────┼──────┐
             ▼      ▼      ▼
           exec   shell   print
                    │
             login-shell / shim
```

`ResolvedConfig` 是加载器输出的、已经完成继承和严格合并的环境配置；`MaterializedEnv` 是在当前 workspace、cwd、用户和 shell 上执行 provider 后得到的进程环境。配置值和环境值都保留来源、敏感性及配置 fingerprint，便于 `explain` 和诊断工具审计。

`dev-env` 的边界是：

- 读取 `[config]`、`[inputs]`、`[override]` 和 `[policy]`；
- 按 `extends` 形成父到子的 profile chain；
- 生成环境变量和结构化 `PATH`；
- 通过 argv 调用通用 provider，不拼接 shell 命令；
- 解析受限的 shell、dotenv 或 JSON 环境输出；
- 以同一个 materializer 启动不同入口。

它不负责：

- 安装 Nix 包或构建 Docker layer；
- 创建/修改 POSIX 账户、挂载点、SSH key 或持久化目录；
- 自动读取任意环境变量作为配置；
- 通过 `eval` 执行 provider 返回的 shell 代码。

## Compose runtime inputs

Docker Compose 注入的环境变量只有在 profile 的 `[inputs]` 中声明为
`runtime = true` 时才会改变 `ResolvedConfig`；其他变量在
`inherit_process = true` 时只作为 ambient environment 继承。这一边界避免
Compose 中的任意变量意外改变镜像行为。

`HOST_UID` 是由 `container-init` 解析的宿主 namespace 输入，不应使用
`${HOST_UID:-1000:1000}` 作为通用默认值：rootless user namespace 可能没有覆盖宿主
GID 1000。Compose 未设置 `HOST_UID` 时会使用已验证的 workspace 挂载属主；需要显式
映射时请传入当前宿主机的实际值，例如 `HOST_UID=$(id -u):$(id -g)`。

Rust 镜像使用这一机制配置 Cargo 和 sccache：

```yaml
- CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
- CARGO_TARGET_DIR=/data/.cargo/target
- SCCACHE_DIR=/data/cache/sccache
- SCCACHE_DISABLE=${SCCACHE_DISABLE:-0}
- ENABLE_SCCACHE=${ENABLE_SCCACHE:-1}
```

`SCCACHE_DISABLE=1` 或兼容变量 `ENABLE_SCCACHE=0` 会关闭 sccache，并从
最终 `MaterializedEnv` 删除 `RUSTC_WRAPPER`；关闭信号不会只依赖 sccache
自身解释。具体 DSL 和优先级见 [Environment DSL 参考](docs/dsl.md) 与
[运行时与 CLI](docs/runtime.md)。

## 快速开始

### 在开发容器中使用

镜像构建层会把 runtime 安装到 `/usr/bin/dev-env`，把基础 profile 放到 `/etc/dev-env/profiles.d/00-nixos-docker.toml`，并写入 `/etc/dev-env/default-profile`。进入容器后，推荐所有新会话都显式经过 `dev-env`：

```bash
# 进入默认 shell
docker exec -it <container> dev-env shell

# 执行任意非 shell 命令
docker exec <container> dev-env exec -- cargo test

# 查看脱敏后的最终环境
docker exec <container> dev-env print --format json

# 查看 profile chain、fingerprint 和 provenance
docker exec <container> dev-env explain
docker exec <container> dev-env explain environment.variables.PATH

# 检查 workspace、shell 和 provider 是否可用
docker exec <container> dev-env doctor --json
```

镜像还可以把 `/bin/bash` 和 `/usr/bin/bash` 做成兼容 shim。root 启动的
`docker exec ... bash -lc ...` 会先重新进入 `container-init` 的身份 Bootstrap，
再物化环境并启动真实 Bash；已经以目标用户运行的 shell 只会物化环境：

```bash
docker exec -it <container> bash -lc 'printf "%s\\n" "$PATH"'
docker exec -it <container> /bin/bash -lc 'id && printf "%s\\n" "$HOME"'
```

需要 root 时显式声明：

```bash
docker exec -e RUN_AS_ROOT=1 -it <container> bash
```

`/usr/bin/dev-env-login-shell` 是 root SSH 使用的稳定 login shell；`/bin/sh` 和
`/usr/local/libexec/dev-env/real/bash` 是绕过 Bootstrap 的低层入口。直接使用
`docker exec ... /bin/sh` 不会自动运行 `dev-env`；需要开发环境时请使用
`dev-env exec -- sh ...`。

### 从源码构建

```bash
cd nixos-dockers/dev-env
cargo build --locked --release -p dev-env-cli
./target/release/dev-env --help
```

源码 workspace 没有默认 profile 文件；本地运行时必须提供一组 profile 和默认 profile，或者使用镜像中已经安装的 `/etc/dev-env`：

```bash
./target/release/dev-env \
  --profiles-dir /path/to/profiles.d \
  --default-profile-file /path/to/default-profile \
  print --format json
```

最小 profile 和字段说明见 [DSL 参考](docs/dsl.md)。快速验证 Rust workspace 本身：

```bash
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked
```

## 入口一致性

容器的默认 `Entrypoint` 是 `/usr/bin/container-init run --`。`container-init` 在 handoff 前完成 UID/GID namespace 解析、账户和降权，并把规范化的进程身份交给 `dev-env`；环境 provider 不在 `container-init` 中执行。

| 使用场景 | 推荐入口 | 环境来源 |
| --- | --- | --- |
| Docker 默认命令 | `container-init` → `dev-env exec/shell` | 当前用户、cwd、profile、provider |
| `docker exec` 非 shell 命令 | `dev-env exec -- <command>` | 重新解析当前会话 |
| `docker exec` 交互 shell | `dev-env shell` | 重新解析当前会话 |
| root 的 Bash 调用 | `/bin/bash` 或 `/usr/bin/bash` shim | Bootstrap 身份、再物化并转发原始 argv |
| 非 root 的 Bash 调用 | `/bin/bash` 或 `/usr/bin/bash` shim | 直接物化并转发原始 argv |
| 显式 root | `RUN_AS_ROOT=1 ... bash` | 保留 root，仍物化环境 |
| SSH 登录 | `/usr/bin/dev-env-login-shell` | 重新解析 SSH 用户的环境 |
| 仅查看 | `print` / `explain` / `doctor` | 不启动目标 shell；`explain`/`doctor` 不执行 provider |

child process 由 `CommandLine` 使用 `env_clear()` 后注入 `MaterializedEnv`。因此不同入口不会依赖某一次 entrypoint 对当前 shell 的临时修改，也不会把动态 provider 环境写入 `/etc/environment`。

## 当前镜像中的 profile

基础 NixOS Docker runtime 在 [`modules/core/runtime.nix`](../modules/core/runtime.nix) 生成 `nixos-docker` profile，提供 workspace `/workspace`、Bash 定义、Nix 相关变量和基础 PATH。coding-images 派生镜像通过额外的 profile 文件增加 provider 和运行时变量：

| profile | 继承 | 主要内容 |
| --- | --- | --- |
| `nixos-docker` | — | 基础 shell、PATH、Nix 环境和 bootstrap handoff |
| `coding-images` | `nixos-docker` | mise、Devbox provider、共享数据目录变量 |
| `coding-images-podman` | `coding-images` | Podman runtime 和容器数据目录 |
| `coding-images-rust` | `coding-images` | Rustup、pnpm、sccache 和 Rust 环境 |
| `coding-images-rust-wasm` | `coding-images-rust` | Fontconfig、headless 图形相关变量 |
| `coding-images-qemu` | `coding-images-podman` | QEMU 数据目录；`/dev/kvm` 权限由容器运行时设备配置提供 |
| `coding-images-qemu-rust` | `coding-images-qemu` | QEMU + Rust 环境 |

实际的派生 profile 示例位于 [`images/common/.config/dev-env.toml`](../../images/common/.config/dev-env.toml)、[`images/rust/common/.config/dev-env.toml`](../../images/rust/common/.config/dev-env.toml) 等文件中。Dockerfile 负责安装工具和复制 profile；provider 的运行时行为由 profile 声明。

## Rust workspace 结构

```text
dev-env/
├── Cargo.toml
└── crates/
    ├── dev-env-model/     # schema、值树、条件、输入、来源和静态校验
    ├── dev-env-loader/    # TOML 读取、profile graph、overlay 和严格合并
    ├── dev-env-core/      # RuntimeContext、Materializer、PATH 和 provider 编排
    ├── dev-env-provider/  # provider 探测、argv 执行、输出解析、锁和 receipt
    ├── dev-env-shell/     # shell argv、环境格式化和 shim
    └── dev-env-cli/       # CLI 参数、配置发现、进程边界和诊断命令
```

各 crate 刻意保持边界：model 不读文件也不启动进程，loader 不执行 provider，core 不解析 TOML，shell crate 不执行 shell source，CLI 只负责把这些组件接到进程边界。

## 实现状态和已知边界

当前源码已经实现：

- schema v1 的环境模型与校验；
- profile 继承、循环/缺失父 profile 检查和严格冲突诊断；
- admin、user、workspace、runtime 和 CLI 的加载层；
- bool、enum、integer、path、string 类型输入及 alias；
- provider 的文件探测、依赖排序、prepare、shellenv、超时、锁和 receipt；
- shell、dotenv、JSON 输出解析与敏感值脱敏；
- `exec`、`shell`、`login-shell`、`shim`、`print`、`explain`、`doctor`、`trust`；
- model、loader、core、provider、shell、CLI 的单元测试，以及 Linux Docker 真实进程测试。

需要留意的当前边界：

- `trust` 命令当前只把文件或 SHA-256 哈希追加到 trust store；现有 CLI loader 尚未把该 store 作为 workspace 配置的自动准入门禁。实际能否覆盖配置，仍由 profile 的 policy 和来源权限检查决定。
- 外部 provider 协议的编解码类型已经在 `dev-env-provider` 中定义，但默认 `ProviderRunner` 当前运行的是通用 executable + argv provider，不会自动发现或启动 `dev-env-provider-*`。
- `untrusted_workspace` 已进入模型并可配置，但当前 environment CLI 主要通过来源类别和 allow-list 拒绝不允许的覆盖，不实现交互式 prompt。
- `/bin/bash` shim 的真实 shell 路径必须与 shim 路径分离；否则会触发递归或启动错误。
