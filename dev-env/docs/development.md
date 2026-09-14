# dev-env 开发与测试

`dev-env` 是一个独立的 Rust workspace。它负责配置加载、环境物化、provider 编排和 shell 入口；`container-init` 负责容器启动阶段的用户、目录、网络和初始化脚本。修改其中一侧时，应先确认问题属于哪一个边界。

## 1. Crate 分层

| Crate | 职责 |
| --- | --- |
| `dev-env-model` | 配置模型、DSL 类型、输入值和验证错误 |
| `dev-env-loader` | TOML 解析、profile 继承、overlay 合并、运行时输入覆盖 |
| `dev-env-core` | 运行时上下文、初始环境和 provider 执行编排 |
| `dev-env-provider` | provider 检测、依赖排序、命令执行、输出解析、锁和 receipt |
| `dev-env-shell` | shell argv 构造、环境导出、shim 行为 |
| `dev-env-cli` | 命令行入口、配置发现、进程边界、诊断和 trust 命令 |

源码入口见 [`Cargo.toml`](../Cargo.toml) 及各 crate 的 `src/lib.rs`、`src/main.rs`。

## 2. 本地构建和静态检查

在 `nixos-dockers/dev-env` 目录执行：

```bash
cargo build --locked
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked
cargo build --locked --release -p dev-env-cli
```

如果修改了 Nix 模块或镜像集成，还应从 `nixos-dockers` 目录做最小评估：

```bash
nix-instantiate --eval --strict images/rust/image.nix -A rust.imageVersion
```

需要完整构建镜像时，使用项目现有脚本 [`tests/docker-image.sh`](../../tests/docker-image.sh)，不要在文档示例中假定某个未声明的本地 Docker tag。

## 3. 测试组织

单元测试主要和被测模块放在同一个 crate 中。跨进程、shell 和 Docker 行为由各 crate 的 `tests/docker.sh` 驱动：脚本构建一个测试 Dockerfile，在容器内编译并执行对应测试。

当前仓库提供四个 Docker harness：

```text
crates/dev-env-core/tests/docker.sh
crates/dev-env-provider/tests/docker.sh
crates/dev-env-shell/tests/docker.sh
crates/dev-env-cli/tests/docker.sh
```

默认测试镜像分别为 `dev-env-core-test`、`dev-env-provider-test`、`dev-env-shell-test` 和 `dev-env-cli-test`。如果本地环境已有同等基础镜像，可通过以下变量替换：

```bash
DEV_ENV_CORE_TEST_IMAGE=... ./crates/dev-env-core/tests/docker.sh
DEV_ENV_PROVIDER_TEST_IMAGE=... ./crates/dev-env-provider/tests/docker.sh
DEV_ENV_SHELL_TEST_IMAGE=... ./crates/dev-env-shell/tests/docker.sh
DEV_ENV_CLI_TEST_IMAGE=... ./crates/dev-env-cli/tests/docker.sh
```

运行单个 harness 的完整示例：

```bash
cd nixos-dockers/dev-env
./crates/dev-env-core/tests/docker.sh
./crates/dev-env-provider/tests/docker.sh
./crates/dev-env-shell/tests/docker.sh
./crates/dev-env-cli/tests/docker.sh
```

这类测试覆盖配置物化、provider 输出传递、shell 环境格式化和 CLI 进程边界。涉及 provider 超时、锁、检测文件或真实 shell 时，应优先运行对应的 Docker harness，而不仅仅是宿主机上的单元测试。

## 4. 修改 DSL 的检查清单

修改 `dev-env-model` 或 `dev-env-loader` 时，建议按以下顺序检查：

1. 在 model 中定义字段、默认值、验证规则和敏感性语义。
2. 在 loader 中接入解析、继承、overlay 或运行时覆盖；确认 strict 合并冲突不会被静默吞掉。
3. 更新 `docs/dsl.md` 中的字段表、示例和当前限制。
4. 为合法值、非法值、冲突和来源追踪补充单元测试。
5. 如果字段会影响 provider 或 shell，补充相应 Docker harness 测试。
6. 执行 `cargo fmt`、`cargo test --workspace --locked` 和 `cargo clippy`。

需要注意：配置文件中的 `bootstrap` 当前不是 dev-env 环境 DSL 的一部分；它属于容器启动流程，文档和代码不应把两套 namespace 混为一谈。

## 5. 修改 profile 和镜像集成

修改 `images/*/.config/dev-env.toml` 时：

- 先确认 `extends` 的 profile 在当前镜像内确实存在；
- 检查 provider 的 `depends_on`、`detect_files` 和 workspace 路径是否适用于派生镜像；
- 对会改变 PATH 或环境变量优先级的改动，说明 `prepend`、`append`、`remove` 和 `locked`/`ambient` 的效果；
- 对项目级 provider 使用 `detect_files`，避免在无项目配置的 workspace 中误执行；
- 对敏感输出设置 `sensitivity`，不要把 token、密码或私钥放进普通 README 示例；
- 运行 CLI 的 `print`、`explain`、`doctor`，并确认 provider 实际执行符合预期。

可用的最小镜像验证：

```bash
cd nixos-dockers
nix-instantiate --eval --strict images/rust/image.nix -A rust.imageVersion

# 下面的 image:tag 取实际构建产物或本地已有镜像。
docker run --rm --entrypoint /usr/bin/dev-env image:tag print --json
docker run --rm --entrypoint /usr/bin/dev-env image:tag explain
docker run --rm --entrypoint /usr/bin/dev-env image:tag doctor
docker run --rm --entrypoint /usr/bin/container-init image:tag plan --json
docker run --rm --entrypoint /usr/bin/dev-env-login-shell image:tag -c 'printf "%s\\n" "$PATH"'
```

`explain` 和 `doctor` 不会因为展示环境而执行 provider；`print` 会先物化环境，因此可用来验证 provider 生成的最终变量。需要验证 provider 编排和目标进程边界时，使用 `exec`、`shell` 或真实 shim 入口。

## 6. 关键入口和实现对照

| 关注点 | 主要源码 |
| --- | --- |
| profile、overlay、输入和环境合并 | `crates/dev-env-loader/src` |
| 环境物化和 provider 编排 | `crates/dev-env-core/src` |
| 检测、依赖、执行和输出解析 | `crates/dev-env-provider/src` |
| shell 导出、argv 和 shim | `crates/dev-env-shell/src` |
| 配置发现、命令和进程替换 | `crates/dev-env-cli/src` |
| Nix 安装路径、profile 和 shim 链接 | [`modules/core/runtime.nix`](../../modules/core/runtime.nix) |
| 镜像 profile | [`images/common/.config/dev-env.toml`](../../../images/common/.config/dev-env.toml)、[`images/rust/common/.config/dev-env.toml`](../../../images/rust/common/.config/dev-env.toml) |

## 7. 当前实现边界

文档和测试应以当前代码行为为准，并明确以下边界：

- `trust` 命令目前只写入并读取 hash 文件格式；loader 尚未把 trust 结果接入 workspace 策略决策。
- provider 外部协议的数据类型已经定义，但默认 CLI 当前不会自动发现 `dev-env-provider-*` 可执行文件。
- `untrusted_workspace = prompt` 在非交互路径不会弹出完整的交互式确认流程；CI 应使用明确策略。
- provider 的 prepare receipt 已生成，但通用 runner 当前仍会执行本次会话声明的 prepare 步骤，不能把 receipt 当作已启用缓存的证明。
- profile source 会保留来源路径和来源类型；当前 loader 的通用来源位置未填充 TOML 行列号。

修改这些行为时，必须同时更新 [`providers.md`](providers.md)、[`configuration.md`](configuration.md) 或 [`dsl.md`](dsl.md) 中相应的“当前实现”说明，并增加回归测试。

## 8. 变更提交前

提交前至少确认：

```bash
git diff --check
cargo fmt --all -- --check
cargo test --workspace --locked
```

若改动涉及 Nix 模块、磁盘、内核参数、服务状态或镜像运行时逻辑，再按仓库测试指南执行 `evalConfig`、相关 `nixosTest` 和 toplevel 构建。纯文档改动不需要伪造 VM 测试结果，但应检查 Markdown 链接和代码示例的路径。
