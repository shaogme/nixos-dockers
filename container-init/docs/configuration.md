# 配置与部署

本页说明 `container-init` 如何发现 profile、合并继承图，以及如何在容器镜像中布置它。profile 是 TOML 文件；文件名只用于定位文件，真正的 profile id 来自文档中的 `id` 字段。

## 1. 推荐的文件布局

CLI 默认读取：

```text
/etc/dev-env/
├── default-profile          # 一个 profile id，不能包含空白
└── profiles.d/
    ├── base.toml
    ├── coding-images.toml
    └── ssh.toml
```

每个 `*.toml` 都会被加载；非 TOML 文件会被忽略。目录内文件按路径排序读取，主要用于让重复 profile id 的诊断稳定。profile id 不必与文件名一致，但同一来源内不能重复。

典型 Docker 镜像配置：

```dockerfile
COPY profiles.d/ /etc/dev-env/profiles.d/
COPY default-profile /etc/dev-env/default-profile
COPY container-init /usr/bin/container-init

ENTRYPOINT ["/usr/bin/container-init", "run"]
```

`container-init` 不要求 runtime 叫 `dev-env`；由每个 profile 的 `bootstrap.handoff.runtime` 决定最终执行哪个绝对路径。

## 2. 路径发现和优先级

### 2.1 Profile 目录

按优先级选择 image profile 目录：

1. `--profiles-dir PATH` 或 `--profile-dir PATH`；
2. `CONTAINER_INIT_PROFILE_DIR`；
3. `CONTAINER_INIT_PROFILES_DIR`；
4. `/etc/dev-env/profiles.d`。

按优先级选择 admin profile 目录：

1. `--admin-profiles-dir PATH`；
2. `CONTAINER_INIT_ADMIN_PROFILES_DIR`；
3. 未配置则不加载 admin profile。

image 目录中的 profile 标记为 `image_profile`，admin 目录中的 profile 标记为 `admin_profile`，两者都是 trusted source。两个目录出现相同 id 时会报 duplicate profile，而不是按目录顺序覆盖。

### 2.2 Profile 选择

按优先级选择要加载的 profile id：

1. `--profile ID`；
2. `CONTAINER_INIT_PROFILE`；
3. `CONTAINER_INIT_PROFILE_ID`；
4. 默认 profile 文件中的内容。

默认 profile 文件按优先级选择：

1. `--default-profile PATH` 或 `--default-profile-file PATH`；
2. `CONTAINER_INIT_DEFAULT_PROFILE`；
3. `CONTAINER_INIT_DEFAULT_PROFILE_FILE`；
4. `/etc/dev-env/default-profile`。

默认文件必须只包含一个非空、无空白的 profile id，例如：

```text
coding-images
```

### 2.3 Workspace

workspace 路径按优先级选择：

1. `--workspace PATH` 或 `--cwd PATH`；
2. `CONTAINER_INIT_WORKSPACE`；
3. `WORKSPACE`；
4. 当前工作目录。

相对 workspace 会相对于启动 `container-init` 时的当前目录转换为绝对路径。`run` 会在执行 action 前切换到该目录；`plan` 和 `doctor` 不切换当前进程目录。

注意：运行时 workspace 主要用于 `RuntimeContext.cwd`、workspace 属主探测和条件求值；profile 中的 `bootstrap.workspace_root` 仍是声明式配置，二者不自动相互覆盖。希望二者一致时，应在镜像生成 profile 时写入同一个绝对路径。

## 3. 一个可继承的 profile 集合

### 3.1 基础 profile

```toml
# base.toml
schema = 1
id = "base"

[bootstrap]
workspace_root = "/workspace"
mode = "strict"
allow_workspace_overlay = false
non_interactive = "deny"

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
runtime = true
format = "uid[:gid]"

[bootstrap.inputs.HOST_GID]
target = "identity.gid"
type = "gid"
runtime = true

[bootstrap.inputs.CONTAINER_HOME]
target = "identity.home"
type = "path"
runtime = true
allow_outside_workspace = false

[bootstrap.handoff]
runtime = "/usr/bin/dev-env"
exec_prefix = ["exec", "--"]
shell_prefix = ["shell"]
login_shell = "/usr/bin/dev-env-login-shell"

[[bootstrap.actions]]
id = "resolve"
kind = "identity.resolve"
run_as = "root"

[[bootstrap.actions]]
id = "home"
kind = "identity.ensure_home"
path = "${identity.home}"
mode = "0755"
owner = "identity.target"
run_as = "root"
depends_on = ["resolve"]

[[bootstrap.actions]]
id = "drop"
kind = "process.drop_privileges"
run_as = "root"
depends_on = ["home"]

[[bootstrap.actions]]
id = "handoff"
kind = "handoff.exec"
run_as = "current"
depends_on = ["drop"]
```

### 3.2 派生 profile

子 profile 可以增加新的 action；父 profile 没有的标量字段也可以补齐：

```toml
# coding-images.toml
schema = 1
id = "coding-images"
extends = ["base"]

[[bootstrap.actions]]
id = "coding-config"
kind = "filesystem.ensure_dir"
path = "/data/coding-config"
mode = "1777"
owner = "identity.target"
run_as = "root"
depends_on = ["resolve"]

[[bootstrap.actions]]
id = "opencode-link"
kind = "filesystem.ensure_symlink"
link = "${identity.home}/.config/opencode"
target = "/data/coding-config/opencode"
parent_mode = "0755"
owner = "identity.target"
run_as = "target"
depends_on = ["coding-config"]
```

子 profile 不需要重复父 profile 的全部配置。加载器先按父到子的顺序合并，并保留 action 的 provenance。父 profile 的 `resolve`、`home`、`drop` 和 `handoff` 会继续存在。

## 4. 继承和冲突

`extends` 是 profile id 数组。加载器会递归读取父 profile，检测缺失父 profile 和继承环；多条路径重复到达同一个父 profile 时只合并一次。最终 profile chain 以父到子的顺序展示在 `plan` 输出中。

合并规则如下：

- 相同标量值重复声明是允许的；不同标量值默认报冲突；
- 相同 action id 且完整定义相同是允许的；定义不同默认报冲突；
- 相同输入名且定义相同是允许的；定义不同默认报冲突；
- policy 中的两个 action kind 集合只做加法，子 profile 不能移除父 profile 的限制；
- 冲突必须使用显式 `override` 并提供非空 `reason`；
- override 由当前子 profile 声明，不能靠“后加载所以覆盖”绕过检查。

例如，修改继承来的 runtime：

```toml
schema = 1
id = "admin-coding"
extends = ["coding-images"]

[bootstrap.handoff]
runtime = "/usr/local/bin/dev-env"

[override."bootstrap.handoff.runtime"]
op = "set"
reason = "admin runtime is installed at the system-local path"
```

修改 action 或 input 时：

```toml
[override."bootstrap.actions.coding-config"]
op = "set"
reason = "move the shared configuration directory to the administrator-managed mount"

[[bootstrap.actions]]
id = "coding-config"
kind = "filesystem.ensure_dir"
path = "/srv/coding-config"
mode = "1777"
owner = "identity.target"
run_as = "root"
depends_on = ["resolve"]
```

支持 override 的路径包括根配置、identity 字段、handoff 字段、`bootstrap.actions.<id>` 和 `bootstrap.inputs.<NAME>`。`op` 省略时默认按 `set` 处理；写出 `op = "set"` 更清楚。其他操作会被拒绝。

覆盖 action 时必须重新写出完整 action 定义，因为 loader 替换的是整个 action，而不是逐字段 patch。`reason` 如果没有写在 action 中，loader 会将 override 的 reason 补到 action provenance 中。

## 5. 来源和信任

| 来源 | CLI 当前是否加载 | 是否 trusted | Bootstrap 权限 |
| --- | --- | --- | --- |
| image profile | 是 | 是 | 可以声明完整配置、root action、SSH、账户和 handoff |
| admin profile | 可选 | 是 | 与 image profile 相同；适合运维策略和显式 override |
| workspace overlay | CLI 未提供加载入口 | 否 | 仅在允许时声明安全 action，且必须 `run_as = "target"` |
| user overlay | 否 | 否 | 不能进入 bootstrap projection |
| runtime/CLI | 仅作为输入 | 否 | 只能设置 profile 已声明的输入，不能改 profile 结构 |

不受信任的 workspace profile 只能含有 `[bootstrap.actions]`。它不能贡献 `schema`、`identity`、`handoff`、`policy`、`inputs` 或 bootstrap override。父 profile 必须设置：

```toml
[bootstrap]
allow_workspace_overlay = true

[bootstrap.policy]
workspace_safe_action_kinds = [
  "filesystem.ensure_dir",
  "filesystem.ensure_symlink",
]
```

workspace action 仍需同时满足：kind 在安全集合中、`run_as = "target"`、action 来源是 workspace。workspace 不能替换继承 action，也不能声明 root action。当前 CLI 没有从 workspace 自动读取这种 overlay；这是 loader/model 的可用能力，需由上层集成显式以 `ProfileSource::new(id, SourceKind::WorkspaceOverlay, contents)` 传入。

## 6. 与 NixOS/Docker 的集成建议

container-init 本身不包含 NixOS module，也不推断镜像目录。NixOS 或镜像构建层应负责：

1. 把编译出的 `/usr/bin/container-init` 放进镜像；
2. 写入 `/etc/dev-env/profiles.d` 和默认 profile；
3. 确保 profile 中的 `runtime`、`login_shell`、`ssh_daemon`（如果使用）是真实绝对路径；
4. 为 `run_as = "root"` 的 profile action 选择 root entrypoint；
5. 需要 SSH 时确保 `ssh-keygen` 存在，并在 action 中声明 `ssh_keygen` 或使用默认 `/usr/bin/ssh-keygen`；
6. 给持久化数据目录配置正确 mount 与权限，避免把真实 mountpoint 当成待替换的 symlink；
7. 在镜像构建和运行时分别执行 `plan`、`doctor` 与测试。

Docker 的 `ENTRYPOINT ["/usr/bin/container-init", "run"]` 只规定入口；真正的最终 runtime 和参数由 profile handoff 决定。`docker exec` 不应再次调用 container-init 作为隐含前置；需要新会话时应直接调用上层 runtime，例如 `dev-env shell`。

## 7. 常见配置错误

### profile 找不到

检查 profile id 是否与 TOML 内的 `id` 一致，并确认搜索目录里扩展名是 `.toml`：

```bash
container-init --profiles-dir /etc/dev-env/profiles.d \
  --profile coding-images plan --json
```

### profile graph 没有 bootstrap

至少一个 profile 必须提供 `[bootstrap]`；只有 environment DSL 的 profile 不能作为 container-init profile。

### 标量冲突

不要依赖文件名顺序。检查错误中列出的两个 origin，在子 profile 增加 `[override."..."]` 和非空 `reason`，或者删除重复声明。

### action 冲突

继承 action id 相同但定义变化时必须完整重写 action，并声明 `override."bootstrap.actions.<id>"`。workspace 来源不能这样做。

### 输入不生效

确认输入的 `target`、`type` 和 `runtime`；`runtime = false` 时环境变量会被有意忽略。确认 `identity.*_input` 引用的是 canonical name 或 alias，且 `--input` 的名字已经在 profile 中声明。
