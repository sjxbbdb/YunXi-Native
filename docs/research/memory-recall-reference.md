# YunXi 记忆召回参考研究

> 研究性质：只读参考，不是生产实现方案。本文整理本地 Miyu 源码与成熟开源项目的公开资料，用于指导 YunXi 的记忆召回重构。
>
> 研究快照：2026-09-28。外部项目会持续变化，正式实现前仍需重新核对版本、许可证和接口。

## 结论先行

YunXi 不适合把“记忆”简单替换成一个向量表。更稳妥的方向是：

1. 保留不可替代的结构化层：Persona/Soul、用户档案、当前会话状态、审批状态和记忆可见性不能交给相似度决定。
2. 采用 Miyu 已经验证过的“两阶段生命周期”：回合结束先写入短期情景，空闲或达到批量阈值后异步整理为长期事实/长期经历。
3. 召回采用混合检索：中文词面/精确匹配负责姓名、路径、命令、项目名等硬信号；向量负责改写、近义表达和概念关联；最后用 RRF 或等价的 rank fusion 合并。
4. 召回必须是“有门控的自动召回”，不是每次把全部相似结果塞进上下文。先判断当前问题是否需要个人记忆，再做候选检索、权限过滤、时间/可信度/疲劳修正和 token 预算裁剪。
5. 记忆库与知识库必须分开。知识库可回答 Linux 命令与私有资料，记忆库只回答“这个人、这台机器、这段关系、这些历史决定”的信息；两者不可共用数据库文件、表、索引命名空间或写入路径。

## 一、本地 Miyu 源码核实

### 1. 三类记录与生命周期

本地参考文档 `references/miyu-agent/docs/wiki/08-记忆系统.md` 将记忆分为：

| 类型 | 作用 | 召回/保留特征 |
| --- | --- | --- |
| `short_term` | 每个成功回合即时写入的短期日记 | 默认保留 14 天；被有效召回会续期 |
| `long_term` | 从短期日记整理出的、有回溯价值的经历 | 按半衰期衰减，低于阈值标记为 `forgotten`，不物理删除 |
| `fact` | 稳定偏好、设备/环境事实、关系和项目结论 | 可被召回、强化和显式搜索 |

同一文档说明：同一人格累计 14 条未整理日记后，由后台线程异步整理；整理成功后保留短期原文，召回达到 3 次会触发长期化。整理失败指数退避，不能阻塞主回复。

### 2. 召回的真实调用路径

本地源码中的关键入口：

- `references/miyu-agent/crates/miyu-core/src/memory/recall.rs`
  - `MemoryStore::association()`：一次回合的联想入口。
  - `association_candidates()`：先查 facts 和 episodes，并对短期日记做自回声过滤。
  - `finish_association()`：按短期/长期关系去重，执行 `reinforce()`，再打包注入上下文。
  - `search_table()`：词面检索、状态过滤、访问范围过滤和基础排序。
  - `HitRanking::score_bonus()`：把衰减热度、重要性、置信度、truth status 和召回疲劳纳入排序。
- `references/miyu-agent/crates/miyu-core/src/memory/semantic.rs`
  - `association_with_semantic()`：词面候选和语义候选的混合入口。
  - 当前模型缺失的向量每轮最多 inline 补 32 条，剩余由后台批量补齐；语义失败自动退回词面召回。
  - `fuse_kind()`：取语义 top candidates，与关键词 id 通过 RRF 合并，再回表取得完整记录。
- `references/miyu-agent/crates/miyu-base/src/embedding/vectors.rs`
  - `rrf_fuse()`，默认 `RRF_K = 60`。

召回不是纯读取操作。`reinforce()` 会增加 `recall_count`、提高 `strength`、刷新 `last_recalled_at`；短期日记还会刷新过期时间，达到 promotion 次数后标记为待长期整理。这样“被有效使用的记忆”会更稳定，但 `score_bonus()` 又用 `ln(1 + recall_count)` 做疲劳惩罚，防止一条热门事实永久霸屏。

### 3. Miyu 的候选排序与降噪

Miyu 当前排名的主要信号可以概括为：

```text
lexical_score
+ strength * 5
+ importance
+ confidence * 4
+ truth_status_bonus
- ln(1 + recall_count) * fatigue_weight
```

另外还有几道边界：

- `forgotten` 默认不进入自动联想，但显式搜索可以要求包含。
- `rejected` 事实被过滤。
- principal 会话只能看 `public` 或属于当前主体的记录。
- 当前上下文中已出现的日记会被自回声过滤，避免模型刚说完就把自己的话再次召回。
- `association.rs` 将结果按“知识点 / 近期发生的事 / 长期经历”分段，并受总字符预算与单条预算限制。

### 4. 语义检索的降级设计

Miyu 的语义层是增强项，不是单点依赖：

- 词面召回先完成，向量模型可用时再补语义候选。
- 向量按 `(kind, id, model, content_sha256)` 记录；更换模型或内容变化会自然失效并重建。
- 当前回合最多补建少量向量；剩余由后台 worker 批量处理。
- embedding worker 与主进程隔离；模型加载失败、请求失败或超时都回退到关键词检索。
- 中文使用 `jieba-rs` + 紧凑 FST 词典；这比直接按空格切中文更适合命令名、项目名和自然中文混合查询。

### 5. Miyu 的上下文化石与回放原则

`references/miyu-agent/docs/cache-and-prompt-plan.md` 明确了一个很关键的设计：动态联想块追加在当前回合尾部，并冻结为回放侧车数据；redo/replay 不重新检索，也不重新计算当时的记忆关联。这避免同一历史回合因记忆库变化而产生不同输入，也避免破坏供应商缓存和 append-only 日志。

### 6. 需要借鉴、但不能直接照搬的部分

适合 YunXi 借鉴：

- 短期日记 → 异步整理 → 长期事实/经历。
- 词面 + 向量 + RRF 的混合召回。
- 召回强化、时间衰减、召回疲劳惩罚。
- 主体/可见性隔离、自回声排除和动态上下文预算。
- embedding 失败时关键词可用，不能因为语义模型不可用而失去记忆功能。
- 回放冻结和 append-only 侧车。

不能直接照搬：

- Miyu 的表结构、配置名、数据库路径和宿主集成。
- 将知识库与记忆放在同一逻辑系统中的做法。YunXi 的硬约束是两者物理/逻辑隔离。
- Miyu 的默认参数。14 天、3 次、半衰期 7 天只能作为实验初值，必须用 YunXi 的中文/终端场景数据重新校准。

## 二、成熟项目对比

### 1. Letta（MemGPT 系列）

Letta 将记忆分为始终在上下文中的 **memory blocks** 与不自动注入、需要搜索的 **archival memory**。官方文档明确：memory block 有 label、description、value、limit，适合 persona、human、当前任务和高频状态；archival memory 是大容量的向量化历史数据，必须通过搜索显式取回。

参考：

- [Letta Memory blocks（官方文档）](https://docs.letta.com/v1-sdk/memory/memory-blocks)
- [Letta Archival Memory API（官方文档）](https://docs.letta.com/api/python/resources/agents/subresources/passages)
- [Letta memory architecture（官方技能参考）](https://github.com/letta-ai/skills/blob/main/letta/agent-development/references/memory-architecture.md)

对 YunXi 的启发：

- Persona/Soul 和 user profile 应保持小而稳定、始终可见，不能依赖向量召回人格。
- 大量历史、项目经历和旧对话放在可搜索的长期层，不污染每轮固定 system prompt。
- “自动召回”和“模型显式搜索”应是两个入口：前者低预算，后者可让用户/模型主动查更深历史。

不适合照搬：Letta 的 agent-managed block 与云端 API 生命周期。YunXi 的 Persona/Soul 受本地文件和用户明确边界控制，模型不能无审批重写核心人格。

### 2. Mem0

Mem0 的官方流程明确分为 `add` 后的抽取和下一次模型调用前的 `search`：先查相关已有记忆，LLM 抽取持久事实，再做去重和 embedding；检索时结合语义、关键词、实体和时间信号，并要求用 `user_id`、`agent_id`、`run_id` 等范围过滤。

官方还区分三类后端：SQL 保存事实与元数据、向量库保存 embedding、实体存储保存实体信息。默认保存抽取后的干净事实而非原始逐字 transcript；需要保留原文时使用显式 raw 模式。

参考：[Mem0 How it works（官方仓库文档）](https://github.com/mem0ai/mem0/blob/main/docs/core-concepts/how-it-works.mdx)

对 YunXi 的启发：

- 记忆写入应是“候选抽取 + 现有记忆上下文 + dedup/update”，而不是每个回合直接 append。
- 元数据范围必须参与检索，不应在召回后才补做权限判断。
- 关键词、实体、时间和向量应视为互补信号。
- 更新旧事实需要明确 update/delete 语义，不能仅凭新文本并存造成矛盾。

不适合照搬：Mem0 的实体图和托管平台是可选扩展；YunXi 首先要保证本地、可审计、离线可降级，并保留 pending/confirm/reject 流程。

### 3. Graphiti（Zep 开源记忆核心）

Graphiti 面向动态事实和关系，核心记录是 episode、entity、fact。它采用双时间模型：同时记录摄入时间和事件发生时间；事实有 `valid_at`/`invalid_at`，新事实可以使旧事实失效而保留历史。官方说明它用语义、BM25 关键词和图遍历做混合检索，支持增量更新，不依赖批量 GraphRAG 总结。

参考：[Graphiti 官方 GitHub](https://github.com/getzep/graphiti)

对 YunXi 的启发：

- “我现在的设备/项目状态”和“过去某个日期的决定”应分开建模。
- 新状态覆盖旧状态时，最好记录 supersedes/validity，而不是物理删除旧记忆。
- 关系型问题（“我和 YunXi 之间经历过什么”“这个项目由哪个决定演化而来”）未来可以用图关系补强。

不适合现在直接引入：Neo4j/图数据库和多次实体抽取会明显扩大 Linux 版依赖、资源和运维边界。YunXi 第一阶段使用结构化元数据 + 向量/词面混合召回即可，图层留作后续关系记忆升级。

### 4. Haystack

Haystack 的官方混合检索教程直接把 BM25 和 embedding retriever 并行执行，然后用 cross-encoder 进行二次排序。官方特别指出：dense embedding 擅长语义上下文，关键词检索在领域术语、精确词和训练语料不足时往往更强。

参考：[Haystack Creating a Hybrid Retrieval Pipeline](https://haystack.deepset.ai/tutorials/33_hybrid_retrieval)

对 YunXi 的启发：

- 当前 #30 的自然语言查询问题，不应只把 AND 改成 OR 后结束；应建立 lexical candidate、dense candidate、融合、可选 rerank 四段式管道。
- Linux 命令、路径、环境变量和模型名需要保留精确词面通道。
- reranker 应是可选的第二阶段，不能成为每轮主链路的硬依赖。

### 5. LlamaIndex

LlamaIndex 的官方 retriever 文档把 BM25 Hybrid、Reciprocal Rerank Fusion、Relative Score Fusion、Router 和 Ensemble Retrieval 作为独立模块。它提供的思想不是“唯一正确的数据库”，而是把多个 retriever 组合成可评估的检索编排。

参考：[LlamaIndex Retriever Modules](https://developers.llamaindex.ai/python/framework/module_guides/querying/retriever/retrievers/)

对 YunXi 的启发：

- 把“查询改写”“召回候选”“融合”“重排”“过滤”“上下文装配”拆成可测试模块。
- 可以按查询类型路由：精确命令/路径优先 lexical，模糊回忆优先 dense，关系/时间问题再增加 metadata 或图检索。
- 不同记忆类型可以使用不同 top-k、阈值和 token 预算，而不是一个全局 limit。

## 三、项目之间的共同规律

| 问题 | Miyu | Letta | Mem0 | Graphiti | Haystack/LlamaIndex |
| --- | --- | --- | --- | --- | --- |
| 什么时机召回 | 每回合前自动联想 | 核心块常驻，archival 显式搜索 | 搜索前显式调用 | 查询图/混合检索 | 由应用编排 retriever |
| 核心存储 | facts + episodes + embeddings | core blocks + archival | SQL + vector + entity | episodes + entities + temporal facts | DocumentStore + retrievers |
| 词面通道 | Jieba/FST | 取决于 archival 后端 | keyword/entity | BM25 | BM25 |
| 语义通道 | 可选本地 embedding | archival semantic search | embedding | embedding + graph | dense embedding |
| 时间/状态 | strength、半衰期、forgotten、promotion | 由应用/记忆块管理 | temporal metadata | 双时间、事实失效 | metadata/reranker |
| 去重/更新 | organizer + semantic dedup | block 编辑/重写 | extraction + update/delete | 实体/事实整合 | 通常由应用层负责 |
| 失败降级 | 语义失败回关键词 | 由服务实现 | 由配置/后端决定 | 依赖结构化抽取和图后端 | 可组合、需应用处理 |

共同结论是：成熟系统都没有把“向量相似度最高”当作唯一召回标准。结构化元数据、词面精确性、时间、权限、去重和上下文预算共同决定最终是否注入。

## 四、YunXi 建议的召回架构

### 1. 存储边界

建议固定为三层，并将知识库单独拆出：

```text
Persona/Soul + User Profile       结构化、始终可见、人工/审批控制
        │
Short-term Episodes               当前会话与近期情景，原文保留，有限期
        │  idle/batch distillation
Long-term Memory Store             事实/偏好/关系/经历，向量 + 元数据

Knowledge Store                   Linux 命令、私有资料、文档块；独立索引与权限域
```

长期记忆可以使用向量数据库或 SQLite/关系表 + 独立 embedding 表，但必须拥有独立数据库文件/表/索引 namespace 和 migration。知识库不得通过“记忆检索”旁路进入，反之亦然。

### 2. 召回触发门控

先判定问题是否有记忆需求，再决定是否启动召回：

- 明确回指：刚才、之前、你记得、我的偏好、上次项目决定。
- 个人化任务：涉及用户称呼、设备、路径、项目约定、关系阶段。
- 时间/历史问题：什么时候、当时、后来、最近一次。
- 当前工作上下文需要 profile/短期情景时，读取对应结构化层。
- 一般知识问题和纯命令解释优先走知识库，不自动查询私人记忆。

门控结果应记录为 `none | profile | episode | long_term | knowledge | mixed`，避免每轮盲目注入。

### 3. 候选生成

建议每个记忆查询同时生成四路候选：

1. 精确词面：命令、路径、项目名、专有名、日期、模型名、中文分词。
2. 宽松词面：AND 无命中时 OR/BM25 fallback；CJK 使用分词或短语/子串策略，解决自然语言长句全部 0 命中。
3. 语义向量：只查长期记忆向量库，按模型版本过滤；短期情景可在数量受控时参与。
4. 结构化过滤：scope、owner、visibility、memory kind、status、valid time、truth status。

词面和语义都只负责产生候选，不直接决定注入。候选必须在同一条 pipeline 中经过访问控制和状态过滤。

### 4. 融合与排序

第一版可采用 RRF，避免 lexical 分数与 cosine 分数不可比；在此基础上增加业务修正：

```text
fused_rank
+ importance_bonus
+ confidence_bonus
+ temporal_relevance
+ current_project_scope_bonus
+ explicit_user_reference_bonus
- stale_decay
- repeated_recall_fatigue
- contradiction/status_penalty
```

排序必须满足：

- 精确命令/路径出现时，词面命中不能被一个泛化向量结果挤掉。
- 新事实覆盖旧事实时，旧事实保留 provenance，但默认不与新事实同时注入。
- 过度召回不能自我强化；保留 Miyu 的 `recall_count` 疲劳惩罚。
- 不同 kind 有独立配额，例如 profile 只读结构化层、近期 episode 2–3 条、long-term facts 3–5 条，总字符/token 预算硬上限。

### 5. 注入与回放

召回结果应作为动态的 per-turn context sidecar，放在用户消息之后/当前回合尾部；稳定 system prompt 只放 Persona/Soul、隐私边界和召回解释规则。

一旦模型请求开始：

- 冻结本回合选中的记忆 id、版本、排序和渲染文本。
- redo/replay 使用冻结的 sidecar，不因记忆库在后台变化而重检索。
- 记录原始候选与最终注入的 id，但默认不把隐私正文写入普通诊断日志。
- 当前上下文已出现的短期内容不再重复注入。

### 6. 写入、整理和更新

建议沿用 Miyu 的 lifecycle，但改成 YunXi 的审批边界：

1. 回合成功后写入 short-term episode，保存原文、session、source、timestamp、subjects、visibility。
2. 用户明确“记住”或高置信候选可以进入 pending memory；自动抽取默认先 pending，不直接改变核心用户档案。
3. 空闲、达到批量阈值或显式维护命令触发异步 organizer。
4. organizer 只提炼“这个人/这台机器/这个项目/这段关系”的可复用内容；通用教程、临时问答和秘密不进入长期记忆。
5. 与既有事实相同或冲突时使用 `update`/revision；不要简单 append 造成两个互相矛盾的用户事实。
6. 每次长期记忆变更重新计算 embedding；内容 hash 或模型版本变化时标记旧向量失效。

### 7. 可观测性

为解决当前 #31“抽取命中率不清楚、没有诊断”和 #30“召回 0 命中不知原因”，每个回合应提供脱敏诊断：

```text
memory_decision: none/profile/episode/long_term/knowledge/mixed
lexical_candidates: N
dense_candidates: N
fused_candidates: N
filtered_by_scope/status/decay/echo/budget: N...
selected_ids: [kind:id, ...]
latency_ms: gate/lexical/embed/fuse/render
fallback: none | lexical_only | no_match
```

诊断不应默认输出完整私人记忆正文；CLI 可在显式 debug 命令中按权限查看。

## 五、建议的落地顺序

### P0：先修现有召回的确定性问题

- #30：将自然语言检索从单一 AND 改为“精确 AND → OR/BM25/CJK fallback”，保留相关性阈值，并补充相关/不相关回归矩阵。
- #31：增加候选抽取诊断；补充“我叫……/我住在……/我喜欢……”等中文自述规则，但仍保持 pending/确认边界。
- #28：让 Linux frontend 正确桥接 PersonaSettings 与 companion/love-letter 配置，确保诊断路径可达。
- #29：修复 undo list 的内层 journal symlink 与单条损坏隔离；它不是召回功能，但会影响后台维护可靠性。

### P1：建立可评估的混合召回层

- 把 lexical、dense、fusion、filter、render 拆成独立 Rust 模块。
- 加入中文分词/短语/路径/命令的测试集。
- 为每条记忆保存 scope、kind、status、confidence、importance、valid time、source、content hash、embedding model。
- 输出脱敏召回诊断与 latency。

### P2：生命周期与质量

- 异步 organizer、失败重试、promotion 和 decay。
- 语义去重与 revision/update。
- 召回疲劳惩罚、时间意图和当前项目 scope 加权。
- 建立离线 replay 数据集，至少测 `precision@k`、`recall@k`、无关注入率、重复注入率和 p95 延迟。

### P3：关系记忆（可选）

- 仅在事实/经历模型稳定后，增加 entity/relationship/validity 层。
- 先用关系表或 SQLite 结构化索引，不立即引入 Neo4j/图服务。
- 用于“人物—关系—项目—事件”的时间查询，不能改变记忆与知识库的物理隔离。

## 六、研究边界与许可提醒

本研究只提取公开架构思想和本地参考源码行为，不把 Miyu、Letta、Mem0、Graphiti、Haystack 或 LlamaIndex 的代码直接复制进 YunXi。正式实现前需逐项目确认 LICENSE、依赖许可证、版本兼容性，并用 YunXi 自己的测试证明行为符合 Persona/Soul、审批、Sandbox、隐私和知识库隔离约束。

