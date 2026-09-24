# 运行时与 CLI

二进制名称是 `container-init`，命令行入口由 `container-init-cli` 提供：

```text
container-init [OPTIONS] run [--] [COMMAND...]
container-init [OPTIONS] exec [--] [COMMAND...]
container-init [OPTIONS] plan [--json]
container-init [OPTIONS] doctor [--json]
container-init [OPTIONS] status [--json]
container-init [OPTIONS] version
```

`-h`/`--help` 打印帮助；`--version` 等价于 `version`。

## 1. 子命令

### `plan`

```bash
container-init --profile coding-images plan
container-init --profile coding-images plan --json
```

`plan` 优先读取运行中 backend 持有的不可变 snapshot；没有 backend 时才离线读取 profile、解析继承图、完成 schema/trust/model 校验。离线结果会标记 `offline`，不会：

- 解析环境变量中的 typed runtime input；
- 读取或修改 passwd/group；
- 创建目录、文件或软链接；
- 启动 backend 或获取实例锁；
- 检查 runtime 是否真实可执行；
- 执行 SSH keygen 或 handoff。

文本输出包含 profile、profile chain，以及每个 action 的序号、id、kind、phase、run_as、idempotency、origin 和 effect。`--json` 输出对象的主要字段为：

```json
{
  "profile": "coding-images",
  "profile_chain": ["base", "coding-images"],
  "actions": [
    {
      "id": "resolve",
      "kind": "identity_resolve",
      "phase": "root",
      "run_as": "root",
      "depends_on": [],
      "origin": {
        "profile": "base",
        "source": "image_profile",
        "location": "/etc/dev-env/profiles.d/base.toml:bootstrap.actions[0]"
      },
      "idempotency": "idempotent",
      "effect": "identity_resolve"
    }
  ]
}
```

实际 JSON 的 `kind`、`phase`、`run_as` 和 `idempotency` 使用内部 snake_case 名称。文件内容不会进入 effect；这使得 `plan --json` 可用于审计而不直接泄露固定配置或 authorized keys 内容。

### `doctor`

```bash
container-init --profile coding-images doctor
container-init --profile coding-images doctor --json
```

`doctor` 在不执行 bootstrap action 的情况下检查：

- workspace 是否存在且为目录；不可写时是 warning；
- identity 是否可以解析；
- `bootstrap.handoff.runtime` 是否是可执行文件；
- `login_shell` 和 `ssh_daemon`（如果声明）是否可执行；
- 每个 SSH action 使用的 `ssh-keygen` capability 是否可执行；
- 当前有效 UID 是否满足 root action；
- `process.set_user_shell` 的 shell 文件是否可执行；
- profile 是否至少声明了一个 action。

文本输出会列出 `pass`、`warn` 或 `fail` 检查和最后的 `status`。JSON 输出包含 `profile`、`profile_chain`、`workspace`、可选 `identity`、`handoff`、`action_count`、`checks` 和 `ok`。warning 不会令 `ok` 变为 false，任一 fail 会令命令以对应错误码退出。

doctor 不会创建 workspace、HOME、SSH 目录、lock 或 receipt，也不会验证每个普通文件 action 的最终路径是否真的可写。

### `run`

```bash
# 无显式 command，runtime 使用 shell_prefix
container-init --profile coding-images run

# 有显式 command，runtime 使用 exec_prefix
container-init --profile coding-images run -- tool --flag 'value with spaces'
```

`run` 是每个容器唯一的 backend 启动入口。执行顺序是：

1. 获取固定 backend 实例 `flock`；已有实例只返回状态，不再加载 profile；
2. 加载 image/admin profile 目录并选择 profile，构建不可变 snapshot；
3. 完成启动 reconcile，创建 Unix socket 并启动初始 handoff 子进程；
4. backend 监督初始子进程，初始子进程退出码成为容器退出码；
5. 收到终止信号时转发给 handoff，回收子进程并清理 socket。

`run` 会先把当前工作目录切换到 runtime workspace。目标身份解析通过 Linux
`/proc/self/mountinfo` 确认 workspace 挂载事实后才读取其属主；镜像构建时预创建的
目录不会触发自动映射。无法证明挂载时会回退到 profile 默认身份并在 doctor/receipt
中记录 warning。

如果 profile 没有显式 `handoff.exec`，执行器仍会根据 `[bootstrap.handoff]` 生成 handoff。显式 action 主要用于让 handoff 出现在计划和 receipt 的 action 轨迹中。

### `exec`

```bash
# 无显式 command，runtime 使用 shell_prefix
container-init exec

# 有显式 command，runtime 使用 exec_prefix
container-init exec -- tool --flag 'value with spaces'
```

`exec` 是连接运行中 backend 的并发权限转交入口（供 `dev-env shim` 或并发 `docker exec` 使用）。它会：

1. 通过 Unix socket 向 backend 请求快照中的 handoff 和身份 reconciliation；
2. 发送绝对 cwd、显式 runtime input 和 schema 允许的环境值；
3. 在调用进程的 handoff 子进程中一次性应用 UID/GID/supplementary groups；
4. 将 `HOME`、`USER`、`LOGNAME` 设置为目标值并执行 handoff runtime。

`exec` 不接收 profile、profiles-dir、workspace 或旧 bootstrap lock 参数；它不会重新读取 profile。请求只会执行受限 identity action 集合，启动 filesystem、SSH、cgroup 和 service action 只在 backend 启动时执行。

### `status`

`status` 查询 backend 的状态、profile id、snapshot id、backend PID、初始子进程 PID 和活跃请求数，不显示 runtime input 值。

### `version`

```bash
container-init version
container-init --version
```

输出 `container-init <Cargo package version>`。

## 2. CLI 选项

| 选项 | 作用 |
| --- | --- |
| `--profile ID` | 选择 profile id |
| `--profiles-dir PATH` / `--profile-dir PATH` | image profile 目录或单个文件 |
| `--admin-profiles-dir PATH` | trusted admin profile 目录或单个文件 |
| `--default-profile PATH` / `--default-profile-file PATH` | 默认 profile id 文件 |
| `--workspace PATH` / `--cwd PATH` | runtime workspace |
| `--input NAME=VALUE` / `--set NAME=VALUE` | 设置一个已声明的 typed bootstrap 输入，可重复 |
| `--backend-socket PATH` | 覆盖 backend Unix socket 路径 |
| `--request-timeout-ms MS` | backend 请求/启动连接超时 |
| `--receipt-path PATH` | 写执行 receipt 的路径 |
| `--json` | `plan`/`doctor`/`status` 支持 |
| `-h` / `--help` | 打印帮助 |

通用选项可以位于 command 名之前，也可以位于 `run` 后、最终 command 之前。`run` 的最终 command 推荐放在 `--` 后面；否则第一个非选项参数会被视为 command 的开始，之后参数全部原样转发。

例如下面的 `--profile` 是最终 command 的参数，不会被 container-init 解析：

```bash
container-init --profile coding-images run -- tool --profile dev
```

`plan`/`doctor` 不能携带最终 command，`run` 不能使用 `--json`。

## 3. 环境变量

CLI 环境变量的发现顺序如下；同一类配置中，命令行选项优先于环境变量。

| 环境变量 | 作用 | 默认值 |
| --- | --- | --- |
| `CONTAINER_INIT_PROFILE_DIR` | image profile 目录/文件 | — |
| `CONTAINER_INIT_PROFILES_DIR` | image profile 目录/文件的兼容名称 | — |
| `CONTAINER_INIT_ADMIN_PROFILES_DIR` | admin profile 目录/文件 | — |
| `CONTAINER_INIT_PROFILE` | profile id | — |
| `CONTAINER_INIT_PROFILE_ID` | profile id 的兼容名称 | — |
| `CONTAINER_INIT_DEFAULT_PROFILE` | 默认 profile 文件 | — |
| `CONTAINER_INIT_DEFAULT_PROFILE_FILE` | 默认 profile 文件的兼容名称 | — |
| `CONTAINER_INIT_WORKSPACE` | workspace | — |
| `WORKSPACE` | workspace 的通用回退变量 | 当前目录 |
| `CONTAINER_INIT_BACKEND_SOCKET` | backend Unix socket | root: `/run/container-init/backend.sock` |
| `CONTAINER_INIT_BACKEND_TIMEOUT_MS` | backend 请求/启动连接超时 | 5000 |

profile 的 typed input 不是由 container-init 读取全部环境变量，而是仅读取 `bootstrap.inputs` 中声明的名字和 aliases。其他 ambient 环境值只会在条件或路径显式使用 `env.NAME` 时被引用。

## 4. 输入和身份在运行时的优先级

对每个 `runtime = true` 的输入：

```text
--input/--set canonical name
  > --input/--set alias
  > 环境中的 canonical name
  > 环境中的 alias
  > profile default
  > 未设置
```

`runtime = false` 的输入不读取 CLI 或环境，只使用 profile default；没有 default 就没有这个解析值。`uid_pair`/`gid` 输入还必须声明 `namespace = "host"` 或 `"container"`，两者不能混用；host ID 会按对应的 `/proc/self/uid_map` 或 `/proc/self/gid_map` 转换。

身份 resolver 随后按 Bootstrap DSL 中的规则使用这些 typed value。`RUN_AS_ROOT` 为真时返回 root；否则 UID/GID、已证明挂载的 workspace 属主、profile 默认值和 POSIX passwd 查询共同决定目标身份。`CONTAINER_HOME` 默认不允许离开 `bootstrap.workspace_root`，除非该输入设置 `allow_outside_workspace = true`。

解析成功的 identity 会序列化为：

```json
{
  "uid": 1000,
  "gid": 1000,
  "user": "dev",
  "home": "/home/dev",
  "run_as_root": false,
  "uid_source": "profile_default",
  "gid_source": "profile_default",
  "workspace": "not_mounted"
}
```

这一步只产生内存中的结果。是否修改 POSIX 账户由 `identity.map_user` action 决定。

## 5. 阶段、权限和 handoff

### 5.1 计划阶段

每个 action 根据 kind 和 `run_as` 得到一个计划阶段：

| 阶段 | 常见 action |
| --- | --- |
| `root` | `run_as = root` 的普通文件 action、identity action、SSH action |
| `current` | `run_as = current` 的普通文件 action |
| `target` | `run_as = target` 的普通文件 action |
| `handoff` | `process.drop_privileges`、`handoff.exec` |

模型保证依赖不能从更晚阶段指向更早阶段。所有引用 identity 的 action 自动依赖唯一的 `identity.resolve`；如果 profile 中引用身份却没有 resolve，会在构建 plan 时失败。

### 5.2 执行阶段

执行器在 action 前检查有效 UID：

- `root` 必须由有效 root 执行；
- `target` 可由有效 root 或已经是目标 UID 的进程执行；
- `current` 不额外要求 root/target。

`process.drop_privileges` 调用 POSIX backend 设置 supplementary groups、GID、UID；成功后不能恢复 root。它是 handoff 阶段最后的降权边界。

如果最终命令的第一个参数与 `bootstrap.handoff.ssh_daemon` 完全相同，并且当前进程是 root，执行器会跳过 `process.drop_privileges`，以便将 root 权限保留给 root service handoff。该例外会在 receipt 中标记，并向 daemon 注入 `HOME=/root`、`USER=root`、`LOGNAME=root`；普通命令或路径不完全相等时不享受例外。SSH session 的用户 shell 仍应由 `process.set_user_shell` 和 `login_shell` 配置完成。

### 5.3 失败和依赖

`failure` 的行为：

| 值 | 当前 action 失败 | 后续无关 action | 依赖该 action 的 action |
| --- | --- | --- | --- |
| `error` | 立即返回错误 | 不再继续 | 不执行 |
| `warn` | 记录 `failed_warn` | 继续 | `skipped_dependency` |
| `ignore` | 记录 `failed_ignore` | 继续 | `skipped_dependency` |

条件为假会记录 `skipped_condition`，同样不满足依赖成功条件。

## 6. Backend 生命周期和锁

backend 在 socket 同目录持有固定实例 `flock`。锁只防止同一容器启动第二个 backend，不参与 action 调度；action 使用按账户、路径、cgroup 和 namespace 推导的资源锁。backend 退出时内核释放 `flock`，下次 `run` 才能清理经过 `lstat` 类型校验的陈旧 socket。

socket 父目录拒绝 symlink 和不安全权限；Linux 使用 `SO_PEERCRED` 检查 peer。backend 不接收 profile 文本、action 描述或任意 spawn 命令，错误请求不会执行 bootstrap action。

## 7. Receipt

```bash
container-init --profile coding-images \
  --receipt-path /run/container-init/last-run.json run
```

receipt 以临时文件写入、`fsync` 后 rename 到目标路径，目标目录按需创建，文件模式为 `0600`。内容是 `ExecutionReport`，包含：

- resolved identity；
- 每个 action 的 id、kind、phase、origin、状态、路径、消息和结构化错误；
- 显式 `handoff.exec` 准备出的 handoff command（如果存在）。

receipt 不包含 action 的固定 `content`，也不会把 authorized keys 内容或 secret 直接写进报告。receipt 在 action 全部完成（包括 warn/ignore 结果）后写入；`failure = error` 提前返回时可能没有新的 receipt。最终 handoff 是 `exec` 替换当前进程，因此 runtime 自身的退出状态不由 container-init 再包装。

## 8. SSH 运行时行为

profile 中出现 `service.ssh.prepare` 时，CLI 才启用 SSH capability：

- action 自己的 `ssh_keygen` 路径优先；
- 未声明时使用 `/usr/bin/ssh-keygen`；
- capability 不存在或不可执行时，doctor/run 失败；
- host key 已有成对文件则保留内容，只修正 root 属主和权限；
- 只存在私钥或公钥、路径是 symlink 或非普通文件时失败；
- 不会自动启动 sshd。

SSH action 的详细字段和安全行为见 [DSL 参考](dsl.md#63-ssh-action) 与 [安全模型](security.md#5-ssh-capability)。
