# container-init

`container-init` 是一个独立的 Rust 容器基础引导程序。它读取 profile 中的
`[bootstrap]` 命名空间，解析运行时身份，生成可审计的 action plan，执行有限的
POSIX 基础设施操作，然后把当前进程交给 profile 指定的 runtime。

它解决的是“容器启动前需要准备什么”，例如：

- 仅根据已证明的 workspace 挂载属主或显式输入选择目标 UID、GID、用户名和 HOME；
- 将声明为宿主 namespace 的 `HOST_UID`/`HOST_GID` 映射为当前容器 namespace 的 ID；
- 创建 HOME、缓存目录、配置目录以及声明的软链接；
- 必要时更新 passwd/group 和登录 shell；
- 按声明准备可选的 OpenSSH host key、authorized keys 和运行目录；
- 按声明自动化初始化 cgroup v2 层级、迁移隔离根进程并委托子树控制器，支持默认就地与只读挂载覆挂模式；
- 以结构化 argv 方式 handoff 到 `dev-env` 或其他 runtime。

它不执行 shell 脚本，不运行 `mise`、Devbox、sccache 或 provider，也不负责开发环境变量的物化。环境 DSL 与 Bootstrap DSL 可以存在于同一个 profile，但由不同程序分别读取。

Compose 注入的开发环境变量会随进程环境保留到 handoff runtime；例如
`CARGO_INCREMENTAL`、`SCCACHE_DIR` 和 `SCCACHE_DISABLE` 的解析属于 handoff
后的 `dev-env` environment DSL。`container-init` 不解析这些变量，也不会因为
`SCCACHE_DISABLE=1` 修改 `RUSTC_WRAPPER`。

> 本 README 以当前 `container-init` 源码为准。身份 namespace 映射、挂载证据和 root service handoff 的实现约定见仓库顶层设计文档及[实现状态与边界](#实现状态与边界)。

## 文档导航

- [DSL 完整参考](docs/dsl.md)：profile 结构、输入、action、插值和条件表达式。
- [配置与部署](docs/configuration.md)：profile 文件布局、继承、来源、覆盖和容器配置。
- [运行时与 CLI](docs/runtime.md)：`run`、`plan`、`doctor`、环境变量、锁和 receipt。
- [安全模型](docs/security.md)：信任边界、路径检查、权限阶段、SSH 和错误码。
- [开发与测试](docs/development.md)：crate 分层、构建、单元测试和 Docker 测试。

## 快速开始

### 1. 准备 profile

profile 文件名不必与 profile id 相同，加载器使用 TOML 中的 `id`。下面的例子创建一个 root 容器：启动时创建 `/workspace/.state` 和一个固定内容文件，然后以 `/bin/sh -c` 接收最终命令。

```toml
schema = 1
id = "example"

[bootstrap]
workspace_root = "/workspace"

[bootstrap.identity]
default_user = "root"
default_uid = 0
default_gid = 0
auto_mapping = false

[bootstrap.handoff]
runtime = "/bin/sh"
exec_prefix = ["-c"]
shell_prefix = ["-c", "true"]

[[bootstrap.actions]]
id = "resolve"
kind = "identity.resolve"
run_as = "root"

[[bootstrap.actions]]
id = "state-dir"
kind = "filesystem.ensure_dir"
path = "${bootstrap.workspace_root}/.state"
mode = "0750"
owner = "root"
run_as = "root"
depends_on = ["resolve"]

[[bootstrap.actions]]
id = "marker"
kind = "filesystem.ensure_file"
path = "${bootstrap.workspace_root}/.state/initialized"
content = "created by container-init\n"
mode = "0640"
owner = "root"
run_as = "root"
depends_on = ["state-dir"]
```

把文件放到例如 `/etc/dev-env/profiles.d/example.toml`，并让默认 profile 文件包含单独一行的 `example`：

```text
example
```

默认路径和覆盖方式见[配置与部署](docs/configuration.md)。

### 2. 先检查，再执行

```bash
# 只构建并打印静态计划，不修改文件、账户或锁
container-init --profile example plan

# 输出机器可读 JSON；action 的 content 不会出现在计划中
container-init --profile example plan --json

# 检查 workspace、身份、runtime 和 SSH capability，不执行 action
container-init --profile example doctor

# 启动 backend、执行引导，然后监督初始 runtime
container-init --profile example run -- 'printf "ready\n"'
```

推荐在镜像中使用：

```dockerfile
ENTRYPOINT ["/usr/bin/container-init", "run"]
```

如果需要把 `run` 的命令参数传给 handoff runtime，使用 `--` 结束 `container-init` 自身的选项。没有显式命令时使用 `shell_prefix`；有显式命令时使用 `exec_prefix`。程序始终以 argv 调用，不把参数拼成 shell 字符串。

### `docker exec` 与 Bash shim

Docker daemon 不会为已运行容器重新执行 Entrypoint，因此它不会自动应用
`container-init` 的身份解析和降权。镜像中的 `/bin/bash` 与 `/usr/bin/bash` 是
`dev-env` 的兼容 shim；root 启动 shim 时，shim 会通过内部的
`DEVENV_CONTAINER_INIT`、`DEVENV_BOOTSTRAP_REAL_SHELL` 路径重新执行：

```text
dev-env Bash shim
  → container-init run -- /usr/local/libexec/dev-env/real/bash <原始 argv>
  → backend snapshot / startup reconcile
  → dev-env exec -- /usr/local/libexec/dev-env/real/bash <原始 argv>
```

这些变量只标记镜像提供的内部能力，shim 不会猜测 PATH 中的程序，也不会复制
UID/GID 解析逻辑。`HOST_UID`、`HOST_GID`、`CONTAINER_HOME` 和 `RUN_AS_ROOT` 会
随继承环境传给 Bootstrap。需要保留 root 时显式使用：

```bash
docker exec -e RUN_AS_ROOT=1 -it <container> bash
```

`/usr/bin/dev-env-login-shell` 是 root SSH 的稳定 login shell，不会因为 `/bin/bash`
shim 而把 root 登录映射到开发用户；`/bin/sh` 和 real Bash 是低层诊断/显式逃生
入口，不会自动运行身份 Bootstrap。`docker exec` 的自动身份行为来自 shim 委托，
不是 Docker daemon 修改了容器默认用户。

### 3. 用运行时输入映射宿主身份

profile 先声明输入，CLI 或环境变量才可以设置它：

```toml
[bootstrap.identity]
default_user = "dev"
default_uid = 1000
default_gid = 1000
auto_mapping = true
uid_input = "HOST_UID"
gid_input = "HOST_GID"
home_input = "CONTAINER_HOME"

[bootstrap.inputs.HOST_UID]
target = "identity.uid"
type = "uid_pair"
namespace = "host"
aliases = ["UID_GID"]
runtime = true
format = "uid[:gid]"

[bootstrap.inputs.HOST_GID]
target = "identity.gid"
type = "gid"
namespace = "host"
runtime = true

[bootstrap.inputs.CONTAINER_HOME]
target = "identity.home"
type = "path"
runtime = true
allow_outside_workspace = false
```

运行时可以使用环境变量，也可以使用 CLI 覆盖：

```bash
HOST_UID=1001:1001 HOST_GID=1001 \
  container-init --profile example run

container-init --profile example \
  --input HOST_UID=1001:1001 --input HOST_GID=1001 \
  run
```

Compose 或其他编排默认不应把 `HOST_UID` 写成 `${HOST_UID:-1000:1000}`。这里的值
声明为宿主 namespace 后，UID 和 GID 都必须存在于当前进程的 namespace map 中；在
rootless 容器中宿主 UID 1000 可能已映射，但宿主 GID 1000 未必映射。未设置
`HOST_UID` 时会使用已确认的 workspace 挂载属主或 profile 默认值；需要显式覆盖时，
请传入真实的宿主 UID/GID，例如 `HOST_UID=$(id -u):$(id -g)`。

对 `runtime = true` 的输入，优先级是 CLI `--input`/`--set`，其次是环境变量，最后是 profile 的 `default`。CLI 同名输入优先于环境变量；输入必须在 profile 中声明，未声明的 `--input` 直接以配置错误退出。

## 工作方式

一次 `run` 的逻辑可以概括为：

```text
读取 profile 目录
    ↓
选择 profile，递归加载 extends
    ↓
仅投影并合并 bootstrap 命名空间
    ↓
校验 schema、来源、信任、字段和 action 依赖
    ↓
解析输入和目标身份
    ↓
获取 backend 实例 flock
    ↓
按静态 plan 执行启动 action
    ↓
发布 Unix socket 并监督 handoff child
```

计划由 `bootstrap-model` 生成。它会为显式 `depends_on` 加上必要的身份依赖，检查缺失依赖、循环和阶段倒置，并以稳定的拓扑顺序输出。`plan` 只生成这个静态计划，不探测运行时输入，也不访问宿主文件系统。

执行时，条件会针对实际的 workspace、输入、环境和目标身份求值。条件为假时 action 被跳过；依赖未成功完成时，依赖它的 action 也会跳过。`failure = "warn"` 或 `"ignore"` 允许当前 action 记录失败并继续处理无关 action，但不会让依赖该 action 的后续 action 执行。

## 实现状态与边界

当前已经实现：

- TOML profile 读取、`extends` 继承、父子冲突诊断和显式 override；
- `identity.resolve`、`identity.map_user`、`identity.ensure_home`；UID 0 始终规范化为 `root`/`/root`，不会重写为开发用户；
- `filesystem.ensure_dir`、`ensure_file`、`ensure_symlink`、`chown`、`chmod`；
- `process.set_user_shell`、`process.drop_privileges`、`handoff.exec`；
- 可选 `service.ssh.prepare`，仅通过受信任的 `ssh-keygen` capability 生成 host key；
- 可选 `cgroup.v2_init`，自动化 cgroup v2 根进程子组迁移与控制器（`cpu`、`io`、`memory`、`pids`）委托，支持默认就地模式与只读环境下的挂载覆挂重定向（bind-mount shadowing），供嵌套容器引擎使用；
- `plan --json`、`doctor --json`、结构化错误、非阻塞锁和原子 receipt；
- Linux mountinfo workspace 挂载证据、UID/GID namespace 映射和 group 成员 reconcile；
- 独立的 `bootstrap-model`、`bootstrap-loader`、`container-init-core`、`container-init-posix`、`container-init-backend` 和 `container-init-cli` crate。

当前 CLI/源码没有实现或不负责：

- `bootstrap.extensions` 和 `container-init-action-*` 外部插件协议；
- 读取 environment DSL、执行 provider、生成 `MaterializedEnv` 或运行 shell hook；
- 自动启动 `sshd`；SSH action 只准备目录和 key，daemon 仍由 handoff/服务编排负责；
- CLI 对 `feature:...` 条件的 feature 注入；当前 CLI 的运行时 feature 集合为空；
- `non_interactive` 的交互策略执行；字段会被解析和继承，但当前执行器不进行交互；
- 通过 CLI 加载 workspace overlay。库层支持 `SourceKind::WorkspaceOverlay`，但只能在 profile 明确允许且 action 属于安全集合时使用。

## 源码地图

| 层 | 目录 | 责任 |
| --- | --- | --- |
| 模型与计划 | [`crates/bootstrap-model`](crates/bootstrap-model) | DSL 类型、字段校验、条件 AST、路径模板、依赖图和静态计划 |
| 加载与合并 | [`crates/bootstrap-loader`](crates/bootstrap-loader) | TOML 解析、action kind 归一化、继承、来源和信任、冲突处理 |
| POSIX 边界 | [`crates/container-init-posix`](crates/container-init-posix) | passwd/group、UID/GID、`chown`、权限、`flock` 等系统原语 |
| 执行核心 | [`crates/container-init-core`](crates/container-init-core) | 身份解析、条件求值、文件 action、SSH capability、资源锁、receipt 和 handoff |
| Backend | [`crates/container-init-backend`](crates/container-init-backend) | 单实例 snapshot、Unix socket RPC、实例 flock 和 PID 1 supervisor |
| CLI | [`crates/container-init-cli`](crates/container-init-cli) | 参数解析、profile 路径发现、backend `run/exec/plan/doctor/status/version` |

几个关键入口：

- [`bootstrap-model/src/action.rs`](crates/bootstrap-model/src/action.rs)：action 字段和内置 kind；
- [`bootstrap-model/src/plan.rs`](crates/bootstrap-model/src/plan.rs)：阶段和稳定拓扑排序；
- [`bootstrap-loader/src/merge.rs`](crates/bootstrap-loader/src/merge.rs)：继承合并与冲突规则；
- [`container-init-core/src/executor.rs`](crates/container-init-core/src/executor.rs)：执行和 handoff；
- [`container-init-core/src/filesystem.rs`](crates/container-init-core/src/filesystem.rs)：安全路径及文件操作；
- [`container-init-core/src/identity.rs`](crates/container-init-core/src/identity.rs)：输入和身份解析。
