# 测试迁移到 QEMU：执行矩阵与拆迁计划

## 决策与目标

把 workspace 检查、Docker 集成 fixture 和 Nix 镜像运行测试迁移到 NixOS QEMU guest 中执行。CI runner 只负责构建/启动 VM、传入 checkout、收集日志和返回状态；Cargo、Docker、Podman、Nix 镜像构建及测试命令都在 guest 内运行。已有 `nixosTest` 自己创建 QEMU guest，继续由 NixOS 测试框架驱动，不要再把它套进外层 VM。

rootful Docker 与 rootless Podman 是两种不同能力的测试环境，不是可互换的 Docker 实现：

- **rootful Docker guest** 是 POSIX 权限、真实降权、`--privileged`、`SYS_ADMIN`、设备、挂载、可写 cgroup v2 和 Docker 镜像运行行为的规范环境。workspace、crate suite 和镜像集成目标都在这里运行。
- **rootless Podman guest** 必须运行完整 crate Docker suite 和 Podman engine 镜像 fixture。七个 crate harness 在同一 guest 中顺序执行，不为每个 crate 单独启动 VM；测试必须尝试 cgroup、mount 和 PID namespace 操作。只有操作在 rootless Podman VM 中实际失败并返回权限错误时，才可报告为 `UNSUPPORTED`；按 runtime 预先判断并跳过不算验证。
- **workspace suite** 不验证容器引擎差异，只需在一个固定 NixOS guest 中完整执行一次。重复跑到两种 VM 不增加有意义的 runtime 覆盖。

rootless lane 运行全部 crate test binaries。只有在 rootless Podman VM 中执行后确实收到权限错误的断言才能报告 `UNSUPPORTED`；这不证明 rootful 语义已覆盖。rootful lane 对必需断言仍要求实际通过。

## 迁移前实现与依据

以下条目记录本计划开始实施时的基线；迁移后的行为以目标矩阵和当前源码为准。

当前 CI 和测试入口的事实如下：

- `.github/workflows/ci.yml` 在 runner 上执行 `cargo fmt` 和 Clippy；workspace 测试由容器运行。11 个 Docker 目标由 `docker-test` matrix 直接调用各自脚本。
- `scripts/test-workspace.sh` 使用调用方 Docker 构建 `scripts/Dockerfile.test`，再以 `--privileged` 启动容器。workspace Cargo suite 默认关闭 `docker-tests` feature，本身不需要 `--privileged`。
- 7 个 crate Docker harness 构建各自的 `Dockerfile.test` 并在内层容器执行 `tests/docker.rs`。其中 `container-init-core` 使用 `--privileged --device /dev/fuse`，`container-init-cli` 使用 `--cap-add=SYS_ADMIN --device /dev/fuse`；POSIX fixture 明确要求 root。它们验证真实 Linux 进程、身份、权限、cgroup 或 PID namespace 行为。
- `images/mise`、`images/npins`、`images/rust` 调用 `tests/docker-image.sh`：先做 Nix 静态评估和镜像构建，再 load 镜像并检查 `container-init`、`dev-env`、entrypoint 和 handoff。`mise` 还构建派生镜像并检查 builder artifact。
- `images/podman/tests/docker.sh` 目前依次启动 rootless Podman 和 rootful Docker 两个 QEMU VM。两个外层脚本各自复制 checkout，再在 guest 中运行 `common.sh`。这两份 NixOS 配置是 `images/podman/tests/podman-rootless-image.nix` 和 `images/podman/tests/docker-image.nix`。这是目前唯一已有的双引擎 Docker CLI 兼容测试。
- `container-init-core/tests/docker.rs` 和 `container-init-cli/tests/docker.rs` 会在缺少可写 cgroup、控制器委托或 SSH capability 时提前返回；core 的 bind-mount 测试也会因执行身份不适用而返回。当前这些情况会表现为普通的 `ok`，不能证明对应路径被覆盖。
- `tests/container-init-reaper-vm.nix` 已是独立的 NixOS VM test，但当前 `.github/workflows/ci.yml` 没有运行它。
- [`tests/runtime-static.nix`](../../tests/runtime-static.nix) 被镜像测试脚本用于验证 runtime 模块选项与环境变量；它应继续留在镜像测试覆盖内。

## 目标执行矩阵

| Suite / target | rootful Docker VM | rootless Podman VM | 验收范围 |
| --- | --- | --- | --- |
| Workspace（Clippy、worker build、workspace tests、cargo cop） | 必跑 | — | 在 guest 中直接用统一的 Rust 1.88.0 toolchain 执行；Cargo tests 使用 `--no-default-features` 且单线程运行，不编译 Docker fixture。 |
| Crate Docker suite（以上七个 crate harness 顺序运行） | 必跑，一台 VM | 必跑，一台 VM | 两种 guest 都执行全部 test binaries；rootless 内实际权限失败的 `UNSUPPORTED` 输出不替代 rootful 必需断言。 |
| `mise` | 必跑 | — | Nix 静态评估、基础/VS Code 镜像、派生镜像和 runtime handoff；mise derivation 必须从 NixCache 替换，禁止 guest 源码构建。 |
| `npins` | 必跑 | — | Nix 静态评估、镜像构建和 runtime handoff。 |
| `rust` | 必跑 | — | Nix 静态评估、镜像构建和 runtime handoff。 |
| `podman` | 必跑 | 必跑 | 相同的 guest-side engine image fixture 分别通过 guest Docker daemon 与 rootless Podman Docker CLI 兼容入口运行。 |
| `container-init-reaper-vm` | NixOS `nixosTest` 独立 QEMU guest | — | 运行现有 `tests/container-init-reaper-vm.nix`，验证 backend 运行期间收养并回收子进程。 |

Docker 目标组合总计为 11 个 rootful 和 8 个 rootless 执行（七个 crate fixture 加 `podman` image fixture）。crate fixtures 在每种 runtime 下聚合到一个 job/VM；workspace、三个开发镜像 target、两个 `podman` runtime 结果和独立 `nixosTest` 保持单独可见。CI job 名必须保留 suite/target/runtime 标识。

## VM 与 launcher 设计

### NixOS guest

保留两种明确分开的 guest 配置，按需抽取共享 SSH、工具包和磁盘设置，不要把不同的权限/cgroup 需求压进一份由条件分支控制的“大而全”模块。

- **rootful Docker**：通用测试 VM 配置位于 `tests/vms/rootful-docker.nix`。启用 Docker daemon、overlay2、cgroup v2、FUSE、网络转发、guest 内 Nix 和统一 Rust toolchain。guest 内的 Docker harness 以 rootful daemon 运行。
- **rootless Podman**：VM 配置位于 `tests/vms/rootless-podman.nix`，保留固定用户、subuid/subgid、linger、Nix daemon、rootless storage、`fuse-overlayfs`、`crun`、FUSE/TUN 和 Docker CLI 兼容设置。guest-side test dispatcher 以固定的非 root 用户运行 crate suite 与 Podman image fixture，并断言 UID/GID、`podman info` 的 rootless 状态、GraphRoot 和写权限。
- 两者都从现有 npins nixpkgs pin 导入 NixOS；不手改生成的 `default.nix` 或 `sources.json`。Cargo toolchain 必须统一为一个明确版本：当前 runner 使用 `stable`，而 crate Dockerfile 使用 `rust:1.85-bookworm`；本次选择 Rust 1.88.0，并同步 guest、workspace 和 Dockerfile harness，以支持 guest 内安装 cargo-cop。
- rootful VM 必须明确提供测试需要的内核模块与 cgroup v2。容器内的 `--privileged`、capability 和 device 只由该 VM 的 guest Docker daemon 提供；不能把 Docker socket 暴露给 CI runner。

### Host launcher 与 guest dispatcher

新增唯一的 host-side QEMU 入口 `scripts/qemu-test.sh <suite> [--runtime ...]`。suite 和 runtime 使用 allowlist，不接收任意 shell 字符串。crate suite target `crates` 在 guest 内按固定顺序运行七个 crate harness。host launcher 负责：

1. 用 npins pin 评估并构建所选 VM 的 qcow2 与 NixOS VM runner；workspace job 可复用同一 rootful guest 配置。
2. 为每次运行创建独立临时目录、磁盘 overlay、随机/动态分配的 SSH 转发端口和日志路径；不使用当前 Podman 脚本中的固定默认端口作为并行 CI 的唯一端口。
3. 使用临时 SSH key 登录 guest，等待 guest 与所选引擎就绪，复制当前 checkout（包括本地未提交文件），然后调用 allowlisted guest dispatcher。
4. 返回 guest 的真实退出码；失败时收集测试输出、Docker/Podman 信息、systemd 状态和完整 QEMU 日志，最后可靠清理 VM。失败日志应作为 CI artifact 保存。

guest dispatcher 只允许执行一个已登记的 suite/target，并负责设置运行用户、切入仓库目录和调用 guest-side harness。它不得启动 QEMU、访问 runner Docker socket 或自行选择任意脚本路径。镜像 Nix build 与被测容器的 Docker/Podman 调用也必须从 guest 内发起。

`mise` lane 使用 CI setup action 启动的 NixCache proxy。host launcher 通过临时 TCP relay 将 runner 的 loopback proxy 暴露给 QEMU user network；guest 在 `max-jobs=0` 下先单独替换 mise derivation，再恢复常规构建设置执行镜像 fixture。缓存缺少该 derivation 时 lane 立即失败，不允许在 VM 中编译 mise。

crate harness 继续保留 crate 的 `Dockerfile.test`：这些容器定义了 fixture 要求的 Linux 进程边界。workspace suite 改为在 rootful VM guest 直接运行，不再为不需要容器隔离的普通 Cargo tests 构建一个 `--privileged` 测试容器。`container-init` 与 `dev-env` 的 `docker-tests` feature 仍只由相应 crate Dockerfile 显式启用。

`podman` 目标只保留一份 guest-side `common.sh` 测试体。rootful Docker 和 rootless Podman VM 都调用它；两个现有 host-side QEMU 脚本不再嵌套启动 VM。rootless guest 仍调用 `docker` 兼容入口来覆盖项目当前配置，不要求把 Podman test body 改写成第二套测试。

## cgroup 与 capability 结果必须显式

将环境不满足的情形从静默 `return` 改为可审计结果。对 rootful Docker lane：

- 启动前检查并记录 cgroup v2、必需控制器、`/sys/fs/cgroup` 可写、`/dev/fuse` 和 `CAP_SYS_ADMIN`/挂载能力；缺失时该 lane 失败。
- `container-init-core` 和 `container-init-cli` 的 cgroup 主路径、控制器委托和 bind-mount 断言必须在 CI 中实际执行。SSH fixture 要求 `ssh-keygen` 可用，否则失败。
- 如果测试同时覆盖了 rootful 必需断言和可选分支，报告应逐项列出执行状态。任何被 CI 要求覆盖的断言都不能用 `return` 或测试成功状态表示跳过。

rootless Podman lane 运行七个 crate fixture 与 `images/podman`。crate 测试必须先执行相应操作；只有 rootless Podman VM 实际返回权限错误时，才将该断言报告为 `UNSUPPORTED`。不可根据 runtime、UID、capability 或 cgroup 属性直接推断并跳过；此报告也不能替代 rootful 语义覆盖。rootless 可执行的 fixture 行为仍须真实通过。

## CI 改造

重构 [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)：

- `workspace-test` 安装 Nix/QEMU host 工具并经共享 launcher 启动一次 rootful guest。guest 中依次执行 Clippy、`dev-env-provider-worker` 构建、`cargo test --workspace --locked --no-default-features` 和 `cargo cop --all-targets --all-features --workspace`。命令不得在 runner 执行。
- `docker-test` 使用显式 `include` matrix：`crates` 聚合 suite 分别配 `rootful-docker` 和 `rootless-podman`，`mise`、`npins`、`rust` 只配 `rootful-docker`，`podman` 分别配两种 runtime。不要用全笛卡尔积或按 crate 创建独立 VM。
- `mise` matrix 项在 runner 上启动 NixCache setup action，并将 loopback proxy 端口和公钥交给 QEMU launcher；guest 必须先验证 mise package 可由 cache 替换，禁止 VM 内源码构建该 package。
- 独立 job 构建并运行 `tests/container-init-reaper-vm.nix` 的 NixOS test driver。它本身启动 QEMU guest，不要从 Docker guest 内再运行它。
- 所有 matrix job 都安装 Nix 与 QEMU host 工具，调用共享 launcher，不安装 runner Rust toolchain，不在 runner 启动 Docker/Podman daemon。
- `fail-fast: false`；设置 `max-parallel` 控制 QEMU/Nix 的资源峰值。`ci-success` 汇总 workspace、crate suite 的两个 runtime、每个 image target/runtime 和 NixOS VM test，任何失败或取消都使总状态失败。
- job 名、日志目录和 artifacts 标注 suite、target、runtime；保留失败 guest 输出与 QEMU 日志。成功日志只需保存便于追溯的摘要。

并行按 suite/runtime 执行；七个 crate fixtures 在各自 runtime guest 内顺序运行。每个 job 都会重建或复用 Nix VM closure，实施时用 Nix store/cache 降低重复构建成本。

## 破坏性迁移

采用单一受支持入口，移除绕过 QEMU 的旧用法：

- `scripts/test-workspace.sh` 改为 guest-only harness 或删除；删除其普通 workspace 测试专用 `scripts/Dockerfile.test`。本地 workspace 命令改由 QEMU launcher 发起。
- 各 crate/image 的 `tests/docker.sh` 变成 guest-only harness，不再是宿主机可直接运行的 QEMU/Docker 编排器。直接在宿主机调用它们不再受支持；只有 dispatcher 设置的 guest 标识存在时才允许运行。
- `images/podman/tests/podman-rootless-test.sh` 和 `podman-docker-test.sh` 删除 QEMU 生命周期代码；将其断言移入 guest dispatcher/common fixture。保留两个 NixOS guest 配置作为共享测试 VM 来源或迁移到 `tests/vms/`。
- 移除固定 SSH 端口、root 密码认证和各脚本私有的 QEMU lifecycle 变量。host launcher 统一创建短期凭证和隔离目录。
- README 与 development 文档不再把直接 Cargo/Docker harness 命令描述为受支持 CI 等价验证入口。统一给出 QEMU launcher 命令，并说明如何用 nixos-test driver 调试现有 reaper VM test。

## 实施顺序

1. **锁定 runner 合同**：固定 suite/target/runtime allowlist、guest 输出与退出码格式、checkout 传输方式和 failure artifact 内容。
2. **抽出 VM 配置并建立启动探针**：分别评估/构建 rootful 和 rootless guest；确认 Docker daemon、rootless Podman、SSH、Nix 和 cgroup 基线。
3. **实现共享 host launcher 和 guest dispatcher**：一次只启动一个目标；验证无固定端口冲突、QEMU 退出清理、guest 非零退出码传播和失败日志保留。
4. **迁移 workspace suite 与 rootful fixture**：workspace 直接在 VM guest 运行；将七个 crate harness 和四个 image target 通过 rootful Docker guest 接入。
5. **迁移 Podman 双引擎 fixture**：移除其嵌套 VM 生命周期代码，让同一 guest-side 测试体分别由 rootful Docker 与 rootless Podman guest 调用。
6. **把 reaper nixosTest 接入 CI**：保留独立测试 driver，不嵌套 QEMU。
7. **切换 CI 和文档，删除旧入口**：启用明确的 matrix、总状态汇总、失败 artifacts，更新根 README、两个产品 README/development 文档和 `docs/npins/testing.md`。

## 验收条件

- rootful、rootless 两种 VM 配置均通过 `evalConfig`/严格评估，并可构建 guest toplevel、qcow2 与 VM runner；`nixosTest` driver 可构建。
- workspace suite 全部在 rootful guest 内执行，命令和 Cargo 输出没有 runner-side Cargo 运行记录；不使用 `--privileged` workspace 容器。
- 所有 crate Docker tests 在单个 rootful Docker VM 和单个 rootless Podman VM 中分别完整运行；七个 crate harness 在各 VM 内顺序执行。总计 11 个 rootful 与 8 个 rootless Docker target execution（七个 crate harness 加 `podman` fixture）。
- `images/mise`、`npins`、`rust` 的 Nix evaluation/build/load/runtime 检查仍全部执行；`podman` 的同一行为断言在两个引擎中执行。
- rootless lane 证明执行用户不是 UID 0、Podman 报告 rootless、GraphRoot 与 storage 可写；rootful lane 证明 Docker daemon 活跃且目标 fixture 获得声明的 root/capability/device。
- rootful cgroup/SSH/mount 必要路径不再静默跳过；CI 报告能区分 `passed`、`unsupported` 和 `skipped`，而 required assertion 遇到 `unsupported` 或 `skipped` 会失败。
- `container-init-reaper-vm.nix` 作为独立 `nixosTest` 在 CI 运行并通过；不在 VM guest 中启动第二层 QEMU。
- CI host launcher 不执行 `cargo`、`docker` 或 `podman` 测试命令，不连接 host Docker socket；每个 guest 失败返回非零并上传相关日志。
- `ci-success` 对 workspace、所有目标/runtime 组合和 NixOS VM test 进行汇总，任何一个失败都会使 workflow 失败。

实施结果与各测试命令的执行状态记录在变更交付说明中；本计划继续作为矩阵与验收要求的来源。
