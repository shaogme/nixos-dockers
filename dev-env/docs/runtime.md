# dev-env 运行时与 CLI

本页说明进程边界和命令行为。配置加载顺序见 [配置与部署](configuration.md)，DSL 字段见 [DSL 参考](dsl.md)。

## 1. 一次 materialize

每个需要开发环境的入口都执行相同的基本流程：

```text
选择 profile
  ↓
加载 profile chain 和 overlays
  ↓
解析 runtime inputs / CLI patches
  ↓
创建 RuntimeContext(workspace, cwd, shell, user, ambient env)
  ↓
生成初始 environment 和结构化 PATH
  ↓
应用 conditional environment variables
  ↓
按依赖顺序执行 provider
  ↓
应用 shellenv delta、fingerprint、receipt 和 diagnostics
  ↓
exec 目标命令或输出诊断
```

`explain` 和 `doctor` 只需要加载配置，因此不会执行 provider；`print`、`exec`、`shell`、`login-shell` 和 `shim` 会执行启用的 provider 生命周期。

## 2. 命令总览

```text
dev-env exec [--] <command> [args...]
dev-env shell [--shell <id>] [--login] [--] [shell-args...]
dev-env login-shell [--] [shell-args...]
dev-env shim --shell <id> --real <path> [--] [shell-args...]
dev-env print [--format dotenv|json|shell] [--shell <id>]
dev-env explain [<config.path>]
dev-env doctor [--json]
dev-env trust <path|sha256>
dev-env version
```

通用选项可以放在 command 前，也可在支持它的子命令中使用：

```text
--profile ID
--profiles-dir PATH
--admin-profiles-dir PATH
--default-profile-file PATH
--workspace PATH
--cwd PATH
--config PATH
--set PATH=VALUE
--unset PATH
--user-id UID
```

### `exec`

`exec` 用最终 materialized environment 执行非 shell 命令：

```bash
dev-env exec -- cargo test --all
dev-env exec -- env
```

`--` 用于明确结束 `dev-env` 参数解析；没有 command 是参数错误。Unix 上 CLI 使用 `CommandExt::exec` 替换当前进程，所以目标程序的正常退出码直接成为 `dev-env` 的退出码。

### `shell`

```bash
dev-env shell
dev-env shell --shell bash
dev-env shell --login
dev-env shell --shell bash -- -c 'printf "%s\\n" "$PATH"'
```

不指定 `--shell` 时使用 `config.shell.default`。`--login` 会走 `login_args`；无额外参数时还会加入 `interactive_args`。参数在 `--` 后原样转发。

### `login-shell`

这是给 SSH passwd shell launcher 使用的入口，也可以手动调用：

```bash
dev-env login-shell -- -c 'printf "%s\\n" "$HOME"'
/usr/bin/dev-env-login-shell -c 'printf "%s\\n" "$PATH"'
```

argv0 为 `dev-env-login-shell` 时，CLI 会自动切换到 login-shell 模式。`-c`、`-l`、`-i` 等参数属于真实 shell argv，不会被 dev-env 当作自己的未知选项吞掉。

### `shim`

shim 用于兼容 Docker/IDE 直接调用 shell 的场景：

```bash
dev-env shim \
  --shell bash \
  --real /usr/local/libexec/dev-env/real/bash \
  -- -lc 'echo "$PATH"'
```

非 root shim 只负责：物化环境 → 使用不可递归的绝对 real path → 原样转发 argv。
镜像中的 `/bin/bash` 和 `/usr/bin/bash` 是指向 `dev-env` 的 symlink；CLI 根据
argv0 把它识别为 Bash shim，并使用 `DEVENV_REAL_SHELL` 或默认
`/usr/local/libexec/dev-env/real/bash`。当 shim 以 root 启动且镜像提供
以继承环境的结构化 argv 委托给 `container-init exec -- real-bash ...`。`exec` 连接
容器启动时持有 profile snapshot 的 backend，只执行受限 identity reconciliation；它不
重新读取 profile，也不会触发启动 action 或覆盖启动 receipt。

`--real` 与配置的 shim 路径必须分离。直接将 real shell 配置成 `/bin/bash` 会导致 `/bin/bash` 再回到 dev-env，应该把真实 executable 放在 profile 明确的非 shim 路径。

### `print`

`print` 默认输出 JSON；`--json` 是 JSON 的别名：

```bash
dev-env print
dev-env print --json
dev-env print --format json
dev-env print --format dotenv
dev-env print --format shell --shell bash
```

输出格式：

- `json`：JSON object，所有 value 是字符串；
- `dotenv`：`NAME='value'` 行；
- `shell`：`export NAME='value'` 行。

变量按稳定顺序输出。非 public 的值默认显示为 `<redacted>`；`print --show-secrets` 才输出实际值。脱敏只影响打印，不影响注入给目标子进程的环境。

### `explain`

```bash
dev-env explain
dev-env explain environment.variables.PATH
dev-env explain --json features.devbox.auto_init
```

报告包含：profile id、profile chain、workspace、cwd、merge policy、config fingerprint、可选路径值和 provenance。指定路径不存在时失败。敏感路径值显示 `<redacted>`；当前实现的 explain 以最终成功配置为主，不会启动 provider，也不会列出所有被拒绝的候选 input。

### `doctor`

```bash
dev-env doctor
dev-env doctor --json
```

当前检查：

- workspace 是否存在、是否为目录、是否可写；
- 每个配置 shell 的真实 executable 是否存在且可执行；
- 没有 `detect_files` 的 provider 是否能找到 executable；
- 带 `detect_files` 的 provider 会标为“适用性依赖 workspace 文件”的 warning，不执行实际 provider。

`provider.missing = error` 且 executable 找不到时 doctor 为 fail；`warn` 为 warning；`ignore` 不影响成功状态。存在 fail 时退出码为 65。

### `trust`

```bash
dev-env trust .dev-env.toml
dev-env trust 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

文件参数会计算 SHA-256；64 位十六进制参数直接作为哈希。哈希会追加到：

1. `DEVENV_TRUST_FILE`；
2. `${XDG_DATA_HOME}/dev-env/trusted-hashes`；
3. `$HOME/.local/share/dev-env/trusted-hashes`；
4. `.dev-env-trusted-hashes`。

重复 hash 不重复写入。当前 CLI loader 尚未读取该 store 进行自动准入判断；该命令提供的是哈希登记能力，不等于运行时自动信任 workspace provider。

## 3. 环境注入边界

`MaterializedEnv` 生成后，shell 和 command 都通过 `CommandLine::command_with_environment`：

```text
env_clear()
envs(MaterializedEnv.values)
current_dir(loaded.cwd)
exec(program, original argv)
```

因此：

- `inherit_process = false` 时，宿主环境不会意外泄漏；
- 当前 CLI 只把符合大写环境变量命名规则的 ambient 变量纳入上下文；
- 声明的 runtime input 在配置层更新 `ResolvedConfig`，不是普通 ambient 覆盖；
- conditional variable 条件为假时会从最终环境删除同名 ambient/configured 值；
- provider 继承的是 materialized environment，不是 CLI 进程完整 env；
- command argv 中的 `$()`、分号等只是普通参数，不会重新解析；
- provider shellenv 的 shell 语法只经过受限 parser，不会执行。

## 4. 容器、docker exec 和 SSH

### Docker 默认入口

镜像的 Docker 配置为：

```dockerfile
ENTRYPOINT ["/usr/bin/container-init", "run", "--"]
```

`container-init` backend 先做一次 bootstrap namespace 的 identity/filesystem/SSH action，
然后通过 Unix socket 为后续 handoff 提供同一份 snapshot：

```text
无显式 command: /usr/bin/dev-env shell
有显式 command: /usr/bin/dev-env exec -- <command> <args...>
```

开发环境 provider 在 handoff 后才执行。

Compose 传入的 `CARGO_INCREMENTAL`、`CARGO_TARGET_DIR`、`SCCACHE_DIR`、
`SCCACHE_DISABLE` 和 `ENABLE_SCCACHE` 会随 handoff 保留，并由 Rust profile
声明的 runtime inputs 解析。`SCCACHE_DISABLE=1` 或 `ENABLE_SCCACHE=0` 时，
`RUSTC_WRAPPER` 不会出现在最终交给 Cargo 的环境中。

### `docker exec`

Docker daemon 不会重新运行原 entrypoint，因此推荐：

```bash
docker exec -it <container> dev-env shell
docker exec <container> dev-env exec -- cargo test
docker exec <container> dev-env print --format json
```

如果镜像安装 Bash shim，root 的 `docker exec -it <container> bash -lc ...` 和
`docker exec -it <container> /bin/bash -lc ...` 会先重新进入身份 Bootstrap，再
materialize；非 root 调用则直接 materialize。需要保留 root 身份时显式传入
`RUN_AS_ROOT=1`。直接指定 `/nix/store/.../bash`、
`/bin/sh` 或任意非 shell binary 不会自动注入开发环境；此时请显式写
`dev-env exec --`。

### SSH

SSH 用户的 passwd shell 是 `/usr/bin/dev-env-login-shell`：

```text
sshd → dev-env login-shell -c "..."
     → 重新解析当前用户/workspace/profile
     → materialize
     → 真实 shell -c "..."
```

`/etc/environment` 只应包含构建期静态值。动态 provider 变量由 SSH session 重新产生，因此 SSH、`docker exec` 和默认 handoff 能共享同一 materializer。

## 5. 退出码和错误族

CLI 的常见退出码：

| 退出码 | 类型 |
| --- | --- |
| `64` | CLI 参数错误 |
| `65` | 配置、model、shell、context 或 doctor 失败 |
| `66` | trust/source policy violation 或 trust 操作失败 |
| `70` | provider 运行失败 |
| `74` | 文件或输出 I/O 失败 |
| `127` | 目标 command 无法启动 |

错误会带稳定前缀，例如 `DEVENV-E-CONFIG`、`DEVENV-E-CONFLICT`、`DEVENV-E-PROVIDER`、`DEVENV-E-SHELL`、`DEVENV-E-SHIM`。子命令自己的非零退出码在 Unix `exec` 路径中不被包装为通用错误。

## 6. 常见问题

### `print` 报缺少默认 profile

检查 `/etc/dev-env/default-profile` 或显式指定：

```bash
dev-env --profile nixos-docker print --format json
```

### `shell` 报 default shell 不存在

检查 `config.shell.default` 是否与 `config.shells.<id>` 完全相同，并用 `doctor` 检查真实 executable：

```bash
dev-env doctor --json
```

### `docker exec ... /bin/sh` 看不到 provider 变量

这是预期的低层入口行为。改为：

```bash
docker exec <container> dev-env exec -- /bin/sh -c 'env'
```

### provider 缺失或输出被拒绝

使用 `doctor` 检查 executable，使用 `explain` 检查 profile 和 provider 配置；再查看 [Provider 失败诊断](providers.md#8-provider-失败诊断)。不要通过 `eval` 绕过 shellenv parser。
