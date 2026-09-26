# YunXi Native Linux 升级总计划

> 版本：v0.1 · 2026-09-24
> 状态：架构基线已确定，按验收门分阶段实施
> 适用范围：`YunXi-Native` Linux 产品线

这份文档把 YunXi 从“可以在 Linux 上运行的 Agent”升级为“Linux 原生的自然语言控制平面”。它规定 Miyu Agent 能力如何 YunXi 化、Linux 宿主如何系统级优化，以及通用本地知识平台如何通过 RAG 向量检索服务于理解、规划和执行。Linux 终端命令只是第一批知识域，后续可以扩展到项目资料和私有化知识。

## 1. 产品目标

Linux 的核心能力不变：Shell、文件系统、进程、服务、包管理器、脚本、管道和开放生态仍然是真实执行层。YunXi 增加一个可审计的意图层：用户可以使用自然语言描述目标，YunXi 将其翻译为可解释的执行计划，并在影响系统前进行权限与风险判断。

```text
自然语言 / 已知 Shell 命令
          ↓
fish / TUI / Unix socket 接入
          ↓
意图解析 + 知识库 RAG + 当前系统上下文
          ↓
执行计划 + 风险分类 + 审批 / dry-run
          ↓
YunXi Tools / MCP / Skills
          ↓
Shell、文件、进程、systemd、包管理、网络与第三方工具
          ↓
流式结果、命令摘要、审计记录、可恢复状态
```

### 固定设计原则

1. **终端优先**：TUI、fish 和 Unix socket 是 Linux 版第一界面，Web、微信和语音不进入首个 Linux 发布闭环。
2. **翻译而不是遮蔽**：YunXi 可以替用户规划命令，但必须展示意图、关键命令、影响范围和结果。
3. **执行前审批**：删除、安装、提权、修改服务、写入系统目录和网络副作用都必须经过显式授权或已声明的策略。
4. **Shell 仍是真实语义**：普通命令继续由 fish 执行；自然语言只在被识别为意图时进入 YunXi。
5. **人格服务于工作**：Persona、Soul、记忆和陪伴能力继续保留，但在 Linux 中用于理解上下文、表达风险和维持长期协作，不覆盖系统安全边界。
6. **本地、可组合、可审计**：优先使用 XDG 路径、本地状态、MCP、Skills、脚本和 Unix 工具；每次执行都能解释和回放。

## 2. 范围与明确不做事项

### 本轮纳入

- Linux Host、用户级 daemon、Unix socket 和版本化 IPC；
- fish 接管、命令/自然语言分类、多行输入和 `command_not_found` 兜底；
- YunXi Runtime 的 Persona、Soul、短期状态、长期记忆、向量召回、Provider、Tools、MCP、Skills、多 Agent、审批和沙盒；
- Linux 文件、进程、systemd、journal、Man、包管理和网络诊断能力；
- 通用本地知识平台；第一批内容为 Linux 命令与系统文档，后续支持项目资料和私有知识；
- XDG 数据布局、单例锁、权限收紧、离线回退、资源上限和诊断指标；
- Arch Linux 首发适配，并为其他发行版保留 Host/工具适配边界。

### 当前不纳入

- Windows/Web/微信/iLink/语音链路的 Linux 化复制；
- Miyu 的人格、数据库、凭证、Web 路由或插件状态模型直接进入 YunXi Runtime；
- 为了功能数量而全量复制 Miyu 工具；
- 默认 root daemon、公开 TCP 服务或无审批的自动执行；
- 将知识库内容当作可直接执行的命令来源。

## 3. Miyu 能力的 YunXi 化

Miyu 是重要的架构和源码参照，但不是第二个 Runtime。所有吸收都经过“参考源码 → YunXi adapter → 行为测试 → 安全验收”四步。

| Miyu 能力 | YunXi 化方式 | 归属 | 验收门 |
|---|---|---|---|
| fish 首词分类、普通命令放行 | 重写为 `yunxi-agent-linux` 的 fish adapter，保留 Shell 原语义 | Linux Host | 命令替换、glob、alias/function、多行和 127 退出码 PTY 矩阵 |
| `fish_command_not_found` | 作为第二道自然语言转发路径，统一进入 YunXi session | Linux Host + Runtime | 不重复执行、不吞掉真实错误 |
| daemon 单例与用户级生命周期 | 采用 YunXi daemon facade，锁、PID/start-time、socket 权限由 Host 负责 | Linux Host | 重启、并发启动、陈旧锁、权限和崩溃恢复 |
| Unix socket IPC | 设计 YunXi 版本化协议，复用事件顺序、Follow、Cancel、重同步思想 | Protocol/Host | frame 上限、版本拒绝、游标过期、断线语义 |
| 会话 origin、可重连客户端 | 映射为 fish origin → YunXi session/thread，保持父子会话和本地存储 | Runtime/Storage | 多终端隔离、断线取消/续接边界 |
| XDG 路径与运行目录权限 | 使用 YunXi 的 XDG data/state/cache，默认 0700/0600 | Storage/Host | 跨用户不可见、旧布局迁移、权限回归 |
| Landlock 等宿主沙盒 | 作为 `yunxi-agent-sandbox` 的可选 Linux backend | Sandbox | policy 存在时 fail-closed；策略语义仍由 YunXi 决定 |
| 配置、插件和工具注册 | 转写为 YunXi Skills/MCP/ToolSpec，不复制 Miyu 开关模型 | Tools/Skills | 每个工具单独权限、schema、回滚测试 |
| Miyu 记忆/SQLite organizer | 只吸收批处理、FTS+semantic 召回和 generation barrier 思路 | Storage | 不改变 YunXi 记忆状态、敏感度和向量边界 |
| Web、QQ、语音和角色系统 | 明确排除，不进入 Linux 首版 | — | 以仓库边界检查阻止误依赖 |

固定源码快照见 [`references/miyu-agent/`](../references/miyu-agent/)，审计结论见 [`MIYU-SOURCE-AUDIT.md`](../yunxi-agent-linux/MIYU-SOURCE-AUDIT.md)。

## 4. Linux 系统级优化

### 4.1 进程与服务模型

- `yunxi-linux` 负责 TUI、一次性命令和安装/诊断入口；
- `yunxi-linux daemon` 负责用户级常驻 Host，不使用 root，不开放 TCP；
- fish hook 使用短连接或可重连连接，按客户端类型定义断线：一次性命令默认取消，可重连客户端可以 Follow；
- 提供 `systemd --user` 单元作为常驻方式，同时保留手动启动和无 systemd 环境；
- daemon 具备单例锁、socket 0600、PID/start-time 校验、优雅退出和崩溃重启保护。

### 4.2 终端接管与交互可靠性

- 只拦截明确的自然语言意图，已知命令、alias、function 和带 shell 语法的输入交给 fish；
- 保留原始输入、当前 cwd、终端尺寸、环境摘要和命令历史的边界，不把完整环境变量写入日志；
- 支持多行、Ctrl+J、粘贴、光标重绘、PTY resize、Ctrl+C 和 Ctrl+D；
- 通过事件 id、origin、session id 防止重复执行、回显错位和跨终端串线；fish hook 使用当前 fish 进程派生的 session id，不再仅按 cwd 共享上下文；
- 所有自然语言执行结果都返回“做了什么、实际命令、退出码、变更摘要、下一步”。

### 4.3 Linux 工具层

第一批工具以只读、可预览、可回滚为优先：

1. 文件搜索、内容检索、目录摘要和补丁预览；
2. 进程、端口、磁盘、内存、日志和 systemd 状态诊断；
3. `man`、`info`、`--help`、包元数据和已安装版本查询；
4. Arch `pacman`/`makepkg`/AUR 审查；安装和升级必须有计划、来源和确认；
5. 网络连通性、DNS、代理、证书和路由诊断；
6. 执行脚本、服务重启和系统配置修改，统一走高风险审批。

工具实现必须通过 YunXi Tool/MCP/Skills 注册，不允许 Linux 工具绕过现有 Sandbox/Approval。

### 4.4 性能和资源策略

- daemon 常驻只保持 socket、事件总线和轻量缓存，不常驻加载大模型；
- Provider、embedding、RAG 索引和工具执行采用按需加载、空闲回收和有界并发；
- 所有队列、frame、输出、历史和诊断日志设置字节/条数上限；
- 命令知识库优先本地小型 embedding，检索阶段不调用远程服务；
- 记录启动耗时、首 token、首个可执行计划、工具耗时、RAG 命中率和 daemon RSS，形成 Linux 性能基线。

## 5. 通用本地知识平台与 RAG

### 5.1 定位：通用平台，Linux 命令首发

知识平台回答“某个领域的可靠资料是什么、它适用于什么版本和范围”。首发集合聚焦 Linux 命令、发行版和系统文档，但底层模型必须从第一天支持多个知识空间：

- **system**：本机 `man`、`--help`、发行版和系统服务资料；
- **project**：项目源码、设计文档、运行手册和团队约定；
- **private**：用户主动导入的私有文档、笔记、资料库和离线知识包；
- **shared**（后续）：经过授权的团队/组织知识，不默认开放给其他用户。

长期记忆回答“用户是谁、项目如何协作”；知识平台回答“某个知识空间中的资料是什么”。二者不能混成一个向量空间：

- 个人档案和高敏感记忆不进入知识库；
- 知识库不写入 Persona/Soul，也不能覆盖审批策略；
- RAG 只提供候选证据和命令语义，不能直接触发执行；
- 最终执行计划必须回到 YunXi Tool/Approval/Sandbox。

每个知识空间都必须有 owner、可见性、来源、版本、保留策略和授权范围。检索先做访问控制过滤，再做关键词/向量召回，不能用“召回后再过滤”替代权限隔离。

### 5.1.1 硬性存储边界

这是不可放宽的工程不变量：

- 长期记忆使用 `long-term-vectors.sqlite3`；知识平台使用 `knowledge.sqlite3`；两者不能共用数据库文件；
- 记忆与知识不得共用表、索引命名空间、generation、迁移脚本或写入路径；
- 任何共享的 embedding/vector 抽象都必须显式携带 `Memory` 或 `Knowledge` domain 类型；
- MemoryRecord 不得进入知识索引，KnowledgeChunk 不得写入记忆台账；
- 删除、归档、敏感度变化和 generation 切换必须分别实现、分别测试；
- 访问控制、审计和诊断中必须能明确显示数据属于哪个 domain。

如果未来引入外部向量引擎，也只能建立两个逻辑集合或两个独立实例，并保留同等的物理/逻辑隔离测试；不能以“统一向量服务”为理由合并两个域。

### 5.2 首发知识域与未来扩展

Linux 首发集合的来源分层：

**P0：本机权威来源**

- 当前系统的 `man`、`info`、命令 `--help`、已安装包元数据；
- 当前发行版、内核、systemd、shell、包管理器版本。

**P1：发行版与上游文档**

- Arch Wiki、Arch 手册页、pacman/makepkg 文档；
- POSIX、GNU coreutils、util-linux、systemd、OpenSSH、Git、网络工具文档。

**P2：经过审核的专题资料**

- 文件系统、权限、进程、网络、容器、编译工具链和常见故障排查 runbook。

P0 必须优先于 P1/P2；回答涉及版本差异时，检索结果必须携带发行版、版本和来源时间。

通用平台的后续导入适配器包括：Markdown/纯文本、源码目录、PDF/HTML、结构化 JSON/YAML、离线知识包和受控网络抓取。导入适配器只负责解析与登记，不改变知识空间的权限和生命周期。

### 5.3 数据模型

知识平台使用独立的 `knowledge.sqlite3`，不与长期记忆的 `long-term-vectors.sqlite3` 混用。SQLite 先采用现有 Rust `rusqlite` 与向量 BLOB/余弦相似度方案，后续数据规模超过本地扫描阈值时再评估 HNSW/专用向量引擎。Linux system、project 和 private 空间可以使用同一个数据库文件，但必须通过空间 id、owner、visibility 和 generation 做硬隔离；高敏感私有空间可单独使用加密数据库文件。

核心表：

```text
knowledge_documents
  id, space_id, owner_id, visibility, source_uri, source_type,
  distro, package, version, section, title, content_hash,
  fetched_at, verified_at, license

knowledge_chunks
  id, space_id, document_id, command_tokens, text, synopsis,
  risk_class, requires_root, distro, version_range, content_hash

knowledge_vectors
  chunk_id, space_id, embedding_model, dimensions, vector_blob,
  indexed_at, generation

knowledge_fts
  chunk_id, space_id, title, command_tokens, text, synopsis
```

每个 chunk 必须有空间、来源、版本、风险等级、内容 hash 和可见性。建议初始切分为 300–700 token，重叠 50–100 token；命令 synopsis、参数约束、示例和危险提示尽量保持在同一 chunk 内。私有资料还必须记录导入者、导入时间、撤回状态和加密状态。

### 5.4 构建与更新流水线

```text
采集本机命令、发行版文档或用户授权的私有资料
          ↓
规范化、去导航噪声、提取命令与版本元数据
          ↓
按章节/命令语义切块 + 风险标注
          ↓
生成 embedding + 写入 SQLite BLOB + FTS5
          ↓
校验 hash、许可证、来源和版本
          ↓
原子切换索引 generation
```

- 首次运行允许离线只构建 P0；
- Linux 首发阶段默认只启用 system 空间；project/private 空间必须由用户显式创建或导入；
- 文档更新使用 content hash 增量重建，不重复计算未变化 chunk；
- 索引构建在后台执行，不阻塞 TUI/daemon 主回路；
- 新索引必须完成 schema、向量维度、来源完整性和危险命令抽样检查后才成为 active generation；
- 失败时保留旧索引，查询继续使用上一代索引；
- 删除或撤回私有资料时生成新的 generation，并保证旧向量不可再被授权查询。

### 5.5 查询与回答流水线

1. 判断输入是普通 Shell 命令、知识问题还是执行意图；
2. 根据当前用户、workspace、显式选择的知识空间和会话策略做访问控制过滤；
3. 从 cwd、发行版、已安装版本、当前工具和用户问题提取过滤条件；
4. 对精确命令 token 做 FTS/BM25 检索，同时对自然语言做 embedding 检索；
5. 合并 lexical + semantic 结果，按 space/distro/version/source/risk 做重排与去重；
6. 返回 5–12 个带来源的候选 chunk，并把证据注入 YunXi 规划上下文；
7. YunXi 生成解释、dry-run 或执行计划；
8. 高风险命令必须展示风险与影响并等待 Approval；知识库文本不得直接作为 shell 字符串执行。

初版采用混合召回：`BM25 + cosine + metadata filter + deterministic rerank`。后续只有在评测证明收益明确时才引入更重的 reranker。

### 5.6 RAG 验收指标

建立不少于 200 条 Linux 任务集，覆盖命令解释、参数选择、故障排查、版本差异和危险操作：

- Recall@5 / MRR：正确文档是否进入前五；
- source accuracy：回答是否引用正确版本和来源；
- command safety：是否把 `rm`、提权、服务重启等风险正确标记；
- execution grounding：计划中的命令是否能在检索证据中找到依据；
- freshness：本机 P0 更新后是否在下一代索引生效；
- latency：冷启动、热查询、无命中和索引切换的 p50/p95。
- isolation：未授权用户、workspace 或知识空间不会召回私有 chunk；
- revocation：撤回或删除资料后，旧 generation 不再返回该资料。

## 6. 阶段计划与验收门

### Phase 0：产品线分离（当前完成）

- 建立 `YunXi-Native` 独立公开仓库和 Linux 工作区；
- 固定 Windows YunXi 与 Miyu 源码快照、许可证和审计文档；
- 共享跨平台 crate 与 Linux Host 独立纳入工作区；
- README、AGENTS、能力矩阵和适配路线图就位。

**门槛**：`cargo fmt --all --check`、`cargo test --locked`、Linux crate release build 通过。

### Phase 1：Host/daemon/IPC 契约

- 定义协议版本、frame 上限、错误码、事件 id、origin、Follow、Cancel、Ping 和 resync；
- 完成用户级单例锁、socket 权限、PID/start-time 检查和 graceful shutdown；
- 提供 `systemd --user` 单元与无 systemd 启动路径。

当前已提供 `yunxi-linux systemd-unit` 输出模板、用户级服务示例、SIGTERM/SIGINT 清理路径、PID/start-time 锁校验，以及有界的“已完成回合”事件游标回放；daemon lock metadata 采用临时文件同步后硬链接抢占，损坏 metadata 不会被直接删除。活动回合断线续跑仍不支持，真实 PTY 矩阵由 Phase 2 的 smoke 先行覆盖。

同时提供真实 Unix socket smoke：`yunxi-agent-linux/tests/daemon_ipc_smoke.sh` 在临时
XDG 目录启动 release daemon，验证版本握手、Ping、未知回合 Follow 重同步、确定性
Provider 配置失败与超限 `Turn` 请求的结构化 `Error` 帧，以及 SIGTERM 后 socket 清理；
超限请求在 `run_accepted` 之前被拒绝，随后仍能 Ping，说明语义限额不会破坏 daemon 生命周期。
它不需要模型凭据，也不执行真实系统工具。该 smoke 与单元测试互补，前者覆盖真实
进程/套接字生命周期，后者继续覆盖锁、回放、协议和字段边界细节。

当前 `Turn` 语义边界为：`prompt` ≤ 64 KiB、`cwd` ≤ 4 KiB、`request_id`/`session_id` ≤
512 字节、`provider`/`model` ≤ 256 字节。它们独立于 24 MiB frame 传输上限，目的是在
Runtime、embedding 和路径处理之前建立资源边界；后续若调整必须同步更新协议文档、单元
测试与真实 IPC smoke。

daemon 还对 `Hello` 与握手后的首个请求设置 5 秒超时，防止半连接长期占用连接任务；该
超时只作用于握手阶段，不限制正常回合的 Runtime 执行时间。

真实 smoke 还覆盖超大 frame 与截断 JSON：这类协议错误只终止当前连接，daemon 主循环和
后续客户端仍可完成握手与 Ping，不把不可信输入升级为进程级故障。

**门槛**：并发启动、陈旧锁、权限、断线、重连、过期游标和 daemon 崩溃恢复测试通过。

### Phase 2：fish 原生接管

- 完成首词分类、`type -q`、多行、粘贴、Ctrl+J、command-not-found 和嵌套命令边界；
- 已建立真实 fish PTY smoke，覆盖 alias/function、中文自然语言、Ctrl+J、多行、命令替换、重定向、管道、窗口 resize、输入态 Ctrl+C 和普通命令退出码；另有 `shell_prompt_cancel_smoke.sh` 使用真实 PTY 与协议假 daemon 验证审批等待期间的 Ctrl+C；继续扩展为完整行为矩阵；
- 记录 cwd/session/origin，保证 Shell 回显和 YunXi 结果不重叠；fish 前台回合收到
  `Ctrl+C` 时向 daemon 发送 `Cancel`，不把中断留在客户端进程层；审批和用户输入等待使用可轮询的 `/dev/tty`，取消后先回收输入任务再发送 `Cancel`，避免后台读取线程吞掉下一条 fish 输入。

**门槛**：普通命令零误拦截，自然语言零重复执行，PTY resize/中断/退出码一致。

### Phase 3：Linux 系统工具层

- 按只读 → 预览 → 可回滚 → 高风险修改顺序接入工具；
- 建立 pacman/systemd/man/process/network 的 ToolSpec、审批策略和审计事件；
- 可选吸收 Landlock backend，不改变 YunXi 策略层。

**当前增量**：已先落地 `linux_readonly` 固定 ToolSpec，覆盖 `systemd_status`、`man_page`、`process_list`、`network_snapshot` 和 Arch `pacman` 只读查询五类本机观察能力。`pacman` 只允许固定的 `--info`（已安装包元数据）和 `--search`（包数据库搜索）模式，明确排除安装、删除、升级、数据库刷新等变更操作。参数经过严格 schema 与 token 校验，执行使用固定 argv 的 `DirectProcessRunner`，不经过 `sh -c`，输出限制为 64 KiB，并记录 Linux tool runtime event。工具仍进入现有 `ToolRouter`、`ToolPolicy`、审批与沙盒诊断链路；缺少发行版工具时返回结构化 `unavailable`，不会把缺包误报为执行成功。当前 CLI 的 `linux-tool` 仍是便捷探针，通用 Runtime ToolSpec 是模型可见的正式入口。

真实验收脚本 `yunxi-agent-linux/tests/linux_tool_smoke.sh` 已覆盖五个只读探针的 JSON
契约、缺少可选系统工具时的 `unavailable` 结果、64 KiB 输出边界，以及 systemd/man
参数中的 shell 语法拒绝（含 pacman 查询）；它不要求 root，也不修改本机状态。

**门槛**：每个工具有 schema、权限矩阵、错误恢复、单元测试和至少一个真实 Linux 验收脚本。

### Phase 4：通用知识平台与 Linux 首发 RAG

- 建立知识空间、访问控制、P0/P1/P2 采集器、规范化器、chunker、embedding worker、SQLite+FTS 索引和 generation 切换；
- 先覆盖 shell/coreutils/fish/systemd/pacman/git/网络诊断；
- 提供 project/private 的显式创建与 stdin 导入适配器，首版不默认读取用户目录；
- 将 RAG 证据接入 Planner，不直接接入执行器；
- 建立 200+ 任务集和离线评测报告。

**当前增量**：`yunxi-agent-storage` 已建立独立的 `SqliteKnowledgeStore`。它使用
`knowledge.sqlite3`，与长期记忆的 `long-term-vectors.sqlite3` 不共享数据库、表或
FTS 命名空间；当前提供 system/project/private 空间元数据、文档与 chunk 登记、
active generation + owner/visibility 前置过滤的 FTS 查询，以及预留的
`knowledge_vectors` 表，并支持按 embedding model、空间、owner、visibility 和
generation 过滤的有界 cosine 检索。Linux 查询入口和 Runtime 会读取空间当前
active generation，不再把 generation `1` 当作运行时事实；未知空间不会被读路径
静默创建，因此首次使用必须先经过受控采集/初始化。

本增量已把 project/private 的受控入口落地：`knowledge-space-init` 只允许用户显式创建
非 system 空间，重复初始化必须完全匹配 metadata；`knowledge-import-stdin` 只读 stdin，
不扫描路径、不执行导入内容，并把文档绑定到空间当前 generation 后入队 embedding job。
project 首版仅允许 owner visibility，private 允许 owner/private；source/version 必须与
空间一致。`knowledge-search` 与 `knowledge-vector-search` 支持显式 `--space-id`、
`--owner`、`--visibility`，不匹配的访问身份会被拒绝，导入的 project/private 证据不会
自动进入 Linux Planner。真实验收脚本为
`yunxi-agent-linux/tests/knowledge_project_private_smoke.sh`，覆盖重复导入、向量闭环、
空间隔离、显式撤回和长期记忆数据库未被触碰。`knowledge-retract` 现在也接受显式
space/owner/visibility，并在同一存储边界内清理文档、FTS、向量和 embedding job。

当前已经提供同步的单文档 `knowledge-index` 原语和 `knowledge-vector-search` CLI，
使用本地字符 n-gram provider 建立独立向量并支持增量跳过、快照一致性校验和原子
替换；generation 的 staging 文档、独立向量/任务队列和原子切换已经落地，system 空间
的 Planner 只读召回已接入，project/private 仍保持显式查询边界。

本增量已补齐 durable `knowledge_embedding_jobs` 队列契约：作业关联
`document_id`、embedding model 与 generation，入队会校验文档代际并对重复请求幂等；
领取使用 SQLite `IMMEDIATE` 事务，`complete`/`fail` 只允许合法的 worker 状态转换，
失败只记录队列状态并保留已有向量。实际任务执行由有界 CLI worker 提供；system
knowledge 的 Planner 只读召回已经接入，project/private 仍保持显式查询边界，daemon
级跨 workspace 调度留待后续增量。

当前增量已把队列接成一个可验证的最小执行闭环：`SqliteKnowledgeStore` 提供有界的
`process_next_embedding_job`，先按 worker lease 领取，再用当前本地字符 n-gram provider
建立整篇文档向量，成功后完成任务；provider/model 不匹配或索引失败会记录为 `failed`
并保留旧向量。Linux CLI 默认一次性处理有限 batch；显式 `--watch` 才会在一个明确
workspace 内按间隔轮询，便于 systemd/supervisor 托管。active generation 的读取边界、
staging worker 和原子激活已经落地。

为避免 daemon 或终端进程崩溃后留下永久 `running` 任务，领取事务还会回收超过五分钟
未更新的 worker lease，并把它重新置为 `pending`；旧 worker 随后提交 complete/fail
会因 lease 身份不匹配而被拒绝。这里仅处理崩溃恢复，不把 `failed` 任务自动重试，
失败任务的自动重试由后续的有界退避调度边界负责。

同时提供了显式 `retry_embedding_job`/`knowledge-retry` 恢复边界：只有 `failed` 状态
且尚未超过三次尝试的任务才能重新排队，原始 `last_error` 会保留用于诊断。它是
人工强制恢复入口，真正的常驻 daemon 调度、告警和跨任务退避仍留待后续设计。

队列现在为每个任务持久化 `next_attempt_at_millis`。provider 或索引临时失败会按
有界指数退避自动到期重试（最多三次），而文档缺失、generation 过期和 provider
model 不匹配被视为终态失败；lease 回收仍立即恢复，不套用退避。常驻 daemon 的
调度、告警和跨任务退避策略仍不在本切片范围内。

当前新增了显式 `knowledge-worker --watch` 轮询器作为过渡调度边界：它绑定一个明确的
workspace，按间隔以有限 batch 领取到期任务，复用已有 lease/退避/重试契约，不扫描其他
workspace、不自动激活 generation，并可由 systemd 或 supervisor 托管。跨 workspace 的
常驻 daemon 调度、公平性、告警与统一跨任务策略仍留待后续切片；当前 worker 已能在
Ctrl+C 或 SIGTERM 下优雅退出并输出 stopped 记录。

显式空间还提供只读的 `knowledge-space-list` 元数据入口，按稳定的 `space_id` 排序，
不读取文档正文、chunk 或向量，便于本地诊断空间隔离而不扩大知识内容暴露面。

`ensure_system_space` 只在 system 空间不存在时初始化 generation `1`，不会覆盖已有
active generation。采集得到的文档会在写入前绑定当前 active generation，避免空间升级
后新旧资料串代；staging 采集与 active 激活保持两个明确事务边界。

当前已先增加独立的 generation manifest 前置契约：每个空间可以创建不影响 active
指针的 `building` generation，记录 embedding 模型/维度和文档完整性计数，并在计数
与摘要校验完成后转为 `ready`。该 manifest 只描述“未来可激活”的候选代际，不承载
主文档、chunk 或向量；候选数据由 generation-scoped staging 表承载，并通过 readiness
和原子激活边界进入 active。

随后已增加 generation-scoped staging 文档/chunk 写入边界。staging 主键包含
`space_id + generation + document_id`，因此同一逻辑文档可以在 active 主表和候选代际
并存；写入只替换候选代际的文档与 chunk，不写 FTS、主向量或 `knowledge_spaces.generation`。
随后已增加独立 `knowledge_staging_vectors` 表和候选代际 embedding 边界：候选向量与
active 向量完全隔离，readiness 会按候选表统计文档、chunk 和向量覆盖，且候选代际不要求
提前生成 active FTS。随后又增加了独立的
`knowledge_staging_embedding_jobs` 队列：候选任务拥有自己的 lease、重试预算、退避和
过期回收，不与 active jobs 共用状态；`process_next_staging_embedding_job` 只读取
staging 文档并写入 staging 向量，严格不触碰 active 文档、chunk、vector、job 或
`knowledge_spaces.generation`。随后已增加 `activate_generation` 原子边界：它在一个
SQLite `IMMEDIATE` 事务内重新校验 ready manifest、staging 文档/chunk/vector 覆盖、
任务状态与跨空间 document ID 冲突，再复制到 active 表、由触发器重建 FTS、切换
`knowledge_spaces.generation` 并清理候选 staging 行；任何校验或写入失败都会回滚旧代际。
Linux CLI 已暴露这条安全管道：`knowledge-generation-begin` 创建候选代际，
`knowledge-stage-help`/`knowledge-stage-man` 只写候选，`knowledge-generation-worker`
有界处理候选 embedding 任务，`knowledge-generation-readiness` 输出完整性诊断，
`knowledge-generation-seal` 在候选完整后把 manifest 标记为 ready，最后由
`knowledge-generation-activate` 在显式确认后切换 active 指针；未 ready 的候选不会被激活，
运行中的 active generation 不会被后台任务自动替换。

采集 CLI 的成功路径现在会在 `ingest_text` 完成后为当前文档 generation 自动创建
本地 provider 的 pending job，并在 JSON 结果中返回 job 元数据；它只入队、不启动
worker。文档内容发生变化时，ingest 事务会同时清理旧向量和旧 embedding jobs，保证
下一次采集不会复用已经 completed 的旧任务。

同时新增了无副作用的 `knowledge_ingest` 基础层：在进入存储前清理 ANSI
终端控制符、NUL、CRLF 和多余空行，执行输入上限检查，并按段落与字符边界
生成带稳定 hash 和 ordinal 的有界 chunk。它不读取任意路径，也不启动命令，
后续采集器和 embedding worker 只接收这层的确定性输出。

`SqliteKnowledgeStore::ingest_text` 已将这条路径接到存储边界：文档元数据、chunk
替换和旧 chunk 对应向量的清理在同一 SQLite 事务中完成。重复导入不会留下旧的
FTS 内容或孤立向量；文档 hash 与分块参数都未变化时会跳过重建并保留已有向量；
事务失败时旧版本仍保持可检索。

同时提供 `replace_document_vectors` 批量边界：embedding worker 先完整校验文档、
空间、generation、chunk 引用和维度，再在一个事务内替换指定模型的向量集合，拒绝
批次时旧索引保持不变。

知识库还提供 `index_document_with_embeddings`：它复用现有本地字符 n-gram provider
为当前文档 chunk 生成向量，再通过上述批量边界写入独立的 `knowledge_vectors`。
这里复用的是 embedding 算法，不是记忆数据库或记忆表；后续替换为更强的本地模型
只需保持 provider 的模型、维度和批量写入契约。索引入口会先检查当前文档的 chunk、
模型、generation、维度和向量 blob 完整性；内容未变化且向量齐全时直接跳过，缺失、
内容变化或模型/维度变化时才重建。
即使调用方绕过 ingest 直接 `upsert_chunk` 更新内容，存储层也会在同一事务中失效该
chunk 的旧向量，保证增量索引不会复用过期 embedding。
Embedding 计算与批量写入之间若发生 chunk 更新，替换事务会再次核对 chunk 内容 hash，
发现快照失效就拒绝写入并要求重试，避免旧内容向量落到新 chunk 上。

当前还新增了 Linux P0 `man` 采集器：只接受经过 token 校验的 topic/section，使用
固定 `man --locale=C -P cat` argv、只读执行策略、`MANPAGER/PAGER/TERM` 固定环境和
64 KiB 输出上限；成功结果带稳定的 `system-man:<section>:<topic>@<source-version>` 文档身份并调用
`ingest_text`，缺少 man、非零退出、超时或截断不会把 stderr 当作知识正文，也不会
进入 Planner 或执行器。

在同一边界上补充了受限的 `--help` 采集器，仅允许 `fish`、`git`、`systemctl`、
`pacman`、`ip` 以及 `awk`、`cat`、`cp`、`find`、`grep`、`ls`、`rm`、`sed`、`tar`。
它始终固定执行 `<allowlisted-command> --help`，不接受路径、参数或任意可执行文件，
并与 `man` 采集共用 provenance、输出上限和 `ingest_text`；后续若扩展名单必须逐项
审查副作用与版本差异。
真实 Linux 验收脚本 `yunxi-agent-linux/tests/knowledge_help_smoke.sh` 会逐项检查这些
命令的文档身份、source version、固定 `[command, --help]` argv，以及缺少工具时的
结构化状态，并拒绝带路径、参数或 shell 语法的伪命令。

`system-linux` 空间本身使用 `mixed` 版本标记，以便同时收纳 Ubuntu、Arch 等
发行版的系统资料；实际的发行版/运行时版本仍逐文档、逐 chunk 保存并随召回结果
保留，且 P0 `man/help` 文档 ID 包含 source version，避免同主题跨发行版互相覆盖。
project/private 空间不采用这一例外，文档版本必须与空间版本一致，防止私有
知识在版本不匹配时被静默写入。

`knowledge-man` 与 `knowledge-help` 默认使用同一份有界 `os-release` 探测结果，
显式 `--source-version` 仍可覆盖；探测失败只记录 `unknown`，不猜测发行版。

Linux Runtime 已增加只读 `knowledge-search` 入口，并通过独立的只读
`LinuxPlanContext` 在 Linux 目标构建 prompt 时并行召回 system 空间的 FTS 与本地向量证据。
该 facade 只负责 active generation、版本过滤、去重、来源标注和固定字符预算；两类证据
都被明确标记为不可信参考材料，不能覆盖 Tool/Approval/Sandbox 规则，也不会直接进入执行器；
Windows Runtime 不启用该分支。
运行时还会从受控的 `metadata_json` 中保留 `collector` 与 `risk_level` provenance，
并将其作为证据头部的可审计标签输出；原始 metadata/argv 不会直接注入 prompt。
当同一 chunk 同时出现在 FTS 与向量结果中时，运行时保留 FTS 证据、过滤重复向量项，
避免 prompt 预算被同一份知识重复占用。

CLI 还提供 `knowledge-index` 和 `knowledge-vector-search`：前者使用当前本地
字符 n-gram provider 为已登记文档建立独立向量，后者在相同 system 空间内进行
有界 cosine 召回。两者都不读取或写入长期记忆向量库，后续接入更强 embedding
模型时仍通过 provider + 批量替换契约。

两个只读查询 CLI 支持显式 `--diagnostics`。开启后，JSON 才会带有有界诊断：
`knowledge-search` 记录 `retrieval_latency_us`，`knowledge-vector-search` 分开记录
embedding、数据库检索和总耗时，并给出 `result_count`；默认输出保持既有 schema 和
字段兼容。它们仅用于本地性能基线，不包含查询正文、路径或私有知识内容，也不改变
召回排序、空间过滤和执行边界。

为避免混合空间的跨发行版误召回，`knowledge-search` 与
`knowledge-vector-search` 都支持可选的 `--source-version` 精确过滤；不传时保持
向后兼容的跨版本召回，但每条结果仍保留真实版本 provenance。Linux Runtime 自动
召回只读取有界的 `/etc/os-release`（缺失时尝试 `/usr/lib/os-release`）生成同一
过滤值；无法可靠解析时保持未过滤召回，不会猜测发行版或版本。

同时提供显式的 `knowledge-retract` 边界：调用方必须匹配 system 空间、owner、
generation 和 visibility，存储层在一个事务内先删向量、再删 chunk/FTS、最后删文档；
权限不匹配或事务失败时原文档保持可检索。该入口不操作 project/private 空间，也不
触碰长期记忆向量库。

知识存储还新增了独立的确定性评估夹具，覆盖跨发行版 FTS/向量过滤、provenance
保留、内容更新后的旧召回失效，以及撤回后的 FTS/向量清理。当前另有 20 个 Linux
命令 × 10 个意图的 200 条离线基线任务，报告见
`docs/reports/linux-knowledge-recall-baseline.md`；它不把评估样本写入运行时知识库，
也不冒充真实发行版文档的最终质量。当前报告同时记录 FTS 200/200 的确定性门和
本地 chargram 向量-only 的 Recall@1/5 防回归基线；下一阶段继续扩展真实 `man/help`
文本、风险标签、版本差异，并在更强 embedding 或 rerank 接入后重新评测。

**门槛**：索引可增量更新、失败可回滚、来源可追踪、风险命令可标记、空间隔离和撤回测试通过、RAG 召回指标达标。

### Phase 5：记忆与知识协同

- 保持个人档案、短期会话、长期记忆和知识空间（system/project/private）四类逻辑域；
- 允许会话上下文同时召回“关于用户的记忆”和“关于系统的知识”，但保留不同权限、生命周期和可见性；
- 统一 generation、超时、缓存和 trace，不合并敏感度策略；
- 增加“记忆来源”和“知识来源”的可解释诊断。

当前已落地第一道协同诊断：Linux Runtime 的 `context_assembled` 元数据记录知识召回
是否存在、`knowledge_recall_status`（evidence/no_hit/no_active_space/search_error/
embed_error/skipped_prompt）、active generation、FTS/向量证据数量、`source_version`、本轮检索耗时、
失败阶段和最多 8 条不含正文的 `knowledge_provenance`；记忆继续通过独立的 `MemoryRecall` 事件和上下文元数据记录
scope、source、预算、丢弃原因和召回数量。两者只共享本轮上下文组装的观测面，不共享
数据库、敏感度、权限或写入路径。

**门槛**：个人信息不自动进入知识库，知识文本不改写个人记忆，跨 workspace/用户/知识空间不可串线。

### Phase 6：发行与长期运行

- Arch 包、安装器、升级迁移、卸载和配置诊断；
- systemd user service、日志轮转、资源上限、离线运行和恢复文档；
- 兼容第二个发行版前先冻结 Host/Tool adapter 接口。

当前增量：已新增 `packaging/arch/yunxi-native/PKGBUILD` 与配套用户级 systemd unit，
固定源码 commit 后从 workspace 构建 `/usr/bin/yunxi-linux`；service 对 daemon 设置
`MemoryHigh=1536M`、`MemoryMax=2G`、`TasksMax=128`、`LimitNOFILE=4096` 和
`OOMPolicy=stop`，不自动启用服务、不创建 root daemon、不删除用户数据。配套的
`package-smoke.sh` 可在无 Arch 环境中静态验证这些安装、安全和生命周期边界。
`packaging/README.md` 明确了 Arch 构建、fish hook、卸载和数据边界；安装/升级/回滚的
完整可重复流水线仍未宣称完成。

**门槛**：冷启动/热查询/常驻 RSS/p95 延迟有基线；安装、升级、回滚和卸载可重复执行。

## 7. 风险与回滚

| 风险 | 防线 | 回滚方式 |
|---|---|---|
| fish 误拦截普通命令 | 保守分类、`type -q`、PTY 矩阵 | 关闭 hook，恢复纯 fish |
| daemon 重复实例或串会话 | 单例锁、origin/session、事件 id | 停止 user service，使用一次性 CLI |
| RAG 给出过期或错误知识 | P0 优先、版本 metadata、来源和 generation | 切回上一代索引或禁用知识召回 |
| 知识库建议危险命令 | risk_class + Approval + dry-run | 只读模式，拒绝执行 |
| 私有知识越权召回 | space/owner/visibility 前置过滤、加密空间、撤回 generation | 立即禁用该空间并重建索引 |
| Linux 发行版差异 | Host/Tool adapter 分层 | 禁用对应 distro adapter，不影响核心 Runtime |
| Miyu 许可证或代码边界不清 | 固定快照、来源映射、MIT 保留、适配测试 | 暂停该模块迁移，回退到 YunXi 原生实现 |

## 8. 完成定义

YunXi Native 达到第一版完成，不以“能启动”作为标准，而必须同时满足：

- 普通 Shell 命令行为与原生 fish 一致；
- 自然语言任务可解释、可审批、可取消、可追踪；
- daemon 常驻稳定，断线和重连语义明确；
- Persona、Soul、记忆、Provider、Tools、MCP、Skills 和 Sandbox 边界保持 YunXi 一致；
- 知识平台可本地构建、增量更新、按空间向量召回、来源追踪和回滚；Linux 命令只是首发知识域；
- RAG 只提供证据，不绕过执行策略；
- Arch 首发包可安装、升级、回滚和卸载；
- 所有关键能力都有自动化测试、真实 Linux 验收和性能基线。

后续开发默认按本计划推进；任何新增 Miyu 能力、Linux 工具或知识源，都必须先标注所属阶段、适配边界、数据影响和验收门，再进入实现。
