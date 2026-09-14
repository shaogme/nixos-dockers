# 安全模型

`container-init` 的安全目标是把“容器初始化”限制为可审计的结构化操作。它不是通用脚本 runner，也不是让 workspace 获得 root 权限的入口。安全校验分为 profile 加载、纯模型校验和执行期文件/权限检查三层。

## 1. 信任边界

| 来源 | 当前行为 |
| --- | --- |
| image profile | trusted；可以声明完整 bootstrap 配置和 root action |
| admin profile | trusted；可以增加限制或用有理由的 override 修改 trusted 配置 |
| workspace overlay | untrusted；只有上层显式加载、父 profile 允许且 action 属于安全集合时可用 |
| user overlay | 不允许进入 bootstrap projection |
| CLI/environment input | 仅能填充 profile 已声明的 typed input，不能添加 action 或修改 policy |

当前 CLI 只从 image profile 目录和可选的 admin profile 目录加载文件，所以普通 CLI 使用不会自动信任 workspace 文件。loader/model 的 workspace overlay 支持用于未来或上层集成。

不受信任来源的限制：

- 只能贡献 `bootstrap.actions`，不能贡献 schema、identity、handoff、policy、inputs 或 override；
- 必须通过父配置的 `allow_workspace_overlay = true`；
- action kind 必须在 `workspace_safe_action_kinds` 中；
- `run_as` 必须是 `target`；
- 不能覆盖继承来的 action；
- 不能执行账户映射、设置登录 shell、降权、SSH 准备或 handoff 等 trusted action。

## 2. 配置校验

profile 在产生副作用前会经过以下检查：

- TOML 类型和 `deny_unknown_fields` action 字段检查；
- profile id、extends、输入名和 action id 的字符集检查；
- schema 必须是 `1`；
- action id 唯一；
- kind 的必填字段存在，且字段类型匹配；
- mode 是最多四位的八进制字符串；
- owner 只能是 `root`、`identity.target` 或数字 `uid:gid`；
- executable 和 login shell 是无空白的绝对路径；
- 输入 target 与 input type 一一匹配；
- identity 引用的输入已经声明；
- action 依赖存在、无自依赖和循环，不依赖更晚的计划阶段；
- identity 引用有唯一的 `identity.resolve` action；
- `identity.resolve`、`process.drop_privileges` 和 `handoff.exec` 的数量限制满足；
- SSH key type 是支持的 `rsa`、`ed25519` 或 `ecdsa`，且不重复；
- `authorized_keys_source` 与 `content` 不同时出现。

继承冲突不会因为“子 profile 后加载”而悄悄覆盖。不同值必须有显式 override 和非空 reason；冲突诊断携带 path、两个 profile、来源和建议修复方式。

## 3. Shell 注入和 argv

以下内容在路径、可执行文件和 argv 值中会被拒绝或限制：

- NUL、换行和回车；
- `$(...)`、反引号、分号、管道、重定向等 shell 表达式；
- 路径中的 `..` 穿越；
- 非绝对路径的 executable；
- executable 中的空白；
- 未知的 `${...}` 插值。

handoff 使用 `std::process::Command` 的 program + args，并通过 `exec` 替换当前进程。container-init 不调用 `sh -c` 来解释 profile 字段，也不接受 action 中的任意命令字段。

profile 仍然可以有意把 `bootstrap.handoff.runtime` 配成 `/bin/sh`，并用 `exec_prefix = ["-c"]`；这代表管理员明确选择 shell 作为最终 runtime，不是 container-init 自己绕过安全检查执行 shell。

## 4. 文件和路径安全

执行前，文件 action 会对渲染后的绝对路径执行安全检查：

- 已存在的路径组件默认不能是意外 symbolic link；
- `ensure_symlink` 允许最终 link 节点是 symlink，但会检查它的当前 target；
- link 已存在且 target 不同会失败；
- link 位置是真实文件、真实目录或已识别 mount point 时不会删除或替换；
- `ensure_dir` 遇到非目录路径失败；
- `ensure_file` 遇到非普通文件或内容不一致失败；
- 默认不删除文件、不清空目录、不递归 workspace；
- `chown`/`chmod` 的 wildcard 会被拒绝；
- 递归 `chown`/`chmod` 不能指向 `/`，且遍历不跟随 symbolic link；
- link 的 owner 使用 `lchown`，不会改写 link target 的属主。

文件创建采用临时文件、写入、`fsync`、rename；passwd/group 也使用原子替换。SSH host key 使用更严格的成对检查和 non-overwriting hard-link 安装。

这些检查是当前实现的路径级保护，不应被理解为完整的内核级 sandbox 或 dirfd 级 TOCTOU 防护。对于可被不可信进程并发改写的目录，应该使用容器 mount、目录权限和外部隔离策略；不要把 workspace overlay 当作 root 配置来源。

## 5. 权限阶段

`run_as` 只描述 action 的执行身份；它不会自动授予权限：

- `root` action 要求有效 UID 为 `0`，并且 root action 的来源必须 trusted；
- `target` action 只允许 root 或已经是目标 UID 的进程执行；
- `current` action 使用当前有效身份；
- `process.drop_privileges` 只允许 root，成功后通过 `setgid`/`setuid` 降权且不可恢复；
- `identity.map_user` 和 `process.set_user_shell` 修改的是 POSIX account database，应只在 image/admin profile 声明。
- `HOST_UID`/`HOST_GID` 只有在声明 namespace 后才会转换；无法读取或覆盖 namespace map 时 fail closed，不会把宿主 ID 当作容器 ID。
- workspace 自动属主映射只接受 mountinfo 证明的挂载点；普通 rootfs 目录、不可读 mountinfo 和未知状态不会产生 root 身份。

不要只把 `owner = "identity.target"` 当成权限边界。真正的写入权限还取决于 action 的 `run_as`、当前进程的有效凭据、父目录权限和 mount 状态。

## 6. SSH capability

`service.ssh.prepare` 是可选 capability，不声明该 action 时不会启用 keygen，也不会创建 SSH 目录。

执行细节：

- host key、authorized keys 目录和 runtime 目录均由 action 声明；
- 三个目录按 root/`0755` 创建或 reconcile；
- 默认生成 RSA 与 Ed25519，或只生成 profile 指定的 key type；
- host key 私钥/公钥必须成对存在；半成品、symlink 和非普通文件直接失败；
- 已有 key 不覆盖，只修正私钥 `0600`、公钥 `0644` 和 root 属主；
- 缺失 key 先在同目录临时路径生成，再用 hard-link 安装，避免覆盖并发出现的 key；
- `authorized_keys_source` 必须是普通文件，不能是 symlink；不存在时不安装空文件；
- 固定 `content` 和 source 互斥；已有 authorized keys 内容不同会拒绝覆盖；
- keygen 用结构化参数调用，不经过 shell；
- action 不启动 sshd，不监听端口，也不自动设置服务编排。

SSH private key 不会进入 plan effect 或 receipt。仍应将 host key 目录和 authorized keys source 作为敏感挂载处理，并限制容器内其他进程对其父目录的写权限。

## 7. 输入和敏感值

只有 `bootstrap.inputs` 中声明的名称会被作为 typed input 使用。这样可以避免把整个环境意外映射成 root 行为。输入值仍可能通过路径插值进入 action，所以：

- `path` input 必须是具体绝对路径，不能包含插值；
- HOME input 默认必须在 `workspace_root` 下；离开 workspace 必须显式 `allow_outside_workspace = true`；
- uid/gid 按无符号十进制解析，bool 只接受限定字面量；
- plan 输出不解析 runtime input，也不展示渲染后的 secret；
- `sensitivity` 是 action 元数据，当前不改变执行策略；
- receipt 不写入 action `content`，但 action 的目标路径、identity 和 error 仍可能是敏感信息，应按 `0600` 文件处理。

条件中的 `env.NAME` 是 ambient environment 的显式读取入口。不要在 environment 中放置 token 后再让 bootstrap action 通过诊断输出它；当前诊断会保留结构化错误和部分路径。

## 8. 错误码

| 退出码 | 分类 | 常见原因 |
| ---: | --- | --- |
| 64 | 参数错误 | command、option、`NAME=VALUE` 格式错误 |
| 65 | 配置错误 | TOML、schema、profile、模型、普通 IO 或输出错误 |
| 66 | 信任错误 | workspace/root/admin-only action、非法 override |
| 67 | 身份错误 | UID/GID/HOME 输入或 POSIX identity 无法解析 |
| 68 | action 错误 | 文件、账户、SSH 或其他 bootstrap action 执行失败 |
| 69 | handoff 错误 | runtime exec 失败 |
| 70 | lock 错误 | lock 被其他 container-init 占用或 lock IO 失败 |

错误文本通常包括 action id、路径、来源和修复提示；JSON 错误会包含 `class`、`kind`、`action`/`path` 等结构化字段。`doctor` 发现 fail 时使用检查对应的错误分类；只有无法进一步分类时才使用 65。

## 9. 运维检查清单

部署前建议确认：

- `plan --json` 的 profile chain 和每一个 root action 来源都是预期的 image/admin profile；
- `doctor --json` 的 runtime、login shell 和 SSH capability 全部 pass；
- image 中不存在不必要的 `service.ssh.prepare`；
- `allow_outside_workspace` 仅用于明确需要的 HOME，并且路径来自可信输入；
- `filesystem.ensure_symlink` 的 link 不会覆盖 volume mount 或真实配置目录；
- lock 路径位于合适的 runtime/workspace 目录，并能被 root/container 用户创建；
- receipt 目录权限足够严格；
- workspace 不会被当作 bootstrap trusted profile 加载；
- 镜像、admin profile 和 handoff runtime 的变更都有对应测试。
