# Miyu 参考与许可证说明

YunXi Native 参考了 [Miyu Agent](https://github.com/SHORiN-KiWATA/miyu-agent) 的 Linux 宿主思路。Miyu 项目以 MIT License 发布；本仓库的完整固定源码快照位于 `references/miyu-agent/`，原始许可证保留在该目录中。

当前已参考并在 YunXi 适配层中实现的方向：

- fish shell hook 的首词分类、普通命令放行和自然语言兜底；
- `fish_command_not_found` 兜底路径；
- Unix socket daemon 的按需启动方向。

这里的关系是“参考实现 → YunXi 适配”，不是把两个 Agent 合并成一个运行时。迁入实际源码或进行实质性改写时，必须在对应模块保留原项目版权与 MIT License，并在这里追加文件映射。Miyu 的人格、数据库、平台凭证和 Web 资源不会在未经过 YunXi adapter、权限审查与行为测试的情况下进入运行时。
