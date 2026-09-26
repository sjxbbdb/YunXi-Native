# Miyu Agent 源码全量审计（适配前冻结报告）

> 审计对象：`SHORiN-KiWATA/miyu-agent`
>
> 固定版本：`0.6.2` / commit `04a23ccbfc1ee081ec8e2d82090edfa553552456`
>
> 审计目的：在继续把 Miyu 能力通过适配层接入 YunXi Native 之前，先建立可复核的文件覆盖、依赖边界、运行语义和安全门槛。
>
> 当前结论：**冻结全量拼接；不能以“能编译”或“基础 fish 能跑”作为适配完成。**

## 1. 结论先行

这次审计确认 Miyu 值得借鉴的核心不是若干孤立的工具，而是一套相互耦合的宿主系统：fish hook、一次性与可重连客户端的不同生命周期、Unix socket IPC、daemon 单例锁、事件回放、会话状态和 YunXi/Miyu 自己的 Web 宿主共同组成运行闭环。

因此当前采用以下硬边界：

1. YunXi Runtime 仍是人格、灵魂、记忆、陪伴、工具授权、工作区和隐私的唯一真相源。
2. Miyu 的 fish/daemon 思路只能通过适配层接入；不复制 Miyu 数据库、人格、Web 路由或平台凭证。
3. 在协议、锁、断线语义、会话持久化和 PTY 回归测试通过前，不扩展第二批 Miyu 工具，也不声称 Arch 版与 Miyu 等价。
4. 发现的问题分为“上游行为事实”“YunXi 实验实现风险”“待验证假设”，三者不混写。

这是一份**第一轮全文件覆盖 + 关键路径语义审查**，不是对 32.6 万行 Rust 逐行人工签字的发布许可。文件级覆盖已经完成，关键路径审查已完成，剩余普通功能模块必须在对应迁移批次中逐项做行为测试后才能放行。

## 2. 审计对象与覆盖证据

### 2.1 固定源码版本

- 仓库：`https://github.com/SHORiN-KiWATA/miyu-agent`
- commit：`04a23ccbfc1ee081ec8e2d82090edfa553552456`
- package version：`0.6.2`
- Rust edition：2021；最低 Rust：1.89；许可证：MIT（版权归 SHORiN-KiWATA）

### 2.2 文件扫描统计

扫描器：[`audit/scan-miyu-source.ps1`](audit/scan-miyu-source.ps1)。它通过 `git ls-files` 枚举固定 checkout 的全部受 Git 跟踪文件，读取文本内容，记录路径、分组、物理行数、字节数、资源/进程/网络/数据库/并发/unsafe 信号和跨 crate import；二进制只登记，不把二进制误当源码。

重新生成清单：

```powershell
./yunxi-agent-linux/audit/scan-miyu-source.ps1 `
  -ReferenceRoot .tmp/miyu-agent-reference-20260924
```

清单：[miyu-file-inventory.csv](audit/miyu-file-inventory.csv)。本轮结果：

| 项目 | 数量 |
|---|---:|
| Git 跟踪文件 | 1,717 |
| 已读取内容的文本/脚本/资源 | 1,587 |
| 只做目录登记的二进制或未知格式 | 130 |
| 被识别为源码的文件 | 1,298 |
| Rust 源文件 | 920 |
| Rust 物理行数（含空行、测试） | 326,494 |
| Python 源文件 | 291 |
| Python 物理行数 | 54,938 |
| 无扩展名且纳入源码扫描的脚本 | 19 |

无扩展名脚本集中在 `src/personas/default/scripts/`、旅行规划和 Bilibili 直播 skill；例如 `battery-care` 会写 sysfs，`bilibili_live_stream` 的 `start` 会使直播公开，不能因为它们是“脚本资源”就自动开启。

### 2.3 审查分级

- **A：文件覆盖**：所有 1,717 个 Git 跟踪路径已枚举；文本路径已读取并完成静态信号扫描。
- **B：关键路径语义**：fish hook、shell 安装器、IPC framing/version、daemon 生命周期、事件流、运行断线语义、memory、sandbox、AUR、export/import、工具注册和资源闭包已逐段核查。
- **C：行为验证**：当前只完成源码内已有单元测试与静态门禁的审阅；未在本机真实 fish/PTY、异常断线、恶意归档和 AUR 网络环境中宣称通过。fish 机实测环境当前不具备，后续必须在 Arch runner 上补齐。

固定 checkout 自带的 `test_scripts/arch_dep_check.py --warn` 已执行并以退出码 0 返回；它只检查 Miyu 自己登记的 Rust 模块依赖方向，不能替代编译、PTY、IPC 或安全测试，也不代表 YunXi adapter 已通过同一门禁。

## 3. 架构与依赖图

Miyu 的 workspace 不是“一个 daemon 加几个脚本”，而是：

```text
miyu (CLI/TUI entry)
  ├─ miyu-base   paths / shell / sandbox / config / terminal / embedding
  ├─ miyu-core   IPC / state / memory / LLM / skills / ledger
  ├─ miyu-engine agent / tools / transfer / voice / default KB
  └─ miyu-hosts  daemon / runtime / platforms / render / web
```

关键事实：`miyu-hosts` 的 `lib.rs` 同时公开 `daemon`、`platforms`、`render`、`runtime`、`web`；其 `daemon::run` 先抢家目录单例锁，再直接调用 `crate::web::run`。也就是说，Miyu 的 daemon 当前是 Web/平台宿主的总入口，不是已经独立好的 headless fish 服务。

- [hosts/lib.rs（固定版本）](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-hosts/src/lib.rs#L5-L9)
- [hosts/daemon.rs（固定版本）](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-hosts/src/daemon.rs#L5-L46)
- [hosts/web/server.rs（固定版本）](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-hosts/src/web/server.rs#L10-L82)

因此“只把 `daemon.rs` 拿过来”不是可行的适配方案；需要先定义 YunXi 的 headless host facade，再接入协议、运行时和会话存储。

## 4. 关键路径审查结果

### 4.1 fish 接管：可借鉴，但当前实验实现还没有行为等价

Miyu fish hook 的入口和安装器在 [`miyu-base/src/shell/fish.rs`](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-base/src/shell/fish.rs#L80-L341)。已确认的语义包括：

- 用 `commandline --tokens-raw` 先拿原始首词，尽量在 fish 展开 glob、命令替换之前判断；
- 通过 `type -q` 识别 fish function、alias、builtin 和 PATH 命令，不能只靠 PATH 静态表；
- 普通命令交还 fish，自然语言才进入 daemon；
- 多行输入、Ctrl+J、光标/提示符重绘和 `fish_command_not_found` 是完整交互的一部分；
- command-not-found 兜底必须区分“顶层未知自然语言”与“已执行命令内部返回 127”，不能把后者重复提交；
- hook 生成/卸载有版本标记、内容指纹、非生成文件保护、语法检查、原子写入和 symlink 处理。

当前 YunXi Arch 实验 [`src/shell.rs`](src/shell.rs) 的风险清单：

1. 分类器没有 Miyu 的 `type -q` 语义，用户定义 function/alias 可能被误送给 Agent。
2. fallback 对任意非空当前行转发，尚未具备 Miyu 的顶层/嵌套命令边界保护。
3. 多行复杂语法、提示符重绘、光标恢复、hook 指纹与原子安装尚未按 PTY 行为验收。
4. 现有测试主要检查生成字符串和分类器，不等价于真实 fish 行为测试。

结论：保留 Miyu 的交互模型，重写为 YunXi hook；在真实 fish PTY 矩阵通过前不得扩展 zsh/bash。

### 4.2 IPC：协议细节不能省略

Miyu IPC 的协议版本为 3，使用 4 字节长度前缀 + JSON frame；发送和接收端都拒绝 0 或超过 24 MiB 的 frame。

- [protocol.rs](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-core/src/ipc/protocol.rs#L11-L13)
- [protocol.rs send/receive](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-core/src/ipc/protocol.rs#L549-L568)

命令面不止“发一句文本”：包含 StartTurn、Follow、Cancel、问题回答/关闭、作业、session 操作、ToolCall、catalog、sandbox、voice 和 admin 命令。YunXi 当前 Linux IPC 已有协议版本、Ping、24 MiB frame 上限、Cancel，以及已完成回合的有界 Follow 回放；构建身份协商、活动回合断线续跑和更完整的能力协商仍未进入本阶段。

### 4.3 daemon 与断线：常驻和一次性客户端是两种语义

Miyu 在 [`runtime/run.rs`](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-hosts/src/runtime/run.rs#L177-L223) 明确区分：

- 可重连前端掉线：回合继续由 daemon/actor 持有，客户端可用事件游标回来 Follow；
- 一次性 CLI 或 shell hook（`origin_tty` 存在）：掉线就取消回合、取消挂起问题并按 interrupted 落库，避免永远卡在 running。

IPC server 在 [`web/ipc_server.rs`](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-hosts/src/web/ipc_server.rs#L1111-L1147) 设置 `one_shot`，不是所有客户端都“断线继续”。

当前 YunXi daemon 的剩余问题：future 仍归属于连接，活动回合断开可能丢 turn；完成回合的 run registry、event cursor 和 Follow 仅保存在 daemon 内存中，重启即失效；session map 虽已支持 fish 进程 session id，但仍会在重启时丢失；尚未引入 build identity、setsid 和完整的连接超时策略。

### 4.4 事件流与会话

Miyu 的 EventHub 维护单调递增 event id、broadcast + 有界回放队列，并在游标过旧时返回 resync_required；事件队列有记录数和约 4 MiB 字节目标，但单个超大事件可能暂时超过目标。适配时不能只复制一个 `mpsc` channel：必须保留事件顺序、游标、重同步和尾事件排空语义。

### 4.5 路径、锁和 sandbox

Miyu 使用家目录单例锁、runtime/core/starter 多级 flock，并在 Linux 上结合进程 start time 防 PID 复用；runtime 目录权限为 0700。其路径体系以 `~/.miyu`/`MIYU_HOME` 为中心，并有旧布局迁移。Arch YunXi 应使用 XDG data/state/cache 与 YunXi home，不能静默把用户数据改写成 Miyu 路径。

Miyu 的 Landlock sandbox 是宿主后端，不等于工具授权策略：unsupported kernel 在有 policy 时应 fail-closed；无 policy 时才允许兼容运行。YunXi 必须保留自己的 Approval/Workspace/Sandbox 语义，再决定是否把 Landlock 作为实现后端。

### 4.6 记忆：不能直接替换 YunXi 的模型

Miyu memory 有 SQLite facts/episodes/diary、访问主体/可见性、FTS/分词、向量候选、RRF/余弦、衰减、去重和后台 organizer；测试覆盖访问隔离、敏感边界、reset generation 和语义召回。

YunXi 已有 JSONL 权威台账 + SQLite 向量索引的分层设计：profile/高敏感/agent identity 不进入向量索引，向量只用于候选排序，最终必须回查权威状态。适配可吸收 Miyu 的后台批处理、FTS+semantic 召回和 generation barrier，但不能搬 Miyu schema，也不能绕过 YunXi 的敏感度、审批和失效链。

### 4.7 工具、脚本与资源闭包

Miyu 工具注册由 ToolSpec/permission/trust/lazy/requirements 等元数据和 JSON 描述共同组成；描述文件、prompt、`include_str!` 资源和 persona scripts 是运行时闭包的一部分，不能只复制 Rust 文件。

已发现需要单独隔离/审查的能力：

- `battery-care`：读写 sysfs，部分操作需要 sudo；默认只读，不得自动允许写入。
- `bilibili_live_stream`：`start` 会公开开播并通知关注者，必须走明确用户确认。
- `crack-search`、社交平台搜索、账号/二维码流程：要有权利、凭证、速率和网络失败边界。
- weather/flight/hotel/search 等：外部网络结果带来源、警告和降级字段，不能只取一个数字。

工具迁移方式应是“改写成 YunXi Tool/MCP/Skill + YunXi 权限门”，而不是将 Miyu 脚本目录整体复制到默认启用列表。

### 4.8 AUR/PKGBUILD

Miyu 的 `review_aur_package` 会下载并做启发式审查，`install_aur_package` 要求 `user_confirmed=true`、先有允许的 review，再调用 `paru/yay --noconfirm` 或 makepkg fallback；关键实现见 [`aur_review.rs`](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-engine/src/tools/archlinux/aur_review.rs#L23-L104)。

这不是足够的安全授权：模型传入的布尔字段不能证明人类确认，review 与安装之间也需要绑定 PKGBUILD/快照 digest，防止 TOCTOU；安装需要 YunXi authenticated approval、明确包名和版本摘要。没有这些适配前，不迁移自动安装路径。

### 4.9 export/import

Miyu export 对 SQLite 使用 `VACUUM INTO` 做一致快照，并在 manifest 写入 BLAKE3、版本和 schema；但默认 `no_secrets=false`，导出可包含密钥。import 会拒绝绝对路径和 `..`，先 staging 再逐文件 rename/copy，但当前审查未看到导入阶段对 manifest digest 的完整强制校验；逐文件安装也不是全树原子替换，后续失败可能留下部分状态。

- [export.rs](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-engine/src/transfer/export.rs#L52-L115)
- [export.rs snapshot](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-engine/src/transfer/export.rs#L269-L290)
- [import.rs extract/install](https://github.com/SHORiN-KiWATA/miyu-agent/blob/04a23ccbfc1ee081ec8e2d82090edfa553552456/crates/miyu-engine/src/transfer/import.rs#L157-L209)

YunXi 后续若需要迁移，只允许做显式、版本化、带 digest/大小/条目数量上限、默认排除 secrets、可回滚的导入器。

## 5. 迁移裁决

| Miyu 区域 | YunXi Linux 决策 | 方式 | 放行条件 |
|---|---|---|---|
| fish hook | 发展 | 重写 YunXi adapter，借鉴分类和 UX | 真实 fish PTY 矩阵、函数/alias/多行/command-not-found 全通过 |
| Unix daemon/IPC | 发展 | 新建 headless host facade，吸收协议/锁/事件设计 | version/Ping、frame 限制、单例、Follow/Cancel、崩溃恢复测试 |
| Miyu Web/平台宿主 | 排除当前 Arch 范围 | 不进入 Linux TUI 首版 | 未来若纳入需单独边界评审 |
| Persona/Soul/Companion/Relationship/Mailbox | 保留 YunXi | 直接复用 YunXi Runtime | 回归现有人格与记忆测试 |
| Memory | 保留 YunXi，选择性吸收算法 | adapter/port，不迁 schema | 权威台账、敏感访问、向量索引和 reset 语义不变 |
| Shell/File/Patch/MCP/Skills/Multi-agent | 保留 YunXi | YunXi Tool/MCP/Skill facade | Approval、workspace、sandbox 测试 |
| Arch/AUR/Man/ProtonDB 等 | 选择性增加 | 逐工具 port，默认最小权限 | 网络、权限、用户确认和失败回报测试 |
| export/import | 暂缓 | 先做 YunXi 专用格式 | digest、secret、配额、回滚、symlink/hardlink 测试 |
| voice/Web/Weixin/Windows-only | 当前排除 | 不从 Miyu 搬到 Arch 首版 | 用户重新确认范围后单独设计 |

## 6. 适配前必须通过的验收门

1. **协议门**：旧/新版本握手、坏长度、超大 frame、半包、EOF、Ping、能力协商。
2. **daemon 门**：两个终端并发启动只留一个；socket 权限；锁释放；PID 复用；崩溃后 stale socket 清理。
3. **运行门**：可重连客户端掉线继续；one-shot shellhook 掉线取消并落 interrupted；Follow 不丢尾事件；Cancel 能取消等待中的问题和工具。
4. **fish 门**：真实 fish PTY 覆盖普通命令、function/alias/builtin、glob/命令替换、多行、Ctrl+J、unknown command、嵌套返回 127、中文输入和终端尺寸变化。
5. **数据门**：cwd/session 映射持久化且不会跨用户/跨 home 串线；YunXi memory/profile 不被 Miyu 数据覆盖。
6. **安全门**：Approval 与工具写权限、Landlock unsupported policy、AUR review digest/人工确认、外部脚本网络和 sudo 操作。
7. **迁移门**：所有资源、prompt、JSON schema、脚本和许可证闭包齐全；`cargo test --locked`、格式化、静态依赖门禁和 Arch runner 通过。

在这些门通过之前，任何“直接搬源码”“全量启用 Miyu 工具”都视为未授权的高风险改动。

## 7. 当前工作树动作

- 本轮只新增审计脚本、文件清单和本报告；没有把 Miyu 核心源码复制进 YunXi。
- 现有 YunXi Arch 实验代码保持不动，报告中明确记录其风险，不用文档掩盖问题。
- `MIYU-ADAPTER-ROADMAP.md` 已按“审计门”更新；后续实现必须以本报告的验收门为入口。
