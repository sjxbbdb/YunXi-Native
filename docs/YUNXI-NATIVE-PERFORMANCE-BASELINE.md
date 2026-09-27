# YunXi Native 性能基线

这份基线记录 `yunxi-agent-linux/tests/performance_smoke.sh` 的可重复测量方法和一次
实测结果。它用于回归比较，不是所有硬件、发行版或模型 Provider 的 SLA；Provider
网络耗时不在本基线内。

## 测量方法

脚本对 release `yunxi-linux` 执行：

- 9 次独立 `--help` 进程启动，记录进程启动耗时；
- 5 个临时用户 daemon，记录 socket 就绪并 Ping 成功的耗时；
- 每个 daemon 就绪后采集 20 次 `/proc/<pid>/status` 的 `VmRSS`，共 100 个样本。

运行方式：

```bash
bash yunxi-agent-linux/tests/performance_smoke.sh
```

## 2026-09-27 WSL2 实测

| 环境 | 启动 p50 / p95 | daemon 就绪 p50 / p95 | RSS p50 / p95 |
|---|---:|---:|---:|
| Ubuntu 24.04 WSL2 | 12.28 / 13.27 ms | 22.42 / 23.57 ms | 5772 / 5852 KiB |
| Arch Linux WSL2 | 13.66 / 27.77 ms | 17.50 / 25.19 ms | 6032 / 6068 KiB |

样本数分别为启动 9、daemon 就绪 5、RSS 100；文件系统可能已 warm。两套发行版
均使用同一 WSL2 kernel 和同一 release 构建入口。未来接入真实 systemd、不同 CPU
架构或更大知识索引时，应重新记录并比较 p95、RSS 及错误率。
