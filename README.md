# YunXi Native

> **The natural-language control plane for Linux.**
>
> 让云熙成为 Linux 的原生 Agent：不再要求人先记住命令，而是让终端理解人的意图，同时保留 Linux 的开放、可组合、可审计和可控。

YunXi Native 是 YunXi Agent 的 Linux 产品线。它不是把 Windows/Web 版本缩小后重新打包，也不是把 Miyu 代码拼在一起，而是重新设计 Linux 的交互入口：TUI 和 fish 终端是第一界面，YunXi Runtime 是理解、规划、授权和执行的核心。

## 特别鸣谢与技术来源

YunXi Native 明确引用了 **Shorin（SHORiN-KiWATA）开发的 [Miyu Agent](https://github.com/SHORiN-KiWATA/miyu-agent)** 的部分 Linux 原生架构与源码实现。感谢 Shorin 对 Linux Agent 交互方式的探索与开源贡献；Miyu 为我们重新思考“让 Agent 接管终端”提供了重要的工程参照。

当前重点参考与适配的部分包括：

- fish 接管、首词分类、普通命令放行与自然语言兜底；
- `fish_command_not_found` 的第二道转发路径；
- 用户级 daemon、Unix socket、单例生命周期与事件回放模型；
- XDG runtime/state 路径、权限收紧、断线与会话生命周期设计；
- 与上述机制直接相关的源码组织方式和行为测试思路。

Miyu 的完整固定源码快照保存在 [`references/miyu-agent/`](references/miyu-agent/)，当前参考版本为 `0.6.2`、commit `04a23ccbfc1ee081ec8e2d82090edfa553552456`。Miyu 的 MIT License 和版权信息一并保留。我们采用“参考源码 → YunXi 适配层 → 独立行为测试”的方式吸收这些成果，不直接覆盖 YunXi 的 Persona、Soul、记忆、审批、沙盒或数据边界。

完整的 Linux 升级路线见 [`docs/YUNXI-NATIVE-UPGRADE-PLAN.md`](docs/YUNXI-NATIVE-UPGRADE-PLAN.md)。计划把 Linux 终端命令作为第一批知识域，同时预留 project/private 知识空间，后续可接入用户授权的私有化知识，并通过 RAG、空间权限、来源追踪和 generation 回滚保持可控。

> 当前状态：Linux-native foundation / experimental。Arch Linux 是第一目标平台；daemon、fish 接管和 IPC 正在按验收门推进，尚未宣称生产级稳定。

## 这解决什么问题

Linux 的能力很强，但交互通常要求用户记住大量命令、参数、路径、管道和工具差异。YunXi Native 增加一层“自然语言 → Linux 意图”的翻译层：

```text
自然语言输入
      ↓
fish / TUI 接入
      ↓
YunXi 意图解析与执行计划
      ↓
审批、沙盒、工作区和风险检查
      ↓
Shell / 文件系统 / 进程 / 包管理 / MCP / Skills
      ↓
终端中的流式结果、命令摘要与可追踪记录
```

已知的 Shell 命令仍然交给 Shell；只有自然语言意图进入 YunXi。系统操作不会因为“接管终端”而绕过审批，也不会把用户输入盲目拼接成命令。

## 新的设计哲学

- **Terminal-first**：终端不是备用界面，而是产品主界面。
- **Natural language as shell surface**：自然语言是 Shell 的另一种表达方式，不是旁边的聊天窗口。
- **Translate, don’t obscure**：YunXi 可以替用户规划命令，但要说明将做什么、影响什么以及执行结果。
- **Approval before impact**：删除、安装、系统配置和网络动作必须有明确授权、预览或 dry-run。
- **Composable by default**：拥抱 Unix 管道、MCP、Skills、脚本和第三方工具，不封闭成单一功能列表。
- **Local and transparent**：人格、记忆、会话、授权和执行事件优先本地保存，失败要可解释。
- **Personality serves interaction**：云熙的 Persona/Soul 仍然保留，但它在 Linux 中表现为理解上下文、协助完成工作和守住边界，而不是装饰性的聊天角色。

## 架构

```text
fish / TUI / future terminal clients
                ↓ Unix socket
        Linux Host + daemon + IPC
                ↓
        YunXi Runtime (唯一真相源)
        ├─ Persona / Soul
        ├─ Memory / Vector Recall
        ├─ Provider / Session
        ├─ Tool / MCP / Skills / Multi-agent
        └─ Approval / Sandbox / Workspace
```

Linux Host 负责终端接入、daemon 生命周期、IPC、事件回放和 Linux 路径；Runtime 负责真正的 Agent 能力。二者不能互相复制人格、记忆或权限逻辑。

### Linux 系统工具层（当前增量）

Linux 版已经开始把系统能力接入为固定的 `linux_readonly` ToolSpec：`systemd_status`、`man_page`、`process_list`、`network_snapshot`。它们只读本机状态，使用严格的 JSON schema、参数白名单和固定 argv 直接进程执行，不经过 `sh -c`，并沿用 YunXi 既有的 ToolRouter、ToolPolicy、审批、沙盒诊断和审计事件。缺少发行版工具时返回结构化 `unavailable`，不会自动改用任意 shell 命令。

### 知识库边界（Phase 4 基础切片）

Linux 知识库已经有独立的 `SqliteKnowledgeStore` 基础：数据库文件为 `knowledge.sqlite3`，与长期记忆的 `long-term-vectors.sqlite3` 物理分离。知识空间、文档、chunk、generation、owner 和 visibility 会在检索前校验，当前支持 FTS5 和按模型隔离的有界向量检索。采集前会经过确定性的文本规范化和分块，不读取任意路径；`ingest_text` 在单事务内替换文档 chunk 并清理旧向量，文档 hash 与分块参数未变化时会跳过重建，避免索引与向量残留；`replace_document_vectors` 可为 embedding worker 原子替换一个文档的模型向量集合。它还没有接入 Planner 或执行器，知识文本不会被当作 shell 命令直接运行。

当前可通过 Linux CLI 的 `knowledge-index` 为已登记文档建立本地字符 n-gram 向量，
再用 `knowledge-vector-search` 做有界召回；这是同步索引基础，不代表后台 embedding
队列或自动执行已经启用。

Linux 查询入口和 Runtime 会读取知识空间的当前 active generation，并校验 owner 与
visibility；不会把 generation `1` 当作永久默认值。首次使用由 `knowledge-help` 或
`knowledge-man` 受控初始化 system 空间，后续采集会绑定当时的 active generation，
不会覆盖已有代际。未来的 staging/原子激活仍按升级计划单独实现。

队列闭环现在也可显式验证：先用
`yunxi-linux knowledge-enqueue <document-id> --cwd .` 为文档当前 generation 入队，
再用 `yunxi-linux knowledge-worker --max-jobs 1 --cwd .` 处理有限数量的任务。worker
只使用当前本地 provider，成功后才完成任务；模型不匹配、代际过期或索引失败会记录为
`failed`，不会覆盖已有向量。该命令是一次性、有界执行入口，不会自行扫描工作区，也不
会替代未来的 daemon/systemd 调度器。
领取中的任务带五分钟 lease；worker 崩溃后，下一次领取会回收过期 lease，旧 worker
的迟到提交会被拒绝。失败任务可用 `yunxi-linux knowledge-retry <job-id> --cwd .`
显式恢复，最多三次尝试且保留 `last_error`。对于 provider/索引临时失败，worker 会
写入持久化 `next_attempt_at_millis`，按有界指数退避自动到期重试；代际过期、文档
缺失和模型不匹配会直接终态失败，不会在错误条件下形成无界自动循环。worker 仍是
一次性有界 CLI，不会自行变成长驻调度器。
成功的 `knowledge-man`/`knowledge-help` 采集会在写入文档后自动创建当前 provider 的
pending job，并在 JSON 中返回 job 元数据；随后可用 `knowledge-worker` 有界处理。
如果文档正文变化，旧向量与旧 job 会在同一 ingest 事务中失效，下一次采集会重新入队。

Linux 版现在提供只读 P0 `man` 采集入口：
`yunxi-linux knowledge-man fish --section 1 --source-version ubuntu-24.04 --cwd .`。
它只运行固定的 `man --locale=C -P cat` argv，把成功正文送入独立 system knowledge
space；topic/section 不允许 shell 语法或路径，缺少 man、非零退出或超时只返回结构化
状态，不写入知识库。

还提供受限的本机命令帮助采集：
`yunxi-linux knowledge-help systemctl --source-version ubuntu-24.04 --cwd .`。
当前只允许 `fish`、`git`、`systemctl`、`pacman`、`ip`，始终固定为
`<命令> --help`，不会执行用户提供的路径或参数。

`system-linux` 是可容纳多发行版资料的混合版本空间：空间自身标记为 `mixed`，
但每个文档和 chunk 仍保留采集时传入的发行版/运行时版本（例如
`ubuntu-24.04` 或 `arch-rolling`）；`man/help` 采集器也把该版本纳入文档 ID，
因此同一主题的不同发行版资料可以并存。project/private 空间则继续要求文档版本与
空间版本严格一致，避免私有资料发生静默串版本。

采集后的 system 知识可以用 `yunxi-linux knowledge-search <query> --cwd .` 只读检查；
在混合空间中可用 `--source-version ubuntu-24.04` 做精确版本过滤。
Linux Runtime 会有限召回同一空间的 FTS 与本地向量证据并把它们标记为不可信参考；它不会替代
人格、记忆、审批或沙盒，也不会把知识文本直接当作命令执行。
同一 chunk 若被两种检索同时命中，只保留一份 FTS 证据，避免重复占用上下文预算。
在 Linux 上，Runtime 只读取有界的 `/etc/os-release`（缺失时尝试
`/usr/lib/os-release`）生成精确的 `source_version` 过滤；无法可靠解析时保持未过滤
召回，不会猜测发行版或版本。

知识库向量化目前可复用本地字符 n-gram provider，通过独立的
`index_document_with_embeddings` 批量生成 `knowledge_vectors`；这与长期记忆向量库
继续保持不同数据库、不同表和不同检索边界。索引前会检查当前文档的 chunk、模型、
generation、维度和向量完整性；内容未变化且向量齐全时直接复用，只有缺失、内容变更
或模型/维度变化时才重建。
低层 `upsert_chunk` 也会在同一事务中使该 chunk 的旧向量失效，避免绕过 ingest
路径更新内容后误用旧 embedding。

CLI 验证路径：先运行 `yunxi-linux knowledge-index <document-id> --cwd .`，再运行
`yunxi-linux knowledge-vector-search <query> --cwd . --source-version ubuntu-24.04 --limit 5`。
不传 `--source-version` 时保持跨版本召回；传入后 FTS 与向量检索都只返回精确匹配
的 chunk。当前默认模型为
`yunxi-local-chargram-v1`，这是可替换的本地 provider，不代表最终 embedding 选型。

如果来源需要撤回，使用 `yunxi-linux knowledge-retract <document-id> --cwd .`。
撤回只允许命中固定的 `system-linux` 公共空间，并在一个事务内删除文档、chunk、
FTS 行和向量；找不到文档不会误报成功，也不会触碰 project/private 或长期记忆库。

这条边界是 Linux 原生交互的第一步：先让 YunXi 能可靠地理解并观察系统，再进入预览、可回滚修改和高风险操作。CLI 还提供 `yunxi-linux linux-tool describe|processes|network|systemd-status|man` 作为本机诊断入口；它与 Runtime ToolSpec 同样禁止写入和任意命令拼接。

## 仓库结构

```text
crates/
├─ yunxi-agent-core          # Agent 事件、输入、回合和控制
├─ yunxi-agent-runtime       # Provider、记忆、工具和会话编排
├─ yunxi-agent-persona       # Persona / Soul / 本地身份
├─ yunxi-agent-storage       # 本地状态与记忆存储
├─ yunxi-agent-tools         # Shell、文件、补丁、MCP、Skills
├─ yunxi-agent-tui           # 终端交互
└─ ...                       # 共享跨平台能力

yunxi-agent-linux/           # Linux 原生入口
├─ src/main.rs               # TUI 与 Linux CLI
├─ src/shell.rs              # fish 接管、daemon 与 socket 原型
├─ README.md                 # Linux 子项目说明
├─ audit/                    # Miyu 源码审计工具与文件清单
└─ MIYU-SOURCE-AUDIT.md      # 适配前审计与验收门

references/
├─ yunxi-agent-windows/      # Windows YunXi 源码只读快照
├─ miyu-agent/               # Miyu 0.6.2 固定 commit 只读快照
└─ REFERENCE-SOURCES.md      # 来源、许可证和使用边界
```

`references/` 不参与 Cargo 构建，也不自动进入运行时。它们只用于理解设计、核对行为和定位可迁移的 Linux 思路。

## 构建环境

第一目标环境是 Arch Linux：

```bash
sudo pacman -S --needed base-devel git rustup fish
rustup default stable
```

如果要使用在线 Provider，请在当前 shell 配置对应凭证；没有凭证时可以使用离线 Runtime。

## 构建与运行

```bash
cargo fmt --all --check
cargo test --locked
cargo build --release -p yunxi-agent-linux
./target/release/yunxi-linux --cwd "$PWD"
```

常用参数：

```text
--cwd <PATH>       工作区路径，默认当前目录
--provider <NAME>  Provider profile
--model <NAME>     覆盖模型名
--live             强制使用在线 Provider
--offline          强制使用离线 Runtime
```

## fish 接管

安装实验性 hook：

```bash
./target/release/yunxi-linux fish-init
source ~/.config/fish/conf.d/yunxi.fish
```

目标交互：

```text
ls -la                         → 交给 fish
git status                     → 交给 fish
帮我找出最近修改的 Rust 文件     → 交给 YunXi
解释一下这个编译错误             → 交给 YunXi
```

当前 hook 仍处于 Linux-native foundation 阶段。已覆盖 fish 运行时 alias/function、中文自然语言、Ctrl+J 多行的真实 PTY smoke；真正发布前还必须通过 glob/命令替换、`command_not_found`、嵌套命令返回 127、终端尺寸变化、PTY 断线和 daemon 重启测试。

## systemd 用户服务

Linux 版同时提供不依赖 systemd 的手动/按需启动路径，以及可选的 `systemd --user` 常驻方式。先把 release binary 放到约定位置，再安装 unit：

```bash
install -Dm755 target/release/yunxi-linux ~/.local/bin/yunxi-linux
mkdir -p ~/.config/systemd/user
yunxi-linux systemd-unit > ~/.config/systemd/user/yunxi-linux.service
systemctl --user daemon-reload
systemctl --user enable --now yunxi-linux.service
```

查看、停止和移除：

```bash
systemctl --user status yunxi-linux.service
journalctl --user -u yunxi-linux.service -f
systemctl --user disable --now yunxi-linux.service
rm -f ~/.config/systemd/user/yunxi-linux.service
```

该 unit 以当前用户运行，不使用 root，不开放 TCP；daemon 自己仍负责 Unix socket、单例锁、审批和会话。没有 `systemd --user` 的环境继续使用 `yunxi-linux daemon` 或 fish hook 的按需拉起路径。

## 安全边界

- 默认不使用 root，不开放 TCP daemon。
- 普通 Shell 命令仍由 Shell 执行，YunXi 不替换 Shell 的最终语义。
- 文件、进程、安装和网络操作必须经过 YunXi Tool/Approval/Sandbox。
- Persona、Soul、长期记忆和向量索引不由 Miyu 数据库覆盖。
- 长期记忆向量库与知识库向量库始终分离，不能共用数据库、表、索引命名空间或写入路径。
- AUR、sudo、系统服务和公开网络动作需要额外的人工确认与审计记录。
- daemon 断线语义必须区分可重连客户端与一次性 shell hook，不能统一“断线继续”。

## Windows 版与 Linux 版

Windows 版继续保留 Web、语音、微信/iLink、PowerShell、Windows 凭据和任务调度等平台能力；Linux 版的开发重点转向终端、daemon、fish、IPC、Linux 工具和开放生态。

两边共享 YunXi Runtime 的人格、记忆、Provider、工具和安全原则，但不共享平台宿主实现。Linux 版不会把 Windows/Web/语音模块作为依赖带进来。

## 技术参考

- [YunXi Windows 源码快照](references/yunxi-agent-windows/)
- [Miyu 源码快照](references/miyu-agent/)
- [参考来源与许可证说明](references/REFERENCE-SOURCES.md)
- [Miyu 源码审计报告](yunxi-agent-linux/MIYU-SOURCE-AUDIT.md)
- [能力边界矩阵](yunxi-agent-linux/CAPABILITY-MATRIX.md)
- [Linux 升级总计划](docs/YUNXI-NATIVE-UPGRADE-PLAN.md)

Miyu Agent 以 MIT License 发布；其原始许可证保留在 `references/miyu-agent/LICENSE`。任何未来的实质性改写都必须保留来源说明、许可证和行为测试。

## 当前开发顺序

1. 完成 Linux Host、Unix socket、协议版本和 daemon 单例生命周期。
2. 完成真实 fish PTY 行为矩阵，避免误执行、重复执行和会话串线。
3. 接入 Linux 文件、进程、包管理、Man、Arch/AUR 等工具，每个工具单独审查权限。
4. 建立 MCP/Skills/脚本扩展规范。
5. 通过稳定性和安全验收后，再拆分发布节奏和发行包。

这不是“给聊天 Agent 加一个 Linux 模式”，而是 YunXi 的 Linux 原生产品线。
