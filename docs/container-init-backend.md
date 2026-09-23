# container-init 单一后端重构方案

状态：已实施。本文记录目标行为、架构、锁模型、迁移和验证步骤；实现已按阶段提交并通过 Rust 与 CLI Docker fixture 验证。

## 1. 目标与决策

将每个容器中的多个 `container-init` 运行实例改为一个长期存活的后端实例。后端在容器启动时加载一次 image/admin profile，生成并持有不可变配置快照和静态 plan；后续 `exec`、运行时身份协调和状态查询通过本地 Unix socket 使用该快照。

目标：

- 同一个容器内只允许一个后端持有配置和管理状态；
- 运行中的 `exec` 不重新读取 profile 文件、不重新解析 TOML/继承图；
- 后端按实际共享资源协调 action，并允许互不冲突的 action 并行；
- 锁不覆盖 handoff 命令的生命周期；
- 保留直接 argv handoff、终端、标准输入输出和调用方环境；
- 配置、CLI 和生命周期允许破坏性调整，不维护旧文件锁语义兼容层。

非目标：

- 多个 profile/backend 在同一个容器内共存；
- 监听网络端口或跨容器共享后端；
- 通过后端 socket 转发终端流；
- 将任意 workspace 内容变成可信 bootstrap 配置。

## 2. 历史实现和问题位置

本节保留重构前的实现作为迁移背景，不描述当前代码路径。当前实现已经由上面的长期 backend 进程模型替代；其中 `--lock-path`、`is_reconciled` fallback 和每次 CLI 重新加载 profile 都是历史问题。

当前职责分散在每个 CLI 进程中：

- `container-init-cli/src/application.rs` 的 `run` 和 `exec` 都调用 `config::load`；
- `container-init-cli/src/config.rs` 每次扫描 profile 目录、读取 TOML、合并继承并构建 plan；
- `run` 由 `container-init-cli/src/lock.rs` 按 profile id 和 workspace 算出 lock key，再在整个 `PlanExecutor::execute` 期间获取一个 `flock`；
- `exec` 虽然正常路径不获取锁，但仍单独加载配置和解析身份；若 `identity.ensure_home` 检查失败，则退回完整 plan 执行和全局 lock；
- `PlanExecutor` 在执行器内部持有 lock 到所有 action 完成，而 lock key 的粒度是整个 profile/workspace；独立文件、账户数据库、SSH key 和 cgroup 操作不能分别调度；
- `run` 最终用 `exec` 替换自身，因此没有常驻进程保存配置或协调后续 `docker exec`；
- Nix Docker entrypoint 是 `container-init run --`。root Bash shim 则会在每次调用时启动 `container-init exec`。

因此，同一配置会随每个 CLI 进程重复加载；全局 lock 又把不同资源的操作混为一个互斥域。仅把 `flock` 换成进程内 mutex 不能解决问题，因为当前生产路径在 handoff 后已不存在可共享的进程。

## 3. 目标进程模型

新增 `container-init-backend` crate，负责单实例生命周期、请求协议、配置快照、资源锁管理和 PID 1 监督。依赖方向保持为：

```text
bootstrap-model <- bootstrap-loader
       ^                 ^
       |                 |
container-init-posix <- container-init-core <- container-init-backend <- container-init-cli
```

容器生命周期：

```text
Docker ENTRYPOINT: container-init run -- [command...]
        |
        +-- 获取 backend 单实例 flock
        +-- 读取 profile、构建 LoadedConfig 和 Plan（整个生命周期一次）
        +-- 执行启动阶段 action，写启动 receipt（如果配置）
        +-- 启动 Unix socket listener 和初始 handoff 子进程
        +-- 留在 PID 1：接收信号、回收子进程、服务控制请求

docker exec / root Bash shim
        |
        +-- container-init exec -- [command...]
        +-- 连接 backend，提交 argv、cwd 和允许的 runtime input
        +-- backend 解析身份、协调必要的 identity action、生成 handoff
        +-- client 直接降权并 exec handoff runtime，保留 TTY 和 stdio
```

`run` 保留为镜像启动入口，但从“执行后 exec 替换进程”改成“启动并监督后端”。后端是容器中的 PID 1，初始 handoff runtime 是它的子进程。`exec` 变为纯 RPC client；不再自行加载配置，也不能选择另一个 profile。

客户端只让 socket 承载控制面数据，不由后端代理 stdin/stdout/stderr/PTY。后端返回结构化 `PreparedHandoff`（runtime argv、工作目录、目标 UID/GID、补充组、`HOME`/`USER`/`LOGNAME`），客户端在当前进程执行该 handoff。这样 `docker exec -it` 的终端和退出状态仍由 Docker 直接关联到实际命令。

## 4. 后端启动和状态

协议状态枚举保留 `STARTING`、`READY`、`FAILED`、`STOPPING`。当前实现只在 socket 发布后暴露 `READY` 和 `STOPPING`：启动 action、handoff 构造或 socket 初始化失败会在发布服务前返回错误并退出，因而不会留下一个可查询但不可执行的 `FAILED` 实例。`exec` 客户端在 socket 尚未出现或 backend 正在退出时按有限退避重试，超时后返回明确的可重试错误，不退回本地配置加载或本地执行。

启动顺序：

1. 解析仅用于 backend 启动的 CLI 参数和环境变量，确定 profile 来源、profile id、启动 workspace、启动输入、socket 路径和 singleton lock 路径。
2. 获取 `/run/container-init/backend.lock`（root）或选定 runtime directory 中的固定实例锁。必须在加载 profile 之前取得，避免第二个 `run` 进程为了判定实例状态又解析一遍配置。
3. 若实例锁已被占用，连接已有 socket 并返回 `already running`/状态；不得启动第二份执行器。若 socket 暂不可用，有限等待后报告现有实例启动失败。
4. 持有实例 lock 文件描述符直到后端退出。它只用于防止重复 backend，不用于串行化 bootstrap action。
5. 一次性读取 image/admin profile，执行 loader、trust/model 校验并构建 `Arc` 配置快照和静态 plan。backend 生命周期内不自动热加载；修改 profile 后重启容器生效。
6. 在打开请求处理线程前完成会迁移进程 cgroup、mount namespace 或其他进程级状态的启动 action。启动 action 失败时直接返回带 action/path 摘要的错误并退出，不发布 socket 或可执行 handoff 的 `READY` 服务。
7. 安全创建 socket，启动初始 handoff 子进程，再发布 `READY` 并进入 supervisor loop。客户端在 socket 创建前连接失败时只做短暂重试。
8. 收到 SIGTERM/SIGINT/SIGHUP 等信号时转发给初始子进程/其进程组，停止接受新请求，等待或按超时策略终止子进程，回收所有子进程，清理本实例 socket 并释放 flock。

实例 lock 文件可在进程退出后保留；`flock` 释放才代表实例已停止。下次启动只有在成功取得 flock 后才能检查并清理残留 socket。清理时必须 `lstat` 验证它是预期位置的 Unix socket，不能跟随 symlink 或删除普通文件。

## 5. 配置和请求语义

### 5.1 单一配置快照

profile、profile chain、信任来源、policy、handoff runtime 和静态 plan 由 `run` 启动参数确定，配置对象在后端中只读共享。`exec` 不接收 `--profile`、`--profiles-dir`、`--admin-profiles-dir`、`--default-profile` 等配置发现参数。改选 profile 需要重启容器。

`plan`、`doctor` 和新增的 `status` 应优先向运行中的 backend 查询快照，避免活容器内诊断命令再读出与服务实际配置不同的文件版本。没有 backend 时，`plan`/`doctor` 可保留离线模式，用本地 profile 独立加载；离线结果要标记 `offline`，不可被误认为运行中快照。

### 5.2 Runtime input 和 workspace

请求可包含 `argv`、调用方 cwd、显式 `--input NAME=VALUE` 和 profile 声明为 `runtime = true` 的环境输入。server 使用已加载 schema 验证名字、alias、类型、namespace 和输入目标；未知值、修改 policy 的字段和 profile 来源一律拒绝。server 不接受客户端提交的 profile 文本、action 描述或可执行文件路径。

协议握手返回允许读取的 runtime input 名及 request identity reconciliation 条件中显式引用的环境名。CLI 只把这些名字的环境值发给 backend；完整环境不发送给 root 后端。实际 handoff 子进程仍由调用方进程执行，因此它继续继承调用方的完整环境。startup `run` 的输入只影响启动 plan；后续请求输入只影响 request identity 和允许的 identity reconciliation。

每个请求允许重新解析 identity 输入，以保留 `HOST_UID`、`HOST_GID`、`CONTAINER_HOME` 和显式 root 模式等容器调用行为。`exec` 不重跑任意 startup action；后端执行 profile 中 identity 相关 action 的受限集合（`identity.resolve`、`identity.map_user`、`identity.ensure_home`、`process.set_user_shell`），并按请求计划重新执行明确标记为 workspace overlay 的安全 target action。其他 filesystem、SSH、cgroup 和 service action 只在启动 plan 执行。模型加载阶段验证：可变 runtime input/environment 不得影响 request 阶段不会重跑的 action 的条件、路径或内容；违反时配置失败并给出 action/input 定位。workspace overlay action 属于 request 计划，因此可以引用 request identity，但不会进入启动计划。

workspace/cwd 是每请求上下文，不改变 backend 当前工作目录。相对 cwd 先由 client 转为绝对路径；server 检查其为可访问目录，并沿用现有 workspace/mountinfo 身份规则。bootstrap 文件 action 的路径仍来自 profile 和受限 typed input，而不是客户端传来的任意路径。

### 5.3 请求与响应

协议使用带版本号和最大帧长的长度前缀结构化消息（第一版可用 JSON/Serde；限制单帧大小，校验 UTF-8、argv NUL、字段数和总环境输入大小）。至少需要：

- `Hello`：协议版本、backend 状态、配置快照 id、允许的 runtime input/env 名；
- `ExecRequest`：request id、argv、cwd、声明输入值；
- `PreparedHandoff`：runtime argv、cwd、目标凭据、登录环境变量和 receipt/error 摘要；
- `StatusRequest`/`StatusResponse`；
- `PlanRequest`/`DoctorRequest` 及其响应；
- 结构化错误：稳定错误 class、retryable 标记、request id、action id/path（不含 content/secret）。

`PreparedHandoff` 中 runtime 只能来自已验证 profile 的绝对 handoff runtime；argv 是结构化数组，不拼 shell 字符串。响应还携带 server canonicalized 的 cwd、严格限定为 `HOME`/`USER`/`LOGNAME` 的登录环境和 request receipt 摘要；client 校验这些字段、身份字段和运行时绝对路径后再调用 exec。控制 socket 不接收 arbitrary spawn 命令。

## 6. IPC 和权限边界

- socket 默认位于 `/run/container-init/backend.sock`，rootless/不可写时使用现有 runtime-directory/workspace 回退规则。路径配置只能在 `run` 时设置，client 必须连接同一 socket。
- socket 父目录由 root 或 backend 用户拥有，建议 `0710`；socket 建议 `0660` 且 group 仅允许配置的 target group。具体 owner/group 按 rootless 模式验证。
- Linux 使用 `SO_PEERCRED` 校验 peer 的 PID/UID/GID。root backend 只接受 root peer 或当前 profile 允许的目标 UID peer；再校验请求中的身份不能令非 root peer 获得更高凭据。`RUN_AS_ROOT`/类似输入只有 peer euid 为 0 且可信配置声明该 input 时才能生效。
- 所有 profile/action 仍按现有 image/admin 信任策略加载。RPC 不增加新的信任来源，不允许 workspace overlay 通过请求注入。
- 对 socket 目录和 socket inode 执行 owner/mode/type 检查；拒绝不匹配文件、symlink、截断帧、未知协议版本和重复 request id。
- 后端错误、status、receipt 和日志不打印固定 content、authorized keys 内容、secret input 原值或完整环境。
- 未认证/非法请求不能执行 bootstrap action；错误请求限速并设并发连接上限，避免本地 socket 被滥用造成线程耗尽。

后端返回的 UID/GID 不能被视为授权本身。客户端若以 root 连接，只能按 profile 和 request 的显式 root 规则执行；非 root client 只能以与其 peer UID 相同的身份启动。对运行 SSH daemon 的 root 例外仍只由配置的 `ssh_daemon` 精确匹配触发，且只作用于初始 handoff。

## 7. 细粒度资源锁

使用 backend 内的 `ResourceLockManager` 取代“每个 CLI 进程按 profile/workspace 获取一个文件锁”的 action 锁。锁只保护一次资源解析/检查/修改，不保护 RPC socket、整个请求或 handoff 子进程。`plan` 仍按静态依赖顺序执行；不同 request 的 action 可并发，只要它们的资源锁不冲突。

第一版锁模式全部为 exclusive，先实现正确的冲突判定；未来只对确定只读且无 TOCTOU 风险的资源增加 shared lock。每个 action 执行前先解析它完整的锁集合，由 manager 一次性授予整组锁，避免逐把持锁导致死锁。多资源键按稳定排序，等待采用公平队列；request 取消/断连后由 RAII guard 释放锁。锁表只保留活动及排队键，空闲键及时回收。

| 资源键 | 冲突规则 | 使用场景 |
| --- | --- | --- |
| `Accounts(passwd, group, shadow?)` | 相同规范化 account database 全互斥；必要时 resolver 读也进入同一域 | `identity.map_user`、`process.set_user_shell`、读取账户映射 |
| `Path(path, exact)` | 相同规范化路径冲突 | 固定文件、receipt 目标、authorized_keys 文件 |
| `Path(path, subtree)` | 与路径相同、祖先/后代的 exact/subtree 锁冲突；路径比较按组件边界，不按字符串前缀 | mkdir、递归 chown/chmod、SSH key directory、父目录创建 |
| `Cgroup(hierarchy/controller-set)` | 同一 cgroup hierarchy 的结构/controller 变更冲突；不同 hierarchy 可并行 | `cgroup.v2_init`、controller delegation、shadow mount |
| `ProcessNamespace(kind)` | 同一 namespace 的不可并行转换冲突 | user/mount namespace 或影响整个进程树的初始化操作 |

lock key 必须从 action 的最终、经过校验的目标推导，不使用 profile/workspace 全局 key。对不存在的 path，规范化为“最近现存父目录的 canonical path + 剩余组件”；路径 action 仍需使用现有 symlink、mount point、`..` 和递归边界检查。锁管理器只能消除 backend request 之间的竞争，不能替代 dirfd/`openat2` 防护，也不能约束容器外或绕过 backend 的进程修改同一文件。

获取规则：

1. action handler 在任何 side effect 前生成锁集合；解析不出安全且稳定的资源 key 时 fail closed。
2. manager 在一把短期内部 mutex 下比较 active set 并原子授予完整集合；mutex 不覆盖文件 IO/action 执行。
3. action 在 guard 持有期间完成路径检查、读改写和必要的 fsync；无论成功、失败或 panic unwind 都释放 guard。
4. action 完成后立即释放锁，再执行下一个 plan action。不得把锁持有到 receipt 输出后的任意命令运行阶段。
5. cgroup/process-namespace 类 action 在 backend 初始化线程启动前完成；backend execution options 会保留 supervisor 当前 PID，不把 backend worker 本身移入会破坏 listener/监督的 cgroup。需要 mount namespace fallback 时，backend 拒绝在自身 namespace 中 unshare；普通 standalone executor 仍可使用原有 fallback。

当前 `process.drop_privileges` 会对执行它的进程调用 `setgroups/setgid/setuid`，不能在长期 root backend 中照旧运行。重构时它应变为 handoff credential intent；由 client 直接 exec handoff 前一次性应用目标凭据，或由初始 handoff 的子进程启动钩子应用，绝不能修改 backend PID 1 本身。

### action 到 lock-set 的映射

- `identity.map_user`：该 action 实际修改的 passwd/group 文件使用一个有序 account lock set；使用同一账户数据库的身份读取与写入协调。
- `process.set_user_shell`：账户锁；不得和 `map_user` 同时部分持有不同顺序的锁。
- `identity.ensure_home`、`filesystem.ensure_dir/file/symlink`：目标路径 subtree/exact 锁。create/rename 还需覆盖会被修改的父目录资源。
- `filesystem.chown/chmod`：非递归目标 exact，递归目标 subtree；递归不得跨越现有安全检查禁止的 `/`、symlink 或挂载边界。
- `service.ssh.prepare`：host key/authorized key/runtime directory 的路径锁集合，覆盖 keygen 和 non-overwriting 安装整个临界区。
- `cgroup.v2_init`：cgroup hierarchy/controller lock；如果影响 PID 1、mount namespace 或当前进程组，作为启动专属 barrier。
- backend 保留 supervisor 在 bind-mount hierarchy 外部；若 bind-mount 模式无法在不移动 supervisor 的情况下启用 controller，启动 action 失败并退出，不能把未启用 controller 报告为成功。
- receipt：目标文件 exact lock，仍以临时文件、fsync、rename 原子写入。
- `process.drop_privileges` 和 handoff：不获取共享资源锁；前者只准备凭据，后者在客户端进程中执行。

不要用“所有 `/workspace` 文件共用 workspace 锁”替代路径锁；它会让不同项目/不同 action 重新串行。也不要只锁叶子字符串而忽略递归树和父目录创建的冲突。

## 8. 进程监督和失败恢复

PID 1 supervisor 必须满足：

- 初始 handoff runtime 作为子进程运行；初始 child 的退出码成为容器退出码；
- singleton lock fd、listener fd 和 worker fd 均设置 close-on-exec；handoff child 不得继承这些描述符，避免 child 意外延长实例锁或接管 socket；
- 正确转发终止信号，回收初始进程及其他可回收子进程；必要时使用独立进程组传播终止信号；
- 请求 worker 不修改 backend 的 cwd、environment 或 credentials；每个 request 使用独立 `RuntimeContext`；
- worker 数量/排队请求有上限；客户端断开不会留下永久锁或 worker；
- backend 崩溃或容器停止时由内核释放 singleton flock，client 收到连接关闭/不可用错误；重启时新进程重新加载一次配置、重新执行启动 reconcile；
- profile/action 启动失败时不启动主 handoff，且不接受可以绕过失败状态的 `exec`。

启动 actions 的文件操作必须保持当前幂等、原子安装和中断后重试行为。receipt 应区分 `startup` 和每次 `exec` 的 request id；建议启动 receipt 固定路径、request receipt 按 request id 或配置路径规则写入，避免并发请求互相覆盖。receipt 不增加 action content 或 secret。

## 9. 破坏性 CLI 和文档迁移

在实现合并点一次切换到新语义，不保留旧 `exec` 的“加载本地 profile”回退：

- `run`：唯一 backend 启动命令；原有启动参数（profile/config/workspace/input/receipt）只在此处有效；同容器重复 `run` 连接已有 backend 并返回 already-running/状态，不再次执行 startup plan。
- `exec`：仅接受 socket 覆盖、请求超时、可选 identity input 和 handoff argv；profile/profile-dir/admin-profile-dir/default-profile/lock-path/lock-timeout 等选项删除并报参数错误。
- `--lock-path`、`--lock-timeout`、相关 `CONTAINER_INIT_LOCK_*` 环境变量删除；增加 `--backend-socket`、`CONTAINER_INIT_BACKEND_SOCKET` 和请求连接超时。singleton lock 路径只属于 server 生命周期配置。
- 移除按 profile/workspace 计算 bootstrap lock 文件名的逻辑。固定实例 flock 保留为防止重复 server 的机制，不参与 action 调度。
- `plan`/`doctor` 在线时读 backend snapshot；离线时显式显示本地加载行为。增加 `status` 展示 profile id、snapshot id、状态、backend pid、启动时间和正在处理请求数，不展示 input value。
- Docker entrypoint 仍可以是 `["/usr/bin/container-init", "run", "--"]`，但测试、README 和退出语义改为 supervisor/child 模式。
- `dev-env` root Bash shim 继续调用 `container-init exec -- real-bash ...`，调用形式保留但从本地执行改为 RPC；更新其 runtime 文档和集成测试以覆盖后端未就绪、backend 中断、非 root caller 和 `RUN_AS_ROOT` 授权。
- 更新 `container-init/README.md`、`docs/runtime.md`、`docs/security.md`、`docs/development.md` 和 `dev-env/docs/runtime.md`，明确 config snapshot、socket ACL、权限变化、锁范围、offline diagnostics 和不再支持的选项。

## 10. 实施阶段

每一阶段完成后运行其定向测试并提交，commit message 描述本阶段的可验证行为：

1. **Git 基线**：实现本方案之前，在 `/workspace/nixos-dockers` 初始化 Git，确认仓库根目录、用户身份和默认分支；先提交当前工作树基线，再开始代码修改。当前 `.git` 目录为空，不得假设现有 refs/index/remote 可用。后续每个阶段提交代码与其测试，保持可回溯；不要为了初始化覆盖用户文件。
2. **锁契约**：在 `container-init-core`/新 backend crate 定义资源键、路径 scope、批量锁获取、fairness、取消释放和锁冲突测试；先让现有执行器用 action resource lock，而不是 profile/workspace 总锁。
3. **后端协议**：新增 backend crate、socket 生命周期、singleton flock、协议版本/帧限制、peer credential 检查、状态查询和拒绝非法请求测试。
4. **执行器拆分**：拆出启动 plan、request identity reconciliation 和 handoff credential preparation；确保长期 backend 不执行 `setuid`，启动 action 只执行一次，request action 只执行 identity action 集合。
5. **supervisor**：实现 PID 1 子进程启动、信号转发、子进程回收、READY/STOPPING 状态和 backend 重启/异常退出用例；启动失败必须在 socket 发布前退出。
6. **CLI 切换**：`run` 启动 server，`exec` 成为 RPC client，删除旧的 profile/workspace 文件锁参数；迁移 `plan`、`doctor` 和 `status`。
7. **镜像和调用方迁移**：更新 Nix image、Docker fixture、dev-env Bash shim 集成路径和所有上述文档。可保持 Entrypoint argv 不变，但必须验证 PID 1/退出行为变化。
8. **删除旧实现**：移除 `is_reconciled` 全 plan lock fallback、基于 profile/workspace 的 lock path/key 和旧锁错误文案；清理过时测试和兼容说明。

实现过程中每个代码/测试阶段至少一个独立 commit；测试修复作为后续 commit，不 amend/重写已验证阶段历史，除非用户另行要求。由于本轮 `.git` 为空，不能在本方案撰写阶段提交；初始化和基线提交是上述实施阶段的第一个动作。

## 11. 验证矩阵与完成条件

### Rust 测试

- 同容器并发 `run` 只能选出一个 server，第二个不会加载配置或重复执行 startup action；崩溃后 flock 自动释放，重启能安全清理陈旧 socket。
- 配置 loader 调用计数证明一份 backend 生命周期只加载一次；并发 `exec` 不访问 profile 文件；在线 plan/doctor 与服务持有相同 snapshot id。
- 协议版本、超帧、截断 JSON、未知字段/输入、NUL argv、过大环境输入、socket symlink/owner/mode 和 peer UID/GID 验证失败关闭。
- root、目标用户、其他 UID 的连接权限；非 root 不能借请求输入选择 root 或其他 UID；显式 root mode 仅允许 root peer 与受信任 schema。
- 锁冲突矩阵覆盖同一路径、父子路径、路径组件前缀相似但非祖先、递归 subtree、共享 passwd/group、相同 SSH key、相同 cgroup hierarchy、独立路径并发。
- barrier 测试证明同资源操作互斥、独立路径重叠运行、一个请求失败/断连不会泄漏 lock、批量锁不会死锁、FIFO 队列无持续饥饿。
- startup action 只执行一次；并发 `exec` 对相同 identity 的 home/account reconcile 幂等；different identity 的受支持操作按资源锁隔离；handoff lock 在命令启动前释放。
- backend parent credentials 始终不变；初始普通 handoff、SSH root handoff、root/target exec 的 uid/gid/groups/HOME/USER/LOGNAME 正确。
- supervisor 信号转发、主 child 退出码、子进程回收、client retry/timeout、backend 停止期间连接失败有覆盖。
- 启动 action 失败不会启动 handoff、不会发布 socket，客户端只能看到连接失败或超时；失败原因由 `run` 的结构化错误输出承担。
- 更新 `container-init-cli` 和 `dev-env` 单测/集成测试，删除旧的 run 并行争抢一个全局 lock 的预期。

### Docker/Nix 验证

- 运行 `container-init-posix`、`container-init-core`、`container-init-cli` Docker fixture，以及受影响的 `dev-env` Docker fixture；测试真实 socket ACL、SO_PEERCRED、PID 1、终端和 signal 行为。
- 执行 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --locked`，以及相关 crate 的 Docker fixture。
- 构建容器 image，确认 Entrypoint/Cmd/profile 路径正确，`container-init run` 作为 PID 1 能启动且 Docker stop 正常收敛。
- 按根仓库指南运行受影响的 NixOS `evalConfig` 和构建验证；只有改动 NixOS runtime/service 集成时增加对应 `nixosTest`。
- 执行 `git diff --check`，审阅 `git log` 阶段提交和工作树状态。

### 验收标准

1. 活容器中重复并发 `container-init exec` 不读取 profile 文件，也不争抢 bootstrap 全局 `.lock`。
2. 同资源 action 被序列化，不同文件树/account database/cgroup 域在锁契约允许时并行。
3. socket 只对授权 peer 可用；profile/action 不会经 RPC 被调用方替换；backend 始终保有 supervisor 所需的 root 凭据。
4. `docker exec` 的 argv、环境、cwd、TTY、signal 和退出码行为有测试证明。
5. 所有变更分阶段提交，Git 基线和实现提交均可在当前仓库历史中追踪。
