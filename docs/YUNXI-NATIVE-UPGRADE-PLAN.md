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

**门槛**：并发启动、陈旧锁、权限、断线、重连、过期游标和 daemon 崩溃恢复测试通过。

### Phase 2：fish 原生接管

- 完成首词分类、`type -q`、多行、粘贴、Ctrl+J、command-not-found 和嵌套命令边界；
- 已建立真实 fish PTY smoke，覆盖 alias/function、中文自然语言、Ctrl+J、多行、命令替换、重定向和管道；继续扩展为完整行为矩阵；
- 记录 cwd/session/origin，保证 Shell 回显和 YunXi 结果不重叠。

**门槛**：普通命令零误拦截，自然语言零重复执行，PTY resize/中断/退出码一致。

### Phase 3：Linux 系统工具层

- 按只读 → 预览 → 可回滚 → 高风险修改顺序接入工具；
- 建立 pacman/systemd/man/process/network 的 ToolSpec、审批策略和审计事件；
- 可选吸收 Landlock backend，不改变 YunXi 策略层。

**当前增量**：已先落地 `linux_readonly` 固定 ToolSpec，覆盖 `systemd_status`、`man_page`、`process_list` 和 `network_snapshot` 四类本机只读查询。参数经过严格 schema 与 token 校验，执行使用固定 argv 的 `DirectProcessRunner`，不经过 `sh -c`，输出限制为 64 KiB，并记录 Linux tool runtime event。工具仍进入现有 `ToolRouter`、`ToolPolicy`、审批与沙盒诊断链路；缺少发行版工具时返回结构化 `unavailable`，不会把缺包误报为执行成功。当前 CLI 的 `linux-tool` 仍是便捷探针，通用 Runtime ToolSpec 是模型可见的正式入口。

**门槛**：每个工具有 schema、权限矩阵、错误恢复、单元测试和至少一个真实 Linux 验收脚本。

### Phase 4：通用知识平台与 Linux 首发 RAG

- 建立知识空间、访问控制、P0/P1/P2 采集器、规范化器、chunker、embedding worker、SQLite+FTS 索引和 generation 切换；
- 先覆盖 shell/coreutils/fish/systemd/pacman/git/网络诊断；
- 预留 project/private 导入适配器，首版不默认读取用户目录；
- 将 RAG 证据接入 Planner，不直接接入执行器；
- 建立 200+ 任务集和离线评测报告。

**当前增量**：`yunxi-agent-storage` 已建立独立的 `SqliteKnowledgeStore`。它使用
`knowledge.sqlite3`，与长期记忆的 `long-term-vectors.sqlite3` 不共享数据库、表或
FTS 命名空间；当前提供 system/project/private 空间元数据、文档与 chunk 登记、
active generation + owner/visibility 前置过滤的 FTS 查询，以及预留的
`knowledge_vectors` 表。该切片还没有接入 embedding worker、Planner 或执行器，
因此知识内容仍只能作为显式检索结果，不能直接触发命令。

**门槛**：索引可增量更新、失败可回滚、来源可追踪、风险命令可标记、空间隔离和撤回测试通过、RAG 召回指标达标。

### Phase 5：记忆与知识协同

- 保持个人档案、短期会话、长期记忆和知识空间（system/project/private）四类逻辑域；
- 允许会话上下文同时召回“关于用户的记忆”和“关于系统的知识”，但保留不同权限、生命周期和可见性；
- 统一 generation、超时、缓存和 trace，不合并敏感度策略；
- 增加“记忆来源”和“知识来源”的可解释诊断。

**门槛**：个人信息不自动进入知识库，知识文本不改写个人记忆，跨 workspace/用户/知识空间不可串线。

### Phase 6：发行与长期运行

- Arch 包、安装器、升级迁移、卸载和配置诊断；
- systemd user service、日志轮转、资源上限、离线运行和恢复文档；
- 兼容第二个发行版前先冻结 Host/Tool adapter 接口。

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
