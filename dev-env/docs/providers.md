# dev-env Provider 参考

Provider 是环境 DSL 对外部工具的通用适配层。Rust 核心不判断“这是 mise 还是 Devbox”，只执行 profile 声明的 provider id、executable、argv、条件和输出格式。

## 1. Provider 配置

```toml
[config.providers.tool]
executable = "/usr/local/bin/tool"
detect_files = ["tool.json", "config/**/*.toml"]
missing = "warn"
depends_on = ["base-tool"]
sensitivity = "public"

[[config.providers.tool.prepare]]
argv = ["install", "--workspace", "{workspace}"]
when = "workspace.config-present && workspace.writable"
failure = "warn"
timeout_ms = 120000

[config.providers.tool.shellenv]
argv = ["env", "--shell", "{shell}"]
format = "json"
failure = "error"
path_mode = "merge"
timeout_ms = 30000
```

字段含义：

- `executable`：绝对路径直接检查；简单命令名使用当前 materialized PATH 查找。
- `detect_files`：相对于 workspace 的文件或 glob。配置了该项但没有匹配时，provider 是 `NotApplicable`，不是缺少 executable。
- `missing`：provider 适用但 executable 不存在时使用 `error`、`warn` 或 `ignore`。
- `depends_on`：provider id 的有向依赖图，依赖先执行；缺失依赖和循环都是配置错误。
- `prepare`：有副作用的可重复操作，例如 install、trust 或 init。
- `shellenv`：获取环境增量的可选操作。
- `sensitivity`：generic runner 产生结果时标记输出环境的敏感级别。

`executable`、每个 argv 和 glob 都必须通过 model 校验；不能包含 NUL，executable 不能包含空格、管道、重定向、命令替换或反引号。

## 2. 生命周期

每个启用的 provider 都经过：

```text
文件探测 + executable 定位
          │
          ├─ 不适用/缺失 → diagnostic 或按 missing 失败
          │
          ▼
       prepare[n]
          │
          ▼
       shellenv
          │
          ▼
   EnvironmentDelta 合并
```

core 按 provider dependency order 调用 runner。provider 关闭时（`features.<id>.enabled = false`）不会启动 executable，而是返回 `ProviderDisabled` diagnostic。provider 的 shellenv 会看到基础环境和前序 provider 已产生的变量。

### `prepare`

每一步可以写：

```toml
[[config.providers.mise.prepare]]
argv = ["install"]
when = "workspace.config-present"
failure = "error"
timeout_ms = 300000
sensitivity = "public"
```

`when` 缺省表示执行。条件为 false 时跳过该 step，不算失败。`failure` 的语义是：

| 值 | 行为 |
| --- | --- |
| `error` | 命令失败、超时或无法启动时终止本次 materialize |
| `warn` | 记录结构化 `PrepareFailed` diagnostic，继续后续流程 |
| `ignore` | 记录 diagnostic，继续后续流程 |

只要有 runnable prepare step，就先获取 workspace/user/provider 锁。默认锁目录是 `$XDG_RUNTIME_DIR/dev-env/locks`，没有 `XDG_RUNTIME_DIR` 时使用 `/run/dev-env/locks`；同一 workspace、user id、provider id 的并发 prepare 会串行化。

当前 runner 会在一次调用中每次执行符合条件的 prepare；receipt 会随结果返回，但当前 CLI 不读取 receipt 来跳过下一次 prepare。因而 profile 中的安装/初始化命令应当自身幂等。

### `shellenv`

```toml
[config.providers.tool.shellenv]
argv = ["env", "-s", "{shell}"]
format = "shell"
failure = "error"
path_mode = "merge"
```

shellenv 成功返回后，stdout 必须是声明的格式；stderr 只作为失败诊断，不会被当成环境内容。无法转为 UTF-8、格式错误或包含非法 shell 语法都会按 `failure` 处理。默认 `failure = "error"`。

## 3. argv 模板

模板只做已知数据替换，替换后的结果仍然是单独 argv 元素：

| placeholder | 值 |
| --- | --- |
| `{provider}` | provider id |
| `{workspace}` | workspace 绝对路径 |
| `{cwd}` | 当前 cwd |
| `{shell}` | 当前 shell id |
| `{user-id}` | 当前 user id |
| `{workspace-config-present}` | `true`/`false` |
| `{workspace-writable}` | `true`/`false` |

未知 placeholder、未闭合 `{`、空 placeholder 和 NUL 都会失败。模板值不会再次经过 shell 解析；例如：

```toml
argv = ["run", "literal;$(not-a-command)", "{workspace}"]
```

第二个参数就是带分号和 `$()` 的字面量。

## 4. 环境输出格式

### Shell

只允许以下环境赋值语句；引号内的换行属于值的一部分：

```text
export NAME='value'
NAME="value"
unset NAME
```

变量名必须符合 POSIX 环境变量格式（首字符为字母或 `_`，后续为字母、数字或 `_`）。值支持受限的单引号、双引号和反斜杠转义；Devbox 使用的行尾分号和单独的 `hash -r` 会被忽略。其他未加引号的空白、命令替换、反引号、分号、管道、重定向、函数和 `&` 都会被拒绝。解析器只把赋值转换成 `ShellEnvEntry`，不会调用 `sh -c` 或 `eval`。

### Dotenv

dotenv 允许空行和以 `#` 开头的注释，其他行使用同一套变量赋值解析：

```text
# project tool environment
API_URL='https://example.test'
unset OLD_API
```

行内 `#` 不会自动作为注释；需要简单地把整个值按引号写清楚。

### JSON

根必须是 object，value 只能是字符串或 null：

```json
{
  "RUSTFLAGS": "-C target-cpu=native",
  "TOOL_HOME": "/data/tool",
  "OLD_API": null
}
```

字符串表示 set，null 表示 unset；数字、数组、嵌套 object 和小写变量名都会被拒绝。

## 5. PATH 合并

provider shellenv 产生的 `PATH` 由 `path_mode` 控制：

```text
merge:   provider PATH + 当前 PATH，然后去重
replace: provider PATH 完全取代当前 PATH
```

provider 本身没有 shellenv PATH 时，仍可通过 `[config.environment.path]` 结构化设置 PATH。不要让 provider 返回一个隐式覆盖的整段 PATH，除非明确需要 `path_mode = "replace"`。

## 6. Provider 子进程环境

generic runner 通过 `CommandRequest` 启动 provider，cwd 是当前会话 cwd，environment 是当前 materialized environment 的显式 map。它不会直接把 runner 自身的完整宿主环境复制给 provider。

这意味着：

- `inherit_process = false` 可以阻止 ambient secret 进入 provider；
- provider A 设置的变量会传给依赖它的 provider B；
- provider 只能通过 stdout 声明环境 delta，不能修改父进程；
- provider stderr 不会成为变量；
- stdout 默认最多 1 MiB，stderr 默认最多 64 KiB；
- 默认单命令超时为 300 秒，profile 的 `timeout_ms` 可以缩短或调整。

## 7. Receipt、fingerprint 和锁

`MaterializedEnv` 会带有：

- `config_fingerprint`：序列化后的 `ResolvedConfig` SHA-256；
- provider receipt：provider id、配置 fingerprint、workspace fingerprint、版本和完成时间（当前 generic runner 的 version/time 可能为空）；
- provider 输出的 sensitivity 和 provider id。

workspace fingerprint 使用 canonical workspace 路径及 provider 探测到的文件内容计算，不会把 workspace 中无关文件和完整 secret 写入 receipt。锁 key 则使用 canonical workspace、user id 和 provider id 的 SHA-256。

receipt 当前用于报告结果和测试审计，不是已接入的缓存系统；不要把它当成 prepare 一定只执行一次的保证。

## 8. Provider 失败诊断

常见错误族：

| 错误码 | 含义 |
| --- | --- |
| `DEVENV-E-PROVIDER-MODEL` | provider schema 或模型校验失败 |
| `DEVENV-E-PROVIDER-DETECT` | workspace/executable 探测失败 |
| `DEVENV-E-PROVIDER-MISSING` | 适用的 executable 不存在且 `missing = error` |
| `DEVENV-E-PROVIDER-TEMPLATE` | argv placeholder 错误 |
| `DEVENV-E-PROVIDER-COMMAND` | 启动、读取、等待或终止子进程失败 |
| `DEVENV-E-PROVIDER-EXIT` | provider 返回非零退出码或超时 |
| `DEVENV-E-PROVIDER-UTF8` | shellenv stdout 不是 UTF-8 |
| `DEVENV-E-PROVIDER-OUTPUT` | shellenv 输出格式或安全语法被拒绝 |
| `DEVENV-E-PROVIDER-LOCK` | 锁目录、锁文件或等待超时失败 |
| `DEVENV-E-PROVIDER-FINGERPRINT` | 计算配置/workspace fingerprint 失败 |

`warn`/`ignore` 不会吞掉结构化事实；它们返回 diagnostic，CLI 或调用库可以继续输出环境并报告具体 provider、step、退出码、timeout 和截断 stderr。

## 9. 镜像中的实际 provider

coding-images profile 将外部工具声明成普通 provider：

```text
coding-images
├── mise             install + shellenv
├── devbox-global    global shellenv
├── devbox-project   detect devbox.json + install + shellenv
└── devbox-init      条件 init
```

Rust、QEMU 等派生 profile 通过 `extends` 增加环境变量或 bootstrap action，不需要修改 `dev-env` Rust 核心。完整示例见 [`images/common/.config/dev-env.toml`](../../../images/common/.config/dev-env.toml) 和 [`images/rust/common/.config/dev-env.toml`](../../../images/rust/common/.config/dev-env.toml)。

## 10. 外部 provider 协议的当前边界

`dev-env-provider` 提供了 version 1 的协议类型：

```text
request : {"protocol":1,"op":"detect|prepare|shellenv","context":...,"config":...,"argv":[]}
response: {"ok":true,"env":...,"diagnostics":...,"receipt":...}
```

协议会校验版本、shell 和 argv，并提供错误分类。但当前默认 `ProviderRunner` 只使用 `ProcessExecutor` 运行 `ProviderConfig.executable`；它不会根据 provider id 自动查找 `dev-env-provider-<id>`。如果接入外部 provider，需要在上层实现 `ProviderRuntime` 适配，而不是假设只添加一个可执行文件就会被 CLI 使用。
