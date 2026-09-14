# dev-env 配置与部署

本页说明 CLI 如何发现 profile、选择 workspace、加载 overlay，以及各层如何影响最终的 `ResolvedConfig`。字段定义见 [DSL 参考](dsl.md)，provider 行为见 [Provider 参考](providers.md)。

## 1. 文件布局

镜像中的默认布局是：

```text
/etc/dev-env/
├── default-profile                 # 例如 coding-images-rust
├── config.toml                     # 可选管理员 overlay
└── profiles.d/
    ├── 00-nixos-docker.toml        # 基础 image profile
    ├── 20-coding-images.toml       # 派生 image profile
    └── 30-project.toml             # 镜像构建时加入的 profile
```

用户和工作区可以继续增加：

```text
${XDG_CONFIG_HOME:-$HOME/.config}/dev-env/config.toml
/workspace/.dev-env.toml
/workspace/.dev-env.local.toml
/workspace/subdir/.dev-env.toml
/workspace/subdir/.dev-env.local.toml
```

`profiles.d` 中只有扩展名为 `.toml` 的普通文件会被读取，文件名不参与继承关系；profile 的真实 id 来自 TOML 的 `id`。同一个加载目录中如果出现重复 id，直接报错，不会按文件名覆盖。

默认 profile 文件必须只包含一个非空 token：

```text
coding-images-rust
```

空文件、包含多个非空 token 或找不到被选中的 profile 都是配置错误。

## 2. Profile 选择和路径覆盖

### Profile 目录

镜像 profile 目录按以下顺序选择：

1. CLI `--profiles-dir PATH` 或别名 `--profile-dir PATH`；
2. `DEVENV_PROFILES_DIR`；
3. `DEVENV_PROFILE_DIR`；
4. `/etc/dev-env/profiles.d`。

可选的受信任管理员 profile 目录按以下顺序选择：

1. CLI `--admin-profiles-dir PATH`；
2. `DEVENV_ADMIN_PROFILES_DIR`；
3. 未设置则不读取管理员 profile 目录。

两个目录中的 profile 都会进入同一个 profile 索引；相同 id 会产生 duplicate 错误，而不是让 admin 文件覆盖 image 文件。

### Profile id

1. `--profile ID`；
2. `DEVENV_PROFILE`；
3. `DEVENV_PROFILE_ID`；
4. 默认 profile 文件中的 id。

默认 profile 文件按以下顺序选择：

1. `--default-profile-file PATH` 或 `--default-profile PATH`；
2. `DEVENV_DEFAULT_PROFILE`；
3. `DEVENV_DEFAULT_PROFILE_FILE`；
4. `/etc/dev-env/default-profile`。

CLI 的 profile id 选择高于环境变量。`DEVENV_DEFAULT_PROFILE` 在这里是“默认 profile 文件路径”，不是 profile id；要指定 id 应使用 `DEVENV_PROFILE`。

### Workspace 和 cwd

workspace root 的来源顺序为：

1. `--workspace PATH`；
2. `DEVENV_WORKSPACE`；
3. 选中 profile 的 `config.workspace.root`。

当前目录的来源顺序为：

1. `--cwd PATH`；
2. `DEVENV_CWD`；
3. 进程启动时的 cwd。

相对路径相对于进程启动 cwd 规范化为绝对路径。最终 cwd 必须位于 workspace root 之下；否则在加载 overlay 前失败。`..` 会做词法归一化，但不能用它让 cwd 逃出 workspace。

`config.workspace.search = "fixed"` 时只检查 workspace root；`"upward"` 时还检查 workspace root 到 cwd 的每一级目录，顺序是从根到当前目录。

## 3. Overlay 顺序

低优先级在前，高优先级在后：

```text
profile inheritance chain
    < /etc/dev-env/config.toml
    < user config
    < workspace root → cwd 的 .dev-env.toml
    < 每个目录紧随其后的 .dev-env.local.toml
    < --config / DEVENV_CONFIG 指定的文件
    < 声明的 runtime inputs 和 DEVENV_OVERRIDE__...
    < --set / --unset
```

其中：

- admin config 是可选文件，不存在时跳过；
- user config 是 `${XDG_CONFIG_HOME}/dev-env/config.toml`，没有 `XDG_CONFIG_HOME` 时使用 `$HOME/.config/dev-env/config.toml`；
- workspace overlay 只有文件存在时加载；
- `.dev-env.local.toml` 适合个人选择，应该加入 Git ignore；
- `--config`/`DEVENV_CONFIG` 是额外 workspace overlay，存在时必须可读；
- runtime 和 CLI 不是任意文本覆盖，而是经过声明、类型和 policy 检查的最后两层。

overlay 文件可以省略 `schema` 和 `id`。CLI 会在内存中补上元数据，然后按 overlay 处理。完整 profile 文件仍应显式写出 `schema = 1` 和 `id = "..."`。

## 4. Workspace 配置歧义

同一目录不应同时存在：

```text
.dev-env.toml
.dev-env/config.toml
```

没有显式 `--config`/`DEVENV_CONFIG` 时，CLI 报 ambiguous workspace config；它不会猜测哪一个优先。需要保留两份文件时，显式选中其中一份：

```bash
dev-env --config /workspace/.dev-env/config.toml print --format json
```

`--config` 文件会在自动发现的 workspace overlay 之后应用，因此可以对前面的配置进行显式 override。相对 `--config` 路径按当前进程 cwd 读取。

## 5. 来源、信任和可覆盖路径

加载器为每个来源赋予 `SourceKind` 和 `Layer`：

| 来源 | Layer | 当前权限 |
| --- | --- | --- |
| image profile | `Profile` | 可声明 policy、input、provider 和完整环境配置 |
| admin profile/config | `Admin` | 受信任；可调整管理员策略和覆盖 profile |
| user config | `User` | 不可声明 policy/input；不能无理由替换继承值 |
| workspace overlay | `Workspace` | 只能显式覆盖 `workspace_can_override` 允许的路径 |
| runtime input | `Runtime` | 只能设置已声明且 `runtime = true` 的输入或允许的命名空间覆盖 |
| CLI patch | `Cli` | 只能覆盖 `cli_can_override` 允许的路径 |

只有 image/admin 来源可以改变 `[policy]` 或声明 `[inputs]`。不受信任来源还不能新增 provider；它只能修改 profile 已经声明的 provider，并仍须满足覆盖规则。

基础 profile 允许工作区修改 shell 默认值的例子：

```toml
[policy]
workspace_can_override = ["shell.default", "environment.variables.*"]
cli_can_override = ["shell.default", "environment.variables.*"]
```

工作区必须同时使用显式 override：

```toml
[override."environment.variables.CARGO_INCREMENTAL"]
op = "set"
value = "1"
reason = "本仓库的本地调试需要开启增量编译"
```

直接写成下面这样不会静默覆盖：

```toml
[config.environment.variables]
CARGO_INCREMENTAL = "1"
```

如果父层已有不同值，严格合并会报 `DEVENV-E-CONFLICT`；如果该路径不在 workspace allow-list 中，则会报 workspace override 不允许。

## 6. 运行时控制变量

以下变量是 CLI 的配置发现控制项：

| 变量 | 作用 |
| --- | --- |
| `DEVENV_PROFILE` / `DEVENV_PROFILE_ID` | 选择 profile id |
| `DEVENV_PROFILES_DIR` / `DEVENV_PROFILE_DIR` | image profile 目录 |
| `DEVENV_ADMIN_PROFILES_DIR` | admin profile 目录 |
| `DEVENV_DEFAULT_PROFILE` / `DEVENV_DEFAULT_PROFILE_FILE` | 默认 profile 文件路径 |
| `DEVENV_ADMIN_CONFIG` | admin overlay，默认 `/etc/dev-env/config.toml` |
| `DEVENV_WORKSPACE` | workspace root |
| `DEVENV_CWD` | 会话 cwd |
| `DEVENV_CONFIG` | 显式 workspace overlay |
| `DEVENV_TRUST_FILE` | `trust` 命令使用的 trust store 路径 |
| `XDG_CONFIG_HOME` | user config 根目录 |
| `XDG_RUNTIME_DIR` | provider lock 目录的父目录 |

profile 中声明的输入变量，例如 `DEVBOX_AUTO_INIT`，与上述控制项不同，只有在 `[inputs.NAME]` 中声明 `runtime = true` 后才会被解析。未声明的普通环境变量不会改变配置；在 `inherit_process = true` 时，它们只作为 ambient environment 进入最终进程环境。

## 7. `--set`、`--unset` 和 `--shell`

CLI patch 始终最后应用：

```bash
dev-env \
  --set environment.variables.CARGO_INCREMENTAL=1 \
  --unset environment.variables.OLD_FLAG \
  print --format json
```

`--set` 的 value 当前按字符串写入，不会根据目标字段自动转换 TOML 类型；是否允许覆盖由 `cli_can_override` 决定。`--unset` 删除该路径，之后仍需通过最终 schema 校验。

`shell --shell ID` 是本次 session 的 shell 选择，不等价于修改 `config.shell.default`，也不要求 profile 允许 shell default override：

```bash
dev-env shell --shell bash
dev-env shell --shell nu
```

当前 CLI 没有实现设计文档中所设想的 `DEVENV_SHELL` 特殊环境变量；需要临时切换 shell 时使用 `--shell`。

## 8. 配置加载排障

### profile 找不到

```bash
dev-env \
  --profiles-dir /etc/dev-env/profiles.d \
  --default-profile-file /etc/dev-env/default-profile \
  explain --json
```

确认目录存在、文件扩展名为 `.toml`、TOML 内的 `id` 与 `extends` 一致，并确认默认文件只有一个 profile id。

### 继承失败

检查 `extends` 引用的 id 是否存在。加载器会明确报告 missing parent 或 inheritance cycle；文件名顺序不能修复继承环。

### 冲突失败

保留错误中的 path、previous origin 和 incoming origin，在更高层添加：

```toml
[override."path.to.value"]
op = "set"
value = "..."
reason = "为什么这个层有意替换父层值"
```

### provider input 不生效

检查目标 input 是否 `runtime = true`、原始环境变量名是否是 canonical name 或 `export_as`，以及输入值是否符合声明类型。详情见 [DSL 输入](dsl.md#7-inputs-类型化运行时输入)。
