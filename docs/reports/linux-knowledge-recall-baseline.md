# Linux Knowledge Recall Baseline

这是 YunXi Native Linux 知识库的第一份离线召回基线。评测夹具位于
`crates/yunxi-agent-storage/tests/knowledge_recall_baseline_tests.rs`，不会写入任何
运行时 workspace 或用户知识库。

## 范围

- 20 个 Linux 命令/工具：systemd、shell、Git、包管理、进程、内存、磁盘、文件、文本、归档、权限和网络等类别；
- 每个命令 10 个自然语言意图：解释、状态、日志、网络、权限、磁盘、进程、包管理、安全使用和故障排查；
- 共 200 条确定性任务；每条任务都指定一个期望文档，并以同一 `system-linux` active generation 查询；
- 评测只验证知识检索，不执行检索结果，也不测试 Provider 生成内容。

## 指标

| 指标 | 结果 |
| --- | ---: |
| 任务总数 | 200 |
| Recall@1 | 200/200 = 1.00 |
| Recall@5 | 200/200 = 1.00 |
| 运行方式 | `cargo test -p yunxi-agent-storage --test knowledge_recall_baseline_tests --locked` |

同一夹具还会用 `yunxi-local-chargram-v1` 建立独立向量并运行相同的 200 条查询。当前
向量-only 基线为 Recall@1 `99/200 = 0.495`、Recall@5 `180/200 = 0.900`；这是一条
防回归基线，不冒充最终质量门。运行时使用 FTS + 向量的混合召回，后续更换 embedding
模型或 rerank 策略时必须重新报告并解释这两个指标。

该结果是本地字符/FTS 基线的回归门，不代表真实发行版文档上的最终质量，也不替代后续
人工整理的 200+ 真实任务集。下一步应加入真实 `man`/`--help` 文本、版本差异、危险命令
风险标签、撤回和权限隔离样本，并报告 MRR、source accuracy、risk-label accuracy 与
延迟分位数。

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
