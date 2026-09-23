# 开发与测试

`container-init` 是 Rust 2021 workspace，最低声明 Rust 版本为 1.74。当前 workspace 包含六个 crate：

```text
container-init/
├── Cargo.toml
└── crates/
    ├── bootstrap-model/
    ├── bootstrap-loader/
    ├── container-init-posix/
    ├── container-init-core/
    ├── container-init-backend/
    └── container-init-cli/
```

## 1. Crate 依赖方向

```text
bootstrap-model  ←  bootstrap-loader
       ↑                 ↑
container-init-posix ← container-init-core ← container-init-backend ← container-init-cli
```

更具体地说：

- `bootstrap-model` 只包含 serde 数据模型、验证、受限条件 AST、路径模板和静态 plan；不读取文件系统、不访问进程、不依赖 POSIX；
- `bootstrap-loader` 负责 TOML 和 profile graph，直到得到已验证的 `BootstrapConfig` 为止；不执行 action；
- `container-init-posix` 封装 passwd/group、UID/GID、文件 owner/mode 和 `flock` 等 POSIX 原语；
- `container-init-core` 执行 plan，处理 identity、condition、filesystem、SSH、资源锁、receipt 和 handoff；
- `container-init-backend` 持有单实例 snapshot，提供版本化 Unix socket RPC 和 PID 1 supervisor；
- `container-init-cli` 负责参数解析、配置路径发现和 backend 启动/客户端命令；二进制名称是 `container-init`。

保持这个方向很重要：bootstrap 不应反向依赖 dev-env provider、shellenv、mise、Devbox 或镜像特化逻辑。

## 2. 本地构建和测试

在 `nixos-dockers/container-init` 目录执行：

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked
```

常用的定向测试：

```bash
cargo test --locked -p bootstrap-model
cargo test --locked -p bootstrap-loader
cargo test --locked -p container-init-posix
cargo test --locked -p container-init-core
cargo test --locked -p container-init-backend
cargo test --locked -p container-init-cli
```

如果需要观察 CLI 集成输出：

```bash
cargo test --locked -p container-init-cli --test cli -- --nocapture
```

`Cargo.lock` 是 workspace 的锁定依赖清单；修改依赖时应保持 `--locked` 测试，避免本地隐式更新依赖版本。

## 3. Docker 集成测试

POSIX、core 和 CLI 各有一个 root Linux 容器 fixture：

```bash
./crates/container-init-posix/tests/docker.sh
./crates/container-init-core/tests/docker.sh
./crates/container-init-cli/tests/docker.sh
```

对应的 Dockerfile 会构建并运行：

- `container-init-posix`：真实容器中的 root account、权限、owner 和降权；
- `container-init-core`：真实 POSIX action、SSH fixture、资源锁和 privilege boundary；
- `container-init-backend`：真实 Unix socket ACL、singleton flock、RPC 和 supervisor；
- `container-init-cli`：profile 目录读取、plan/doctor/run、runtime handoff 和 SSH capability。

这几项测试要求 Docker 可用，且 fixture 默认以 root 运行。镜像名可以通过各脚本使用的环境变量覆盖：

```bash
CONTAINER_INIT_POSIX_TEST_IMAGE=my-posix-test \
  ./crates/container-init-posix/tests/docker.sh
```

## 4. 测试覆盖的行为

### 模型和加载器

- dotted action kind 被转换为内部 snake_case enum；
- profile id、extends、duplicate profile 和 inheritance cycle；
- scalar、input、action 的冲突和显式 override；
- provenance、trusted source、workspace action 限制；
- action id、字段、mode、owner、路径模板和条件校验；
- 缺失依赖、循环依赖、identity resolve 缺失和 phase violation；
- plan 的稳定拓扑顺序以及 effect 不泄露 content。

### Core 和 POSIX

- UID/GID、HOME 输入优先级和 workspace owner 映射；
- ensure_dir/file/symlink 的幂等行为；
- fixed content 不一致时拒绝覆盖；
- `chown`/`chmod` 的递归、wildcard、root 和 symlink 限制；
- passwd/group 原子映射和 login shell 更新；
- root 到 target 的降权；
- 条件跳过、失败策略和依赖跳过；
- 资源锁冲突、FIFO、公平批量获取与 handoff argv；
- receipt 原子写入和 content 隐藏；
- SSH host key 生成、重入、半成品拒绝、symlink 拒绝和 authorized keys 冲突。

### CLI

- `run --` 后 command 参数完整保留；
- plan/doctor 不产生 action 副作用；
- run 先执行 action 再 handoff；
- non-root handoff 暴露正确的 HOME、USER、LOGNAME；
- profile/default profile/path 环境变量发现；
- 未声明输入和 trust 错误的稳定退出码；
- SSH action 自动启用 capability。

## 5. 修改代码时的约束

- Rust 模块遵循 Rust 2018+ 目录结构，不使用 `mod.rs`；
- 不要把 `/workspace`、默认用户、AI 工具名、Devbox 或 SSH 目录写进通用 Rust handler；这些应来自 profile；
- 新增 bootstrap 能力先判断是否能表达为现有结构化 action；不要加入任意 shell 字符串逃逸；
- 新 action 必须同时更新 model enum、字段校验、plan effect、executor handler、trust/phase 规则和测试；
- 任何新路径操作都要考虑 NUL、`..`、已有 symlink、mount point、owner、mode、递归和中断恢复；
- 新的 profile 字段应在 loader raw model、merge/conflict、最终 model、CLI/执行器和文档中保持一致；
- 如果 capability 是可选的，应由 profile 中的 action 显式启用，而不是在启动时无条件探测或创建目录；
- 诊断应带 action id、来源和路径，但不要把 `content`、authorized keys 或 secret 写入 plan/receipt。

## 6. 文档变更自检

文档内容以源码行为为准，提交前可执行：

```bash
rg -n 'kind =|\[bootstrap|CONTAINER_INIT_|--(profile|workspace|input|backend-socket)' \
  README.md docs crates

git diff --check
```

修改配置或运行逻辑时，除了 `cargo test --workspace --locked`，还应运行受影响 crate 的 Docker fixture。仅修改文档时不需要 NixOS `evalConfig` 或 VM 测试；如果后续把 profile 接入 NixOS module、Docker image 或 service，则按仓库根目录测试指南补充对应的构建和 VM 验证。
