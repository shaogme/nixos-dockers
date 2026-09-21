# Bootstrap DSL 参考

本文描述当前 `bootstrap-loader` 和 `bootstrap-model` 实际接受的 TOML。它是容器基础设施配置，不是 shell 配置语言：所有副作用都必须落在固定的 action kind 上，不能写任意命令、管道、重定向或 `eval`。

## 1. Profile 外层结构

一个 profile 的最小外层结构如下：

```toml
schema = 1
id = "example"
extends = ["base"]

[bootstrap]
workspace_root = "/workspace"

[bootstrap.identity]
default_user = "dev"
default_uid = 1000
default_gid = 1000
auto_mapping = true

[bootstrap.handoff]
runtime = "/usr/bin/dev-env"
exec_prefix = ["exec", "--"]
shell_prefix = ["shell"]
```

外层 `schema` 是 profile 的 schema 版本；加载时如果没有 `bootstrap.schema`，它也会作为 Bootstrap schema 版本使用。为了可读性，建议像上例一样在顶层写 `schema = 1`。`bootstrap.schema = 1` 也被支持，但不能与不同值的顶层 schema 同时出现。

同一个 profile 还可以有 `[config]`、`[features]`、`[providers]` 等 environment DSL 内容。container-init 只投影 `[bootstrap]`，不会解析或执行其他命名空间。

根级 Bootstrap 配置的必要字段是：

| 字段 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `bootstrap.schema` | 正整数 | 必填 | 当前只支持 `1` |
| `bootstrap.mode` | `"strict"` | `strict` | 当前唯一模式 |
| `bootstrap.workspace_root` | 绝对路径模板 | 必填 | 用于身份探测和路径引用 |
| `bootstrap.allow_workspace_overlay` | 布尔值 | `false` | 是否允许 workspace 来源贡献 action |
| `bootstrap.non_interactive` | `"deny"`/`"allow"` | `deny` | 当前只保存配置，执行器不主动交互 |
| `bootstrap.identity` | 表 | 空表 | 身份默认值和输入映射 |
| `bootstrap.handoff` | 表 | — | `runtime` 必须存在 |
| `bootstrap.policy` | 表 | 空集合 | workspace 安全 kind 和 admin-only kind |
| `bootstrap.inputs.NAME` | 表 | 空表 | 声明可读取的运行时输入 |
| `bootstrap.actions` | 数组表 | 空数组 | 要执行的结构化 action |

如果选中的继承图完全没有 `[bootstrap]`，加载器会报错；`schema`、`workspace_root` 和 `handoff.runtime` 最终也必须能从继承图合并出来。

## 2. identity

```toml
[bootstrap.identity]
default_user = "dev"
default_home = "/home/user"
default_uid = 1000
default_gid = 1000
auto_mapping = true
run_as_root_input = "RUN_AS_ROOT"
uid_input = "HOST_UID"
gid_input = "HOST_GID"
home_input = "CONTAINER_HOME"
```

字段说明：

| 字段 | 类型 | 作用 |
| --- | --- | --- |
| `default_user` | POSIX 用户名 | 没有更具体结果时使用的用户名；允许尚未存在，配合 `identity.map_user` 创建/映射 |
| `default_home` | 绝对路径 | 默认家目录路径；未显式通过 `home_input` 覆盖时，无论 root 还是非 root 用户均使用该默认路径（缺省为 `/home/user`） |
| `default_uid` | `0..=u32::MAX` | 默认 UID |
| `default_gid` | `0..=u32::MAX` | 默认 GID |
| `auto_mapping` | 布尔值 | 无 UID 输入时是否优先探测已确认挂载的 workspace 属主；普通 rootfs 目录不参与 |
| `run_as_root_input` | 输入名 | 指向 `bool` 输入；值为真时直接选择 root |
| `uid_input` | 输入名 | 指向 `uid_pair` 输入 |
| `gid_input` | 输入名 | 指向 `gid` 输入 |
| `home_input` | 输入名 | 指向 `path` 输入 |

`identity.resolve` 的实际决策顺序如下：

1. `run_as_root_input` 解析为真时使用 UID/GID `0:0`、用户 `root`；HOME 使用声明的 home 输入，否则 `/root`。
2. UID 使用 `uid_input` 的 UID；`uid_pair` 中带有的 GID 作为候选 GID。
3. 如果没有 UID 输入且 `auto_mapping = true`，只使用 platform backend 证明为挂载点的 workspace 属主；无法证明时不使用目录属主。
4. 之后依次考虑 `default_uid`、配置用户名对应的 passwd UID、当前进程 UID。
5. GID 的优先级为：`gid_input`、`uid_pair` 中的 GID、workspace GID、`default_gid`、配置用户的 GID，最后回退为 UID。
6. UID 0 的用户名固定为 `root`；非零 UID 才使用 `default_user`、按 UID 查找 passwd 或 `uid-<uid>`。
7. HOME 优先使用 `home_input`，其次是规范用户名对应的 passwd HOME，最后 root 使用 `/root`，普通用户使用 `/home/<user>`。

`HOST_UID`/`HOST_GID` 等 UID/GID 输入必须声明 `namespace = "host"` 或
`namespace = "container"`。host 值会通过 `/proc/self/uid_map` 或
`/proc/self/gid_map` 转为当前 namespace 的 ID；未映射值和 map 解析错误会直接失败，
不会原样使用。UID 与 GID 输入必须使用同一 namespace。

这里的“解析身份”不会自动修改 `/etc/passwd`。需要修改或创建 POSIX 账户条目时，必须显式添加 `identity.map_user`，并且该 action 必须来自受信任 profile、以 root 运行。

## 3. handoff

```toml
[bootstrap.handoff]
runtime = "/usr/bin/dev-env"
exec_prefix = ["exec", "--"]
shell_prefix = ["shell"]
ssh_daemon = "/usr/sbin/sshd"
login_shell = "/usr/bin/dev-env-login-shell"
```

`runtime` 必须是绝对路径，且不能包含空格或插值。所有 prefix 元素都必须是非空、不含 NUL/换行的 argv 值。

构造 handoff argv 的规则是：

```text
无显式 command: [runtime] + shell_prefix
有显式 command: [runtime] + exec_prefix + command
```

例如：

```toml
runtime = "/bin/sh"
exec_prefix = ["-c"]
shell_prefix = ["-c", "true"]
```

```text
container-init ... run -- 'printf "hello\n"'
→ execve("/bin/sh", ["/bin/sh", "-c", "printf \"hello\\n\""])
```

container-init 通过 `exec` 替换当前进程，不启动子 shell。完成身份切换后，它会把 `HOME`、`USER` 和 `LOGNAME` 设置为解析出的目标身份，再执行 runtime。

`ssh_daemon` 和 `login_shell` 只用于 profile 元数据与 doctor 检查，以及 root service handoff 的识别；container-init 不会因为写了 `ssh_daemon` 就启动 sshd。

## 4. 类型化输入

输入名是大写环境变量名，只允许大写字母、数字和下划线，并且首字符必须是大写字母或下划线。

```toml
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

[bootstrap.inputs.RUN_AS_ROOT]
target = "identity.run_as_root"
type = "bool"
runtime = true
default = false

[bootstrap.inputs.CONTAINER_HOME]
target = "identity.home"
type = "path"
runtime = true
allow_outside_workspace = false
```

### 4.1 字段和类型

| 字段 | 是否必填 | 说明 |
| --- | --- | --- |
| `target` | 是 | 只能是 `identity.uid`、`identity.gid`、`identity.run_as_root`、`identity.home` |
| `type` | 是 | 必须与 target 匹配：`uid_pair`、`gid`、`bool`、`path` |
| `aliases` | 否 | 其他合法环境变量名；所有输入和 alias 在整个合并结果中不得重名 |
| `runtime` | 是 | `true` 才读取 CLI/环境；`false` 只使用 `default` |
| `format` | 否 | 给 profile 使用者看的格式说明；当前解析器按 `type` 工作，不依据该字符串扩展语法 |
| `namespace` | UID/GID 必填 | `host` 表示当前进程 user namespace 的父 namespace；`container` 表示当前 namespace；其他类型不得设置 |
| `default` | 否 | TOML 布尔、整数或字符串；会按声明类型解析 |
| `allow_outside_workspace` | 否 | 仅影响 `identity.home`；默认拒绝 workspace_root 之外的 HOME |

当前解析规则：

| 类型 | 接受的值 |
| --- | --- |
| `uid_pair` | `uid` 或 `uid:gid`，两部分都是十进制无符号整数 |
| `gid` | 十进制无符号整数 |
| `bool` | `1`、`0`、`true`、`false`，大小写仅接受 `TRUE`/`FALSE`/`True`/`False` |
| `path` | 绝对路径，不接受 `${...}`；实际 HOME 的 workspace 限制另行检查 |

运行时输入查找顺序为：canonical name 的 CLI 值、alias 的 CLI 值、canonical name 的环境值、alias 的环境值、`default`。CLI 的 `--input` 和 `--set` 是同义选项。

## 5. Action 通用字段

```toml
[[bootstrap.actions]]
id = "config-dir"
kind = "filesystem.ensure_dir"
path = "${identity.home}/.config/app"
mode = "0755"
owner = "identity.target"
when = "writable(${identity.home})"
failure = "error"
run_as = "target"
depends_on = ["resolve"]
reason = "keep application configuration under the target HOME"
sensitivity = "public"
recursive = false
```

| 字段 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `id` | 字符串 | 必填 | action 标识；只允许字母、数字、`.`、`_`、`-`，在合并结果中唯一 |
| `kind` | enum | 必填 | 使用下表中的 dotted spelling；loader 会转换为内部 snake_case enum |
| `path` | 路径模板 | 无 | 目录、文件、chown/chmod 等使用 |
| `link` | 路径模板 | 无 | 软链接 action 使用 |
| `target` | 路径模板 | 无 | 软链接目标使用 |
| `mode` | 八进制字符串 | 无 | 最多四位 `0`–`7`，例如 `0755`、`1777` |
| `parent_mode` | 八进制字符串 | 无 | 软链接父目录的 mode |
| `owner` | `root`、`identity.target` 或 `uid:gid` | 无 | 文件/目录属主 |
| `user` | 用户名或 `identity.target` | 无 | `process.set_user_shell` 的用户 |
| `shell` | 绝对可执行文件路径 | 无 | `process.set_user_shell` 的登录 shell |
| `content` | 字符串 | 无 | 固定文件内容或 authorized keys；不能含 NUL |
| `when` | 条件字符串 | 总是 | 运行时条件；只能使用[受限条件语法](#7-条件语法) |
| `failure` | `error`、`warn`、`ignore` | `error` | 失败时停止、记录告警后继续，或记录后继续 |
| `run_as` | `root`、`target`、`current` | `current` | 执行动作时需要的有效身份 |
| `depends_on` | action id 数组 | 空数组 | 显式依赖；身份引用还会自动依赖 resolve |
| `reason` | 字符串 | 无 | 变更理由；显式 override 时建议写入 |
| `sensitivity` | `public`、`sensitive`、`secret` | `public` | 诊断元数据；计划和 receipt 不写入 action content |
| `recursive` | 布尔值 | `false` | 仅 `filesystem.chown`/`filesystem.chmod` 有意义 |

源码使用 `#[serde(deny_unknown_fields)]` 解析 action，因此拼错字段会被拒绝。

## 6. 内置 action kind

DSL 中使用 dotted 名称，例如 `filesystem.ensure_dir`；下面的“必填字段”是当前模型校验的要求。

| kind | 必填字段 | 执行效果 | 默认阶段/限制 |
| --- | --- | --- | --- |
| `identity.resolve` | 无 | 将身份解析作为计划中的显式节点 | 必须 `run_as = "root"`；最多一个 |
| `identity.map_user` | 无 | 原子更新目标 passwd 条目，并在需要时补 group 条目 | 必须 root；仅 trusted image/admin 来源 |
| `identity.ensure_home` | 无 | 创建 HOME 目录并 reconcile mode/owner | 必须 root；path 省略时使用解析后的 HOME，默认 `0755`/`identity.target` |
| `filesystem.ensure_dir` | `path` | 幂等创建目录，随后设置 mode/owner | 不接受已有非目录路径或路径中的意外 symlink |
| `filesystem.ensure_file` | `path`，且 `content`/`mode` 至少一个 | 固定内容/权限的幂等文件 | 已有文件内容不同会失败，不会静默覆盖 |
| `filesystem.ensure_symlink` | `link`、`target` | 创建或校验软链接 | 已有链接目标必须完全相同；真实文件、目录或 mount point 会失败 |
| `filesystem.chown` | `path`、`owner` | 设置节点属主 | `recursive` 默认 false；递归不跟随 symlink，不能指向 `/` |
| `filesystem.chmod` | `path`、`mode` | 设置节点权限 | `recursive` 默认 false；拒绝 symlink、wildcard 和递归 `/` |
| `process.set_user_shell` | `user`、`shell` | 更新 passwd 的 login shell 字段 | 仅 trusted image/admin 来源；目标用户必须存在 |
| `process.drop_privileges` | 无 | `initgroups`/`setgroups` 后执行 `setgid`、`setuid` | 必须 root；最多一个；计划阶段为 handoff |
| `service.ssh.prepare` | host key、authorized key、runtime 三个目录 | 创建 SSH 目录、生成/校验 host key、可选写入 authorized keys | 必须 root、trusted 来源；需启用 SSH capability |
| `cgroup.v2_init` | 无（path/subgroup/controllers/mount_mode 均有默认值） | 校验并初始化 cgroup v2、根进程迁移与控制器委托；支持默认就地模式与只读环境下的挂载覆挂重定向 | 必须 root、仅 trusted 来源；计划阶段为 Root |
| `handoff.exec` | 无 | 将最终命令包装成 handoff runtime 命令 | 仅 trusted 来源；最多一个；非幂等、必须位于 handoff 阶段 |

### 6.1 文件 action 示例

```toml
[[bootstrap.actions]]
id = "data-dir"
kind = "filesystem.ensure_dir"
path = "/data/app"
mode = "1777"
owner = "identity.target"
run_as = "root"

[[bootstrap.actions]]
id = "app-config"
kind = "filesystem.ensure_file"
path = "${identity.home}/.config/app/config.toml"
content = "mode = \"safe\"\n"
mode = "0600"
owner = "identity.target"
run_as = "target"
depends_on = ["data-dir"]

[[bootstrap.actions]]
id = "app-link"
kind = "filesystem.ensure_symlink"
link = "${identity.home}/.config/app/shared"
target = "/data/app"
parent_mode = "0755"
owner = "identity.target"
run_as = "target"
depends_on = ["data-dir"]
```

`ensure_file` 对新文件使用临时文件、写入、`fsync`、rename；父目录会按需创建，但 action 的 `mode`/`owner` 只作用于目标文件。已有文件如果声明了 `content`，内容必须一致；不一致就是错误。

`ensure_symlink` 不会删除真实目录来“腾位置”，也不会替换目标不同的已有软链接。给 link 设置 owner 使用 `lchown`，不会跟随链接修改 target 的属主。

### 6.2 POSIX 账户示例

```toml
[[bootstrap.actions]]
id = "resolve"
kind = "identity.resolve"
run_as = "root"

[[bootstrap.actions]]
id = "map-user"
kind = "identity.map_user"
run_as = "root"
depends_on = ["resolve"]

[[bootstrap.actions]]
id = "home"
kind = "identity.ensure_home"
path = "${identity.home}"
mode = "0755"
owner = "identity.target"
run_as = "root"
depends_on = ["map-user"]

[[bootstrap.actions]]
id = "login-shell"
kind = "process.set_user_shell"
user = "identity.target"
shell = "/usr/bin/dev-env-login-shell"
run_as = "root"
depends_on = ["map-user"]
```

`identity.map_user` 会保留既有 passwd 内容，reconcile 目标用户的 UID/GID/HOME；目标用户不存在时追加基本条目，默认 shell 为 `/bin/sh`。目标 GID 已存在时，会把目标用户幂等加入所有匹配 group entry；不存在时追加空成员字段的合法 group entry。passwd/group 会先同时校验，再在 bootstrap lock 内分别原子替换。

### 6.3 SSH action

```toml
[[bootstrap.actions]]
id = "resolve"
kind = "identity.resolve"
run_as = "root"

[[bootstrap.actions]]
id = "prepare-ssh"
kind = "service.ssh.prepare"
run_as = "root"
host_key_dir = "/etc/ssh"
authorized_keys_dir = "/etc/ssh/authorized_keys"
runtime_dir = "/run/sshd"
host_key_types = ["rsa", "ed25519"]
ssh_keygen = "/usr/bin/ssh-keygen"
authorized_keys_source = "/run/secrets/authorized_keys.pub"
depends_on = ["resolve"]
```

`host_key_types` 当前支持 `rsa`、`ed25519`、`ecdsa`；省略时默认 `rsa` 和 `ed25519`。每种 host key 都必须以私钥和 `.pub` 公钥成对存在：两者都不存在才生成，两者都存在则只 reconcile 为私钥 `0600`、公钥 `0644`，只存在一方、是 symlink 或是非普通文件都会失败。

`authorized_keys_source` 可选。源文件不存在时只准备目录和 host key；源文件存在时，以解析出的用户名为文件名写入 `authorized_keys_dir/<user>`，并以 root/`0644` 管理。也可以用 `content` 固定声明内容，但不能同时使用 source 和 content。内容冲突时拒绝覆盖。

SSH capability 只提供受信任的 keygen 可执行文件；action 自己声明所有路径。container-init 不执行 sshd，也不内置任何镜像专用目录。

兼容早期 DSL 的别名仍可使用：`path` 作为 `host_key_dir`、`target` 作为 `authorized_keys_dir`、`link` 作为 `runtime_dir`；新配置应使用命名字段。

### 6.4 cgroup v2 初始化示例

在容器内嵌套运行 Podman / Docker / crun 等容器引擎时，cgroup v2 规范禁止存在内部进程的层级开启子树控制器（"no internal processes" 规则）。若容器内 PID 1 或当前进程位于根 cgroup，向根节点的 `cgroup.subtree_control` 写入会报错 `EBUSY`；而子层级（如 `/libpod_parent/...`）试图启用控制器时则会报错 `ENOENT: write cgroup.subtree_control: no such file or directory`。

此外，在不同的容器运行环境（特权非只读 vs 非特权只读）下，cgroup 挂载点的读写状态存在差异：
- **非只读环境（特权/读写环境）**：容器拥有宿主赋权的 `CAP_SYS_ADMIN` 或 `/sys/fs/cgroup` 挂载为可写（`rw`），可以直接就地操作。
- **只读环境（只读/非特权环境）**：在以 `--read-only` 启动的容器，或外部容器引擎（Docker / containerd）默认以非特权模式运行容器时，宿主会为 `/sys/fs/cgroup` 施加 `MS_RDONLY` 只读保护。若直接向其写入会遭遇 `Read-only file system (os error 30)` 错误。

`cgroup.v2_init` action 声明在容器启动阶段由 root 自动化完成 cgroup v2 初始化与子树委托，并通过子配置项选项支持不同的挂载模式：

```toml
[[bootstrap.actions]]
id = "cgroup-init"
kind = "cgroup.v2_init"
run_as = "root"
```

完整可选参数如下：

```toml
[[bootstrap.actions]]
id = "cgroup-init"
kind = "cgroup.v2_init"
path = "/sys/fs/cgroup"
mount_mode = "default" # "default"（默认就地初始化）或 "bind_mount"（挂载覆挂重定向）
shadow_path = "/run/cgroup" # 仅在 mount_mode = "bind_mount" 时使用，默认 /run/cgroup
subgroup = "init"
controllers = ["cpu", "io", "memory", "pids"]
owner = "root"
run_as = "root"
```

字段说明：
- `mount_mode`（别名 `mode`）：可选。初始化挂载模式，默认 `"default"`：
  - `"default"`（**默认模式**）：就地初始化。直接在目标路径（`path`，默认 `/sys/fs/cgroup`）执行子组创建、进程排空与控制器委托。适用于目标路径自身可写的非只读环境（如特权容器）。
  - `"bind_mount"`（**挂载覆挂重定向模式**）：在独立可写虚拟内存文件系统（`shadow_path`，默认 `/run/cgroup`）上挂载私有 cgroup2 树并完成子组初始化与控制器委托，随后通过 `mount --bind` 覆挂重定向至目标 `path`（默认 `/sys/fs/cgroup`）。该模式专为只读环境（如 Docker `--read-only` 或受限 `ro` cgroup2 挂载）设计，无需依赖宿主特权即可使 `/sys/fs/cgroup` 变为可写层级，向下游 OCI 运行时无缝提供标准 cgroup 树。
- `shadow_path`：可选。挂载覆挂重定向模式下的独立可写暂存路径，默认 `"/run/cgroup"`。
- `path`：可选。目标 cgroup 根路径，默认 `"/sys/fs/cgroup"`。
- `subgroup`：可选。用于移入容器根进程的子组目录名称，默认 `"init"`（对应 `<path>/init`）。
- `controllers`：可选。要启用到 `cgroup.subtree_control` 的控制器列表。未指定时自动读取目标 cgroup2 层级中 `cgroup.controllers` 的所有可用控制器进行全量委托。若指定控制器不可用，校验会直接报错失败。
- `owner`：可选。子组目录的属主（例如 `"identity.target"`）。
- `run_as`：必须为 `"root"`，且仅能来自受信任 profile。

执行逻辑：
1. **模式判定与准备**：
   - 若 `mount_mode = "default"`：基准工作目录为 `path`（默认 `/sys/fs/cgroup`）。校验该路径存在且包含 `cgroup.controllers`。
   - 若 `mount_mode = "bind_mount"`：基准工作目录为 `shadow_path`（默认 `/run/cgroup`）。若尚未挂载，则在当前私有命名空间（User + Mount + Cgroup Namespace）中挂载 `cgroup2` 文件系统至 `shadow_path`。
2. **创建子组**：在基准工作目录下创建子组 `<work_dir>/<subgroup>`（默认 `<work_dir>/init`）。
3. **排空进程**：读取 `<work_dir>/cgroup.procs`，将所有既有进程迁移至 `<work_dir>/<subgroup>/cgroup.procs`，清空根层级的进程占用以满足 cgroup v2 规范。
4. **委托控制器**：读取已在 `cgroup.subtree_control` 启用的控制器，通过追加写入 `+<controller>` 启用目标控制器（支持 EBUSY 自动重试排空，幂等执行）。
5. **属主对齐**：若配置了 `owner`，对子组目录执行属主对齐。
6. **覆挂重定向**（仅 `mount_mode = "bind_mount"`）：通过 `mount --bind <shadow_path> <path>` 将初始化好的可写 cgroup2 树覆盖挂载至目标 `path`（默认 `/sys/fs/cgroup`），原只读挂载点被新层级覆盖，使下游程序（如 `crun`/`podman`）能够透明读写标准路径。

## 7. 插值与条件

### 7.1 路径插值

路径允许绝对路径或以结构化引用开头的模板：

```toml
path = "${identity.home}/.cache/tool"
target = "${bootstrap.workspace_root}/shared"
```

当前执行器可靠支持的引用是：

| 引用 | 值 |
| --- | --- |
| `${bootstrap.workspace_root}` | 合并后的 workspace_root |
| `${identity.uid}` / `${identity.gid}` | 解析出的数字 ID |
| `${identity.user}` / `${identity.target}` | 解析出的用户名 |
| `${identity.home}` | 解析出的绝对 HOME |
| `${context.cwd}` | 当前 runtime workspace |
| `${context.os}` / `${context.arch}` | 当前编译目标的 OS/架构 |
| `${input.NAME}` | 已解析输入的原始字符串 |
| `${env.NAME}` | 当前进程环境中的值 |

模型还允许 `config.*` 作为结构化引用形状，但当前 CLI/runtime 没有向插值上下文提供 environment DSL 的已解析 config 值；不要在可执行 bootstrap profile 中依赖它。

路径会拒绝 NUL、换行、`..`、shell 元字符、反引号、命令替换、管道、重定向和不完整的 `${...}`。渲染后必须仍然是绝对路径。执行器还会逐组件检查已有路径，默认拒绝意外的 symlink。

### 7.2 条件语法

```toml
when = "input_set(HOST_UID) && writable(${bootstrap.workspace_root})"
when = "identity.uid == 1000"
when = "identity.user != 'root'"
when = "exists(${identity.home}) || feature:services.ssh"
when = "!false"
```

支持的形式：

| 形式 | 语义 |
| --- | --- |
| `always` | 总为真 |
| `true` / `false` | 布尔字面量 |
| `context.path_exists_or_create` | 兼容用条件；当前求值为真 |
| `exists(path)` | 路径存在 |
| `writable(path)` | 路径或最近存在的父目录可写 |
| `input_set(NAME)` | 输入已成功解析 |
| `feature:path` | runtime feature 集合包含该路径 |
| `left == right` / `left != right` | 字符串、布尔或结构化引用比较 |
| `!condition` | 取反 |
| `a && b` / `a || b` | 与/或；支持括号 |

比较值可以是 `true`、`false`、纯数字、单/双引号字符串或受支持的引用。条件由 Rust parser 求值，不经过 shell。

当前 CLI 没有 feature 注入选项，因此 `feature:...` 在普通 CLI 运行中通常为 false；库调用者可以通过 `RuntimeContext::with_feature` 提供 feature。条件引用不存在的 runtime input 或不存在的 ambient `env.NAME` 时，会产生错误，而不是自动当作空字符串。

## 8. 计划阶段和依赖

`BootstrapConfig::build_plan` 不访问文件系统、不解析运行时输入，也不执行 action。它会：

1. 校验所有 action；
2. 建立 action id 索引；
3. 自动为引用身份的 action 添加 `identity.resolve` 依赖；
4. 检查缺失依赖、重复 resolve、重复 drop、重复 handoff；
5. 检查依赖是否指向更晚阶段；
6. 使用稳定拓扑排序生成 `PlannedAction`。

计划阶段枚举为 `root`、`current`、`target`、`handoff`。`process.drop_privileges` 和 `handoff.exec` 固定属于 handoff 阶段；如果存在 drop，计划会自动让 handoff 依赖 drop。一个动作失败或条件跳过后，依赖它的动作不会执行。

`handoff.exec` 不是必须的：即使 profile 没有这个 action，`run` 完成其他 action 后仍会按 `[bootstrap.handoff]` 构造最终命令。把它显式写入计划适合需要审计 handoff 边界的 profile。
