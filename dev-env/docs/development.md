# dev-env 开发与测试

`dev-env` 与 `container-init` 共用 `nixos-dockers` 根目录下的 Rust workspace。`dev-env` 负责配置加载、环境物化、provider 编排和 shell 入口；`container-init` 负责容器启动阶段的用户、目录、网络和初始化脚本。修改其中一侧时，应先确认问题属于哪一个边界，并用 `-p <package>` 选择单个 crate。

## 1. Crate 分层

| Crate | 职责 |
| --- | --- |
| `dev-env-model` | 配置模型、DSL 类型、输入值和验证错误 |
| `dev-env-loader` | TOML 解析、profile 继承、overlay 合并、运行时输入覆盖 |
| `dev-env-core` | 运行时上下文、初始环境和 provider 执行编排 |
| `dev-env-provider` | provider 检测、依赖排序、worker 进程监督、输出解析和 receipt；同时构建 `dev-env-provider-worker` |
| `dev-env-shell` | shell argv 构造、环境导出、shim 行为 |
| `dev-env-cli` | 命令行入口、配置发现、进程边界、诊断和 trust 命令 |
| `container-init-protocol` | container-init v2 身份握手的 DTO 与 client，供 `dev-env-cli` 调用 |

workspace 入口见 [`Cargo.toml`](../../Cargo.toml)，源码位于 `nixos-dockers/crates/<package>/`。

`dev-env-cli::identity_broker` 是协议适配层：它把 dev-env 的身份请求映射为
`container-init-protocol` 请求，再将协议 DTO 转成 `EffectiveIdentity`。v2 envelope、版本
检查和 Unix frame 处理只由共享协议 crate 实现。

## 2. 本地构建和静态检查

CI 等价验证从仓库根目录由共享 QEMU launcher 发起：

```bash
bash scripts/qemu-test.sh workspace
```

guest 使用 Rust 1.88.0，执行 Clippy、worker build、workspace tests 和
`cargo cop --all-targets --all-features --workspace`。
workspace test harness 单线程运行，因为 backend startup fixtures 共用固定 abstract endpoint。
各 crate 的 Docker 集成目标 `tests/docker.rs` 使用 `docker-tests` feature，默认不编译。
直接 Cargo 命令适合快速迭代，但不能替代 QEMU guest 验证。

如果修改了 Nix 模块或镜像集成，还应从 `nixos-dockers` 目录做最小评估：

```bash
nix-instantiate --eval --strict images/rust/image.nix -A rust.imageVersion
```

需要完整构建镜像时，使用项目现有脚本 [`tests/docker-image.sh`](../../tests/docker-image.sh)，不要在文档示例中假定某个未声明的本地 Docker tag。

## 3. 测试组织

单元测试主要和被测模块放在同一个 crate 中。跨进程、shell 和 Docker 行为由 guest 中的
各 crate `tests/docker.sh` harness 驱动：脚本构建测试 Dockerfile，在容器内编译并执行对应测试。

各 Dockerfile 使用 `--features docker-tests` 显式编译 Docker 集成目标。通过 target launcher
运行对应 rootful fixture：

```bash
bash scripts/qemu-test.sh crates --runtime rootful-docker
bash scripts/qemu-test.sh crates --runtime rootless-podman
```

每种 runtime 都在一台 guest 中顺序执行 container-init 与 dev-env 的全部七个 crate Docker
suites。仓库提供四个 guest-only dev-env Docker harness：

```text
crates/dev-env-core/tests/docker.sh
crates/dev-env-provider/tests/docker.sh
crates/dev-env-shell/tests/docker.sh
crates/dev-env-cli/tests/docker.sh
```

这些 harness 只由固定 suite/runtime allowlist 的 guest dispatcher 调用；宿主机直接运行会拒绝执行。

CLI backend 从 `dev-env` 所在目录启动 `dev-env-provider-worker`；workspace suite 和 core/provider/CLI
Docker harness 会先构建该 helper。

这类测试覆盖配置物化、provider 输出传递、shell 环境格式化和 CLI 进程边界。涉及 provider
超时、脱组后代清理、检测文件、真实 shell 或 backend transport 时，应运行对应的 QEMU target。

## 4. 修改 DSL 的检查清单

修改 `dev-env-model` 或 `dev-env-loader` 时，建议按以下顺序检查：

1. 在 model 中定义字段、默认值、验证规则和敏感性语义。
2. 在 loader 中接入解析、继承、overlay 或运行时覆盖；确认 strict 合并冲突不会被静默吞掉。
3. 更新 `docs/dsl.md` 中的字段表、示例和当前限制。
4. 为合法值、非法值、冲突和来源追踪补充单元测试。
5. 如果字段会影响 provider 或 shell，补充相应 Docker harness 测试。
6. 运行 rootful Docker 与 rootless Podman 两个 crates guest suite；workspace 级 Clippy 和 Cargo tests 由 `scripts/qemu-test.sh workspace` 执行。

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
| 基础 runtime profile | [`modules/core/runtime.nix`](../../modules/core/runtime.nix) |
| Rust 派生镜像入口 | [`images/rust/image.nix`](../../images/rust/image.nix) |

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
bash scripts/qemu-test.sh crates --runtime rootful-docker
bash scripts/qemu-test.sh crates --runtime rootless-podman
bash scripts/qemu-test.sh workspace
cargo cop --all-targets --all-features --workspace
```

若改动涉及 Nix 模块、磁盘、内核参数、服务状态或镜像运行时逻辑，再按仓库测试指南执行 `evalConfig`、相关 `nixosTest` 和 toplevel 构建。纯文档改动不需要伪造 VM 测试结果，但应检查 Markdown 链接和代码示例的路径。
