# Miyu 源码迁移说明

Arch Linux 大融合会逐步迁移 [Miyu Agent](https://github.com/SHORiN-KiWATA/miyu-agent) 的部分源码与实现。Miyu 项目以 MIT License 发布。

当前已参考并改写的范围：

- fish shell hook 的首词分类、普通命令放行和自然语言兜底；
- `fish_command_not_found` 兜底路径；
- Unix socket daemon 的按需启动方向。

迁入实际源码或实质性改写时，必须在对应模块保留原项目版权与 MIT License，并在这里追加文件映射。Miyu 的人格、数据库、平台凭证和 Web 资源不会在未经过 YunXi adapter 的情况下进入运行时。
