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

该结果是本地字符/FTS 基线的回归门，不代表真实发行版文档上的最终质量，也不替代后续
人工整理的 200+ 真实任务集。下一步应加入真实 `man`/`--help` 文本、版本差异、危险命令
风险标签、撤回和权限隔离样本，并报告 MRR、source accuracy、risk-label accuracy 与
延迟分位数。

