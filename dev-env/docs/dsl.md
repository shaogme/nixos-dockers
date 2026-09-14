# dev-env Environment DSL 参考

本文描述当前 `dev-env-model`、`dev-env-loader` 实际接受的 environment DSL schema v1。它是声明式 TOML，不是 shell 脚本语言；provider 的 argv、条件和输出解析见 [Provider 参考](providers.md)。

同一个 profile 可以同时包含 `[config]` 和 `[bootstrap]`。`dev-env-loader` 会把顶层 `bootstrap` 移除后只解析 environment namespace；`container-init` 则读取 Bootstrap namespace。两套 schema 互不共享执行逻辑，Bootstrap 参考见 [`container-init/docs/dsl.md`](../../container-init/docs/dsl.md)。

## 1. 最小 profile

一个可以被 environment loader 解析的最小 profile：

```toml
schema = 1
id = "local"

[config.workspace]
root = "/workspace"
search = "upward"

[config.shell]
default = "bash"

[config.shells.bash]
command = "/usr/local/libexec/dev-env/real/bash"
kind = "posix"
login_args = ["-l"]
interactive_args = ["-i"]
command_arg = "-c"

[config.environment]
inherit_process = true
configured_value_precedence = "locked"

[config.environment.variables]
EDITOR = "vim"

[config.environment.path]
prepend = ["/workspace/.bin"]
append = ["/usr/local/bin"]
remove = []
```

最终配置必须至少提供 `workspace`、`shell` 和 `shells`，默认 shell 必须在 `shells` 中存在。 profile 通过 `extends` 继承父 profile 后，父层提供的字段也计入最终配置。

## 2. 顶层字段

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `schema` | 正整数 | 当前只支持 `1` |
| `id` | 字符串 | profile 唯一 id；允许 ASCII 字母、数字、`.`、`_`、`-` |
| `extends` | 字符串数组 | 父 profile id；按声明顺序解析，不能循环或引用缺失 profile |
| `policy` | 表 | 合并、override、input 和 workspace 策略 |
| `config` | 表 | environment DSL 的实际配置内容 |
| `override` | 表 | 以路径为 key 的显式操作 |
| `inputs` | 表 | 受信任 profile 声明的运行时输入 |
| `bootstrap` | 表 | 由 container-init 读取，environment loader 忽略 |

除 `bootstrap` 外，profile 顶层字段使用严格的 unknown-field 校验；拼写错误不会被静默忽略。

## 3. Policy

```toml
[policy]
merge = "strict"
workspace_can_override = [
  "shell.default",
  "features.devbox.auto_init",
  "environment.variables.*",
]
cli_can_override = ["shell.default", "environment.variables.*"]
unknown_input = "error"
untrusted_workspace = "prompt"
```

字段如下：

| 字段 | 可选值 | 默认值 | 作用 |
| --- | --- | --- | --- |
| `merge` | `strict`、`prefer-child` | `strict` | strict 要求不同值显式 override；prefer-child 只允许受信任的 profile/admin 层使用 |
| `workspace_can_override` | config path pattern 数组 | `[]` | 工作区可覆盖的路径；末尾 `.*` 表示一级及其后缀 |
| `cli_can_override` | config path pattern 数组 | `[]` | CLI `--set/--unset` 可覆盖的路径 |
| `unknown_input` | `error`、`ignore` | `error` | runtime input 或 namespace input 未知时错误或忽略 |
| `untrusted_workspace` | `prompt`、`deny`、`allow` | `prompt` | 记录 workspace 策略；当前 CLI 不实现交互式 prompt 门禁 |

`policy` 本身只能由 image/admin trusted source 提供或修改。workspace 不能通过修改自己的 policy 来扩大权限。

## 4. Workspace

```toml
[config.workspace]
root = "/workspace"
search = "upward"
```

- `root`：必须是绝对路径；不能包含 `..`、shell 元字符或 NUL。
- `search = "fixed"`：只读取 root 的 workspace config。
- `search = "upward"`：从 root 到当前 cwd 逐级读取 `.dev-env.toml` 和对应 `.dev-env.local.toml`。

CLI 的 `--workspace`/`DEVENV_WORKSPACE` 可以选择实际 workspace，但不会修改 profile 中的 `config.workspace.root`；CLI 会在解析完成后确保 cwd 位于选择的 workspace 之下。

## 5. Shell

```toml
[config.shell]
default = "bash"

[config.shells.bash]
command = "/usr/local/libexec/dev-env/real/bash"
kind = "posix"
login_args = ["-l"]
interactive_args = ["-i"]
command_arg = "-c"
```

### `config.shell`

`default` 是 shell id，不是 executable path。它必须在 `config.shells` 中存在。

### `config.shells.<id>`

| 字段 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `command` | 绝对路径 | 必填 | 真实 shell executable；不能写 shim 自己的路径 |
| `kind` | `posix`、`argv` | 必填 | shell 的分类元数据；当前通用 adapter 都按 argv 构造 |
| `login_args` | 字符串数组 | `[]` | login shell 参数，例如 `-l` |
| `interactive_args` | 字符串数组 | `[]` | interactive shell 参数，例如 `-i` |
| `command_arg` | 字符串或空 | `None` | 执行脚本时的参数，例如 `-c` |

所有参数都是单独 argv 元素，不能包含空值、NUL 或换行。shell adapter 不执行未转义的字符串：`CommandLine` 使用 `env_clear` 和 `envs` 注入环境。

默认 login 行为是：没有额外 shell argv 时依次使用 `login_args + interactive_args`；带有 SSH 常见的 `-c` 等参数时只加 `login_args`，再原样追加输入参数。

## 6. Environment 和 PATH

```toml
[config.environment]
inherit_process = true
configured_value_precedence = "locked"

[config.environment.variables]
NIX_PATH = "nixpkgs=/nix/store/..."
CARGO_NET_GIT_FETCH_WITH_CLI = "true"

[config.environment.path]
prepend = ["/opt/project/bin"]
append = ["/usr/local/bin"]
remove = ["/opt/obsolete/bin"]
```

### Environment variables

变量名必须符合 `[A-Z_][A-Z0-9_]*`，值不能含 NUL。当前 CLI 从 ambient process environment 中只保留同样格式的变量；Docker 常见的 lowercase `container` 等 metadata 不会进入 materialized environment。

`inherit_process = true` 时先复制允许的 ambient environment；`false` 时从空环境开始。随后处理配置变量：

- `locked`：配置变量覆盖同名 ambient 变量；
- `ambient`：如果 ambient 已有同名变量，保留 ambient 值，否则使用配置值。

provider 的 shellenv delta 在上述配置之后应用，因此 provider 可以提供或 unset 变量。

### Conditional variables

需要根据已解析配置决定是否设置变量时，使用与 `variables` 并列的
`conditional_variables`：

```toml
[config.environment.conditional_variables.RUSTC_WRAPPER]
value = "sccache"
when = "features.sccache.enabled && !features.sccache.disabled"
```

`when` 使用本页的受限 condition parser，并在 runtime inputs 应用后求值。条件为真
时设置变量并保留 provenance；条件为假时删除该变量，包括从 ambient environment
继承的同名值。一个变量名不能同时出现在 `variables` 和
`conditional_variables` 中。条件变量在 provider shellenv 之前应用，provider
之后仍可通过显式 `set`/`unset` 改变结果。

Rust profile 可将 Compose 开关映射为两个独立的 feature：

```toml
[config.features.sccache]
enabled = true
disabled = false

[inputs.SCCACHE_DISABLE]
target = "features.sccache.disabled"
type = "bool"
runtime = true

[inputs.ENABLE_SCCACHE]
target = "features.sccache.enabled"
type = "bool"
runtime = true
```

因此 `SCCACHE_DISABLE=1` 和 `ENABLE_SCCACHE=0` 任一出现时，wrapper 条件都为假。
两个变量同时存在时，关闭状态优先。

### PATH

`environment.path` 是结构化操作，不是一个隐式覆盖字符串。计算顺序为：

```text
prepend + inherited PATH + append
```

然后移除 `remove` 中的路径，并按第一次出现保留顺序去重。因此 prepend 优先级高于 inherited 和 append。PATH 项必须是绝对、无 `..`、无 shell 元字符的路径。

如果 provider 的 shellenv 也返回 `PATH`：

- `path_mode = "merge"`（默认）：provider PATH 在现有 PATH 前面，再去重；
- `path_mode = "replace"`：provider PATH 完全替换当前 PATH。

## 7. Features

`features` 是一个不由核心解释具体名称的动态值树，可包含布尔、整数、浮点、字符串、数组和嵌套表：

```toml
[config.features.mise]
enabled = true
install_policy = "on-demand"

[config.features.devbox]
enabled = true
auto_init = "never"
```

`features.<provider-id>.enabled = false` 是 core 当前识别的通用开关：provider 会被跳过并产生 `ProviderDisabled` diagnostic。除这个约定外，`features` 的业务含义由 provider 条件和 profile 自己定义；Rust 核心不会写死 `mise` 或 `devbox` 分支。

## 8. Provider 字段概览

```toml
[config.providers.mise]
executable = "/bin/mise"
missing = "warn"
depends_on = []
sensitivity = "public"

[[config.providers.mise.prepare]]
argv = ["install"]
when = "workspace.config-present"
failure = "warn"
timeout_ms = 300000

[config.providers.mise.shellenv]
argv = ["env", "-s", "{shell}"]
format = "shell"
failure = "error"
path_mode = "merge"
timeout_ms = 300000
```

| 字段 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `executable` | 路径或命令名 | 必填 | 通过 PATH 或绝对路径定位；不能含空格或 shell 元字符 |
| `detect_files` | 相对 glob 数组 | `[]` | 配置了该项但没有匹配时 provider not applicable |
| `missing` | `error`、`warn`、`ignore` | `error` | provider 适用但 executable 缺失时的策略 |
| `depends_on` | provider id 数组 | `[]` | 依赖先执行；不能有环或重复 id |
| `prepare` | `PrepareStep` 数组 | `[]` | 可重复的初始化、安装或信任步骤 |
| `shellenv` | `ShellEnvConfig` 或空 | `None` | 读取 provider 输出的环境增量 |
| `sensitivity` | `public`、`sensitive`、`secret` | `public` | provider 输出环境值的默认敏感级别 |

provider 的完整生命周期和输出格式见 [Provider 参考](providers.md)。

## 9. Inputs：类型化运行时输入

```toml
[inputs."DEVBOX_AUTO_INIT"]
target = "features.devbox.auto_init"
type = "enum"
values = ["never", "if-missing", "ask"]
aliases = { "0" = "never", "1" = "if-missing", "true" = "if-missing" }
runtime = true
export_as = "DEVBOX_AUTO_INIT"
default = "never"
sensitivity = "public"
```

| 字段 | 是否必填 | 说明 |
| --- | --- | --- |
| `target` | 是 | 具体 config path；不能含 `*` |
| `type` | 是 | `bool`、`enum`、`integer`、`path`、`string` |
| `values` | enum 必填 | enum 的规范值列表 |
| `aliases` | 否 | enum 原始值到规范值的映射 |
| `runtime` | 否 | `true` 才读取 process environment；默认 `false` |
| `export_as` | 否 | 另一个可接受的环境变量名；如果需要不同语义的兼容变量，声明独立 input |
| `default` | 否 | 必须与声明的 type 匹配 |
| `sensitivity` | 否 | input 写入 provenance 时的敏感级别 |

类型转换规则：

| 类型 | 接受值 |
| --- | --- |
| `bool` | `1`、`true`、`TRUE`、`True`、`0`、`false`、`FALSE`、`False` |
| `enum` | `values` 中的值或 `aliases` 中的 key |
| `integer` | 十进制 `i64` |
| `path` | 安全绝对路径，不接受 `${...}` |
| `string` | 任意不含 NUL 的字符串 |

canonical input name 和 `export_as` 如果同时在进程环境中出现：值相同则视为同一输入，值不同时报 `RuntimeInputConflict`。非法值不会静默回退默认值。

只有 image/admin trusted source 可以声明或改变 inputs。runtime loader 只读取 `runtime = true` 的 input；未声明的普通环境变量不会自动成为配置项。

## 10. Namespace runtime override

`DEVENV_OVERRIDE__` 用双下划线表达配置路径：

```bash
DEVENV_OVERRIDE__FEATURES__DEVBOX__AUTO_INIT=if-missing \
  dev-env shell
```

它会转换为：

```text
features.devbox.auto_init
```

路径片段只能包含 ASCII 字母、数字和 `_`，转换后统一为小写。目标路径必须在 workspace policy 允许范围内；如果该路径有对应的 typed input，会使用 input parser 校验，否则使用字符串值。未知 input 的行为由 `policy.unknown_input` 决定。

## 11. Override 语法

```toml
[override."features.devbox.auto_init"]
op = "set"
value = "if-missing"
reason = "该仓库允许首次进入时初始化 Devbox"
```

支持的操作：

| `op` | 使用字段 | 行为 |
| --- | --- | --- |
| `set` | `value` | 替换整个路径；`value` 必填，`values` 必须为空 |
| `unset` | 无 | 删除整个路径；不能提供 `value`/`values` |
| `replace` | `values` | 路径必须是数组，替换数组内容 |
| `append` | `values` | 路径必须是数组，在末尾追加 |
| `remove` | `values` | 路径必须是数组，删除相等元素 |

所有 override 都必须有非空 `reason`。`values` 不能为空；`null` 不能写入 TOML merge tree。`environment.path.prepend/append/remove` 的 override values 还必须是合法绝对路径。

profile 自身的 `override` 在该 profile 的普通声明合并时生效；同一文档中如果某 path 有显式 override，普通 `[config]` 对该 path 的声明不会再合并一次。这样可以避免“先写 config、再把同一值追加一次”的歧义。

### 严格合并

默认 `strict` 下：

- 父层没有路径：子层直接增加；
- 值完全相同：幂等声明，允许；
- map：递归合并叶子；
- 不同标量：错误；
- 不同数组：错误，不能靠文件顺序替换；
- provider 以 id 为 map key 合并，同 id 的字段仍遵守上述规则。

冲突包含双方 origin 和修复建议：

```text
DEVENV-E-CONFLICT: environment.variables.CARGO_INCREMENTAL
  parent: ...
  child : ...
  fix  : add [override."environment.variables.CARGO_INCREMENTAL"] ...
```

`prefer-child` 只对 trusted image/admin source 有效；workspace、user、runtime 和 CLI 不能通过它绕过权限边界。

## 12. 条件语法

`prepare.when` 使用受限表达式，不执行 shell：

```toml
when = "workspace.config-present && workspace.writable"
when = "features.devbox.auto_init == 'if-missing'"
when = "!workspace.config-present || context.os == 'linux'"
```

支持：

- `always`、`true`、`false`；
- `!expr`；
- `expr && expr`、`expr || expr`；
- `==`、`!=`；
- 单引号/双引号字符串、数字字面量、布尔值和引用；
- 括号。

可用引用：

| 引用 | 值 |
| --- | --- |
| `provider.enabled` | 当前 provider 是否启用 |
| `workspace.config-present` | 探测到 workspace 配置文件或匹配文件 |
| `workspace.writable` | workspace 是否可写 |
| `workspace.root` | workspace 绝对路径 |
| `config.<path>` / `features.<path>` | 已解析配置值 |
| `input.NAME` / `env.NAME` | materialized provider environment 中的变量 |
| `context.cwd` | 当前 cwd |
| `context.os` / `context.arch` | Rust 运行平台信息 |

未定义引用在求值时为 false/不相等。以下内容会被解析器拒绝：命令替换 `$()`、反引号、分号、管道、重定向、shell 函数和其他 shell syntax。

## 13. 安全约束速查

- 所有 environment name 只能是大写 shell 名称；
- 所有 provider 命令使用 argv 数组；
- shell、path、glob 拒绝 NUL、换行、`..` 穿越和 shell 元字符；
- provider shellenv 不经过 `eval`；
- shellenv JSON 只接受字符串或 null，null 表示 unset；
- sensitive/secret 值在 print/explain 中默认脱敏；
- untrusted overlay 不能声明 policy、input 或未声明 provider；
- shell shim 的 real executable 必须是绝对路径，不能与 shim 自身递归。
