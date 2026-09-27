# Linux Knowledge Recall Baseline

这是 YunXi Native Linux 知识库的第一份离线召回基线。评测夹具位于
`crates/yunxi-agent-storage/tests/knowledge_recall_baseline_tests.rs`，不会写入任何
运行时 workspace 或用户知识库。

## 范围

- 22 个 Linux 命令/工具：systemd、shell、Git、包管理、进程、内存、磁盘、文件、文本、归档、权限和网络等类别；
- 每个命令 10 个自然语言意图：解释、状态、日志、网络、权限、磁盘、进程、包管理、安全使用和故障排查；
- 共 220 条确定性任务；每条任务都指定一个期望文档，并以同一 `system-linux` active generation 查询；
- 夹具使用稳定的命令帮助意图和固定 provenance/risk 元数据，不写入运行时 workspace 或用户知识库；
- 评测只验证知识检索，不执行检索结果，也不测试 Provider 生成内容。

## 指标

| 模式 | Recall@1 | Recall@5 | MRR | source accuracy | version accuracy | risk-label accuracy |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| lexical / FTS | 220/220 = 1.000 | 220/220 = 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |
| vector-only (`yunxi-local-chargram-v1`) | 0.495 | 0.886 | 0.642 | 1.000 | 1.000 | 0.659 |
| hybrid (FTS + cosine) | 220/220 = 1.000 | 220/220 = 1.000 | 1.000 | 1.000 | 1.000 | 1.000 |

固定回归阈值为：所有模式的 source/version accuracy `>= 0.99`；lexical 的 Recall@5/MRR/risk
分别为 `1.00/1.00/0.99`；vector-only 分别为 `0.85/0.60/0.60`；hybrid 分别为
`0.95/0.85/0.90`。这些是离线回归门，不是生产质量 SLA；向量-only 的结果明确保留为
当前字符 n-gram provider 的防回归基线。运行时采用 FTS + 向量的混合召回，后续替换
embedding 模型或 rerank 策略时必须重新报告并解释全部指标。

同一评测还验证知识边界：generation 更新后旧文档不可见（freshness `1/1`）、不同 owner
无法读取 private 空间（isolation `1/1`），以及撤回后 FTS/向量均清理且 tombstone 阻止
重新写入（retraction `3/3`）。这组结果仍然不代表真实发行版文档上的最终质量；下一步
应加入真实 `man`/`--help` 文本、版本差异和更宽的危险命令样本，并补充延迟分位数回归。

运行命令（`--nocapture` 用于显示指标）：

`cargo test -p yunxi-agent-storage --test knowledge_recall_baseline_tests --locked -- --nocapture`

## WSL 查询延迟观测

使用 `yunxi-agent-linux/tests/knowledge_latency_smoke.sh` 在 Ubuntu-24.04 WSL 的临时
workspace 运行 12 次 FTS 查询和 12 次向量查询。脚本把第一次 CLI 调用标记为 cold，
其余调用标记为 warm-ish；每次仍然是独立 CLI 进程，因此这里的 warm-ish 只表示文件系统
和 SQLite 页缓存可能已经热起来，不能等同于常驻 daemon 延迟。以下是 2026-09-26
一次观测，单位为微秒（µs）：

| 路径 | cold p50 | warm-ish p50 | warm-ish p95 |
| --- | ---: | ---: | ---: |
| FTS 检索 | 27,919 | 25,220 | 33,205 |
| 向量 embedding | 1,215 | 1,383 | 2,515 |
| 向量 SQLite 检索 | 6,139 | 5,093 | 6,716 |
| 向量总耗时 | 21,874 | 21,523 | 23,635 |

这组数字是当前本地字符 n-gram provider、三份真实 `--help` 文本和本机 WSL 文件系统的
观测，不是发布阈值或跨机器承诺。后续更换 embedding provider、增加文档规模或接入
常驻 daemon 后，应重新运行脚本并分别记录冷启动、热查询、无命中和 generation 切换。
