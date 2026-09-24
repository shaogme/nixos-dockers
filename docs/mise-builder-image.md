# 方案二：独立的 Mise Builder 镜像设计

## 状态

本文档记录 `images/mise` 的 builder/runtime 分离设计及其已落地的代码、Nix 模块和
CI 接口，同时定义可复用于其他需要在 Docker build 阶段执行工具安装的 image 接口。

## 1. 问题和决定

当前运行时镜像把 Bash 兼容入口替换为 dev-env shim：

```text
/bin/bash  -> /usr/bin/dev-env
/usr/bin/bash -> dev-env
```

root 进程通过 shim 进入 `container-init exec`，再由已经运行的 backend 完成身份
reconciliation。这个约束只适用于容器运行期。

外层 Dockerfile 在 build stage 执行 `mise lock` 或 `mise install` 时，`python-build`
等脚本通过 `#!/usr/bin/env bash` 找到上述 shim。Dockerfile 的 `RUN` 不会启动镜像的
Entrypoint，因此 backend 不存在，命令最终收到：

```text
container-init: backend error: backend did not become ready before the timeout
```

本方案决定采用两个职责明确的镜像：

1. `mise-builder`：只用于 Docker build 阶段，保留真实 Bash，不安装
   `container-init`/`dev-env` runtime，不发布 Bash shim。
2. `mise` 和 `vscode-mise`：只用于容器运行期，继续保留当前 backend、Entrypoint、
   UID/GID reconciliation 和 Bash shim。工具缓存由 builder 阶段生成后复制进来。

运行时镜像不再承担构建 mise 工具缓存的职责。使用运行时镜像作为 builder 是破坏性
变更，必须迁移到 `mise-builder`。

## 2. 目标和边界

### 2.1 目标

- 任何 build-stage shell script 都能通过普通 `bash` 或 `#!/usr/bin/env bash` 执行，
  不连接 container-init backend。
- 最终运行时镜像继续保留现有 `/bin/bash` shim 行为，尤其是 `docker exec ... bash`
  的身份 Bootstrap。
- mise 的 lockfile、插件、工具安装目录和共享数据可以从 builder 复制到 runtime
  image，而不会复制构建期 secret。
- amd64 和 arm64 使用各自架构的 builder，禁止把一个架构的可执行缓存复制到另一个
  架构。
- builder 和 runtime 使用相同的 mise 版本标签，避免 lockfile、插件协议和缓存布局
  不一致。
- 生成的 builder image 可以独立测试，不依赖一个正在运行的容器或 backend。

### 2.2 非目标

- 不在运行时 shim 中增加“backend 不可用时自动 fallback 到本地 Bash”。这会绕过
  身份和权限边界，也会把 backend 中断误报成普通 shell。
- 不让 backend 在 Docker build 的多个 `RUN` 之间持久运行。Docker layer 之间不共享
  backend socket 或 singleton flock。
- 不把构建期的完整 `/root/.config`、GitHub token、provider secret 或临时凭据复制
  到 runtime image。
- 不通过修改 `python-build` 单个脚本的 shebang 来解决所有构建脚本的问题。

## 3. 镜像契约

### 3.1 `mise-builder`

builder 必须满足以下契约：

- 包含基础系统工具、真实 `bashInteractive`、`mise` 和执行工具安装所需的动态库。
- `runtime.enable = false`，因此不包含 container-init/dev-env runtime 内容。
- `/bin/bash` 和 `/usr/bin/bash` 解析到真实 Bash，不得指向 `dev-env`。
- 不设置 `/usr/bin/container-init` Entrypoint，不设置运行时 handoff CMD。
- 不执行 Bootstrap action，不创建 backend socket，不读取 runtime `HOST_UID`、
  `RUN_AS_ROOT` 或 `CONTAINER_HOME`。
- 工作目录仍为 `/workspace`，以便外层 Dockerfile 直接复制项目配置。
- builder 自身可以包含空的 `/etc/mise`、`/usr/local/share/mise` 和
  `/data/cache/mise` 目录，但这些目录不是 runtime 权限协议的一部分。

builder image 不是面向开发者启动的完整开发容器。它的公开用途只有 Docker build
阶段和工具缓存生成。

### 3.2 `mise` / `vscode-mise`

runtime image 继续满足现有契约：

- Entrypoint 为 `/usr/bin/container-init run --`。
- `/bin/bash` 和 `/usr/bin/bash` 是 dev-env shim。
- backend 只在容器启动时由 `container-init run` 建立。
- `docker exec` 通过 `container-init exec` 获得目标身份和 `HOME`/`USER`/`LOGNAME`。
- runtime image 不在最终阶段执行 `mise lock` 或 `mise install`。
- runtime image 可以包含从 builder 复制的 `/etc/mise`、`/usr/local/share/mise` 和
  `/data/cache/mise`，但不包含 builder 的 Bash、root 配置或临时密钥。

如果没有派生 Dockerfile 复制工具缓存，基础 runtime image 只保证 `mise` 命令可用，
不保证 `python` 等动态工具已经安装。

## 4. Nix 模块接口

### 4.1 独立的 builder 构建函数

在 `modules/default.nix` 增加独立的构建函数，而不是让调用方手动拼接 builder role：

```nix
buildBuilderImage = { name, modules ? [ ], specialArgs ? { } }:
  (evalContainer {
    inherit specialArgs;
    modules = [
      {
        docker.name = lib.mkDefault "${name}-builder";
        docker.role = "builder";
        services.openssh.enable = false;
      }
    ] ++ modules;
  }).config.docker.build;
```

`docker.role = "builder"` 使 runtime profile 默认关闭，builder 不安装
`container-init`/`dev-env`。所有 image 都通过同一个 `buildImages` API 生成三种 role；
若未来需要带 SSH 的构建环境，应另行设计，不能把 `vscode-*` runtime 语义混入 builder。

### 4.2 image 输出

每个 `images/*/image.nix` 都通过 `buildImages` 输出原始、vscode 和 builder 三个 attr。
以 mise 为例：

```text
mise
vscode-mise
mise-builder
```

builder 与 runtime 复用同一组 mise profile module，只改变 image role：

```nix
let
  miseModules = [
    {
      profiles.mise = {
        enable = true;
        package = miseRepo.mise;
      };
    }
  ];
in

builder.buildImages {
  inherit name;
  modules = miseModules;
}
```

实际实现可选择 `buildBuilderImages` 等价命名，但必须保持以下可见接口：

- `nix-build images/mise/image.nix -A mise-builder`
- `nix-instantiate --eval images/mise/image.nix -A mise-builder.imageVersion`

builder 不新增 `vscode-mise-builder`；构建工具缓存不需要 SSH，减少发布矩阵和攻击面。

### 4.3 version 和标签

`mise-builder.imageVersion` 必须与 `mise.imageVersion`、`vscode-mise.imageVersion`
相同。构建派生镜像时使用同一版本标签或 digest：

```dockerfile
ARG NIXOS_DOCKERS_VERSION
FROM ghcr.io/shaogme/nixos-dockers/mise-builder:${NIXOS_DOCKERS_VERSION} AS mise-tools
FROM ghcr.io/shaogme/nixos-dockers/mise:${NIXOS_DOCKERS_VERSION}
```

不允许 builder 使用 `latest` 而 runtime 使用固定版本。发布系统应在 immutable
version tag 完成后再更新 `latest`，避免构建期间两个 image tag 指向不同 mise 版本。

## 5. 派生 Dockerfile 流程

派生镜像必须把工具安装放在 builder stage，把结果复制到 runtime stage：

```dockerfile
ARG NIXOS_DOCKERS_VERSION

FROM ghcr.io/shaogme/nixos-dockers/mise-builder:${NIXOS_DOCKERS_VERSION} AS mise-tools

COPY .mise.toml /etc/mise/mise.toml
COPY conf.d/ /etc/mise/conf.d/

RUN --mount=type=secret,id=GITHUB_TOKEN,required=false \
    if [ -f /run/secrets/GITHUB_TOKEN ]; then export GITHUB_TOKEN="$(cat /run/secrets/GITHUB_TOKEN)"; fi; \
    set -eu; \
    mise trust --all; \
    mise lock --global --platform linux-x64,linux-arm64; \
    mise install; \
    chmod -R 1777 /etc/mise /usr/local/share/mise /data/cache/mise; \
    chmod -R a+rwX /etc/mise /data/cache/mise

FROM ghcr.io/shaogme/nixos-dockers/mise:${NIXOS_DOCKERS_VERSION}

COPY --from=mise-tools /etc/mise /etc/mise
COPY --from=mise-tools /usr/local/share/mise /usr/local/share/mise
COPY --from=mise-tools /data/cache/mise /data/cache/mise
```

### 5.1 构建阶段规则

- `FROM mise-builder` 必须保持目标架构。多架构 BuildKit 构建不得使用
  `--platform=$BUILDPLATFORM` 来运行目标架构的 mise 插件。
- `mise lock` 可以继续生成包含 `linux-x64` 和 `linux-arm64` 的锁文件，但每个
  builder 只安装当前目标架构可执行的工具缓存。
- `GITHUB_TOKEN` 只挂载在 builder 的 `RUN` 指令中，不能通过 `ENV`、`ARG` 或
  `COPY` 进入最终 stage。
- 构建脚本必须通过绝对路径或 builder 的普通 `bash` 运行；不应在 runtime stage
  重新执行会调用 Bash shim 的工具安装动作。
- 目录复制使用明确的 allowlist。默认只允许 `/etc/mise`、
  `/usr/local/share/mise`、`/data/cache/mise`；不得复制 `/root`、`/tmp`、整个
  `/usr/local` 或 Nix profile。
- 如果某个 mise plugin 把 secret 写入上述目录，构建必须失败，而不是继续复制。
  plugin 的 secret 输出位置需要在 profile 或 Dockerfile 中显式排除。

### 5.2 runtime stage 规则

- runtime stage 只做文件复制、权限整理和应用自身文件安装。
- runtime stage 不允许再执行 `mise trust`、`mise lock`、`mise install` 或任何会
  通过 `#!/usr/bin/env bash` 启动脚本的工具解析命令。
- runtime image 的 Entrypoint 和 CMD 继续由 Nix image metadata 提供；派生 Dockerfile
  不应复制 builder 的 config metadata。
- 复制后的 cache 必须对默认 `dev` UID、root 和 runtime 输入选择的 UID 可读写，且
  不改变 `/run/dev-env/locks` 的 owner/mode 约定。

## 6. CI 和发布改动

### 6.1 Nix artifact

发布工作流把每个 `<name>-builder` 作为独立 artifact 构建和上传，并与对应的原始、
vscode image 使用同一版本标签。

建议 image 定义导出 metadata：

```text
image role       published name
runtime          <name>
runtime-ssh      vscode-<name>
builder          <name>-builder
```

builder 必须先在目标架构上传；任何基于它的外部 BuildKit workflow 等待对应版本的
builder manifest 可用后再构建派生 image。

### 6.2 多架构顺序

每个 image、每个架构独立完成以下流程：

1. 构建并推送 `<name>-builder:<version>-<arch>`。
2. 构建并推送 `<name>:<version>-<arch>` 和 `vscode-<name>:<version>-<arch>`。
3. 所有架构完成后创建各 image 的 manifest list。
4. 最后更新 `latest`。

每个 builder 也要创建 amd64/arm64 manifest list，供 `FROM <name>-builder:<version>` 自动
选择正确架构。若 builder 只服务于同一条 workflow，也可以使用架构后缀 digest，
但 Dockerfile 必须显式固定该 digest。

### 6.3 缓存

- BuildKit cache mount 可以加速 builder 的下载，但 cache mount 不是最终 artifact；
  必须依靠显式 `COPY --from=mise-tools` 进入 runtime image。
- registry build cache 只能缓存 builder stage 和 runtime stage 的层，不能缓存或
  暴露 `GITHUB_TOKEN`。
- builder image 本身不应预装项目特定 lockfile，项目配置和 lockfile 属于派生
  Dockerfile 的 builder stage。

## 7. 破坏性迁移

实施后立即切换以下规则：

1. `FROM <name>` 或 `FROM vscode-<name>` 的 Dockerfile 不再允许执行工具安装命令。
   需要构建工具时必须改为多阶段 Dockerfile，并从 `mise-builder` 开始。
2. 不再保证 runtime 基础 image 自带动态解析出的 Python、Node 或其他 mise 工具；
   只有复制 builder artifact 的派生 image 才保证这些工具存在。
3. 不支持通过设置 `RUN_AS_ROOT`、启动 `container-init run` 或修改 backend timeout
   来修复 build-stage shell。构建阶段必须使用 builder image。
4. 不提供旧 runtime image 作为 builder 的兼容别名，也不提供自动检测后回退。
5. 现有使用 `FROM mise` 并在同一 stage 安装工具的示例、文档和 CI 全部迁移；旧
   示例应直接失败或被删除，避免继续产生 backend timeout。

迁移前后对比：

```text
旧：FROM mise
    RUN mise lock && mise install

新：FROM mise-builder AS mise-tools
    RUN mise lock && mise install
    FROM mise
    COPY --from=mise-tools /etc/mise /etc/mise
    COPY --from=mise-tools /data/cache/mise /data/cache/mise
```

## 8. 测试和验收

### 8.1 Nix 静态验证

- `nix-instantiate --eval --strict images/mise/image.nix -A mise-builder.imageVersion`
  成功，并与 runtime image version 相同。
- builder、runtime、vscode runtime 三个 attr 都能独立构建。
- builder 的 config metadata 没有 Entrypoint、container-init 或 SSH CMD。
- runtime 的 config metadata 仍为 `/usr/bin/container-init run --`，vscode runtime
  仍暴露 SSH CMD。

### 8.2 builder Docker 验证

加载 `mise-builder` 后至少验证：

- `readlink /bin/bash` 和 `readlink /usr/bin/bash` 不指向 `dev-env`。
- `/bin/bash -c` 成功执行。
- 一个首行是 `#!/usr/bin/env bash` 的脚本成功执行，且不会输出
  `backend did not become ready`。
- `mise --version` 成功。
- `container-init`、`dev-env` 不存在或不能被 builder 契约误用。

### 8.3 派生 image 集成验证

使用真实 BuildKit Dockerfile 构建一个临时 image：

- builder stage 执行 `mise trust`、`mise lock`、`mise install`。
- final stage 只复制 allowlist 目录。
- final stage 中 `mise ls` 或等价命令能找到已安装工具。
- final stage 中执行 `docker run image /bin/bash` 仍走 runtime bootstrap，并正确
  返回 UID/GID、HOME、USER、LOGNAME。
- `docker exec ... bash` 和 `/bin/bash -lc` 仍使用 backend，而不是 builder 的真实
  Bash。
- 构建日志和最终 image 中不存在 `GITHUB_TOKEN`。

### 8.4 多架构验证

- amd64 builder 在 amd64 运行时执行 `python-build --definitions` 成功。
- arm64 builder 在 arm64 运行时执行同一命令成功。
- 两个架构的最终 image 都只执行本架构的工具 binary。
- lockfile 可以同时声明两个平台；任一架构缺失时构建必须明确失败，不能复制另一
  架构的 cache 冒充成功。

### 8.5 回归验证

保留现有 `tests/docker-image.sh mise` 的运行时测试，并增加以下负向检查：

- 在 runtime image 直接执行一个依赖 `#!/usr/bin/env bash` 的 build script，明确
  报告需要 builder stage，而不是静默执行本地 Bash。
- 在 builder image 查询 backend socket、runtime profile 和身份输入，确认 builder
  不依赖这些运行时对象。

## 9. 安全和失败语义

- builder 不提供 container-init backend，不具备 runtime 身份 reconciliation；它只
  以 Docker build 当前 UID 执行工具安装。
- runtime image 仍然禁止 `exec` 在 backend 不可用时回退到本地 profile 或本地执行。
- secret 的生命周期只存在于 builder `RUN`。任何把 secret 写入 artifact allowlist
  的行为都视为构建错误。
- builder 与 runtime 的版本、架构、lockfile 必须一致；不一致时在构建开始阶段
  失败，而不是在容器启动后才发现工具不可执行。
- 复制目录的权限修正不能降低 runtime socket、`/run/dev-env/locks` 或 profile
  文件的安全 owner/mode。

## 10. 实施顺序

1. 增加通用 `buildBuilderImage` Nix API，通过 builder role 关闭 runtime 和 SSH。
2. 让所有 `images/*/image.nix` 导出 `<name>-builder`，并验证 builder metadata。
3. 增加 builder image 单测和真实 Bash shebang Docker fixture。
4. 增加派生 Dockerfile fixture，验证 builder 安装、artifact copy 和 runtime handoff。
5. 扩展 CI image matrix，发布版本化 builder 多架构 manifest。
6. 迁移所有 runtime image 的 build-stage 用法和文档到多阶段流程。
7. 删除旧的 runtime-as-builder 示例、兼容说明和允许在 runtime stage 执行
   `mise install` 的测试。
8. 运行 Rust、Nix、Docker image 全部验证后，再更新 `latest`。

## 11. 完成条件

只有以下条件全部满足才算完成：

- 每个 `<name>-builder` 可以在没有 backend 的 Docker build 阶段执行工具安装命令，
  Mise builder 具体执行 `mise lock` 和
  `mise install`。
- 运行时原始/vscode image 仍通过 container-init backend 提供原有身份和
  shell 行为。
- 工具缓存只通过明确的 builder-to-runtime artifact copy 进入最终 image。
- amd64、arm64 都有独立 builder 和 runtime 验证证据。
- CI 不再把 runtime image 当作安装 mise 工具的 builder 使用。
- 文档和示例不再保留旧的单阶段构建方式。
- 失败时不会通过 fallback 绕过 backend 权限边界，也不会把构建 secret 带入最终
  image。
