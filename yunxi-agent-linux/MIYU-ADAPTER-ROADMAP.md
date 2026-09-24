# Miyu → YunXi Native 适配路线图

## 目标

把 Miyu 已经验证过的 Linux 原生宿主能力作为参考，经过 YunXi 适配层逐项重写或包裹，同时让 YunXi 继续作为唯一的对话、人格、记忆、陪伴、工具审批和数据边界真相源。

最终形态不是两个 Agent 并列运行，而是：

```text
fish / TUI / Linux tools / daemon host
                 ↓
        YunXi Linux adapter
                 ↓
       yunxi-agent-runtime
        ├─ Persona / Soul
        ├─ Memory / Vector Recall
        ├─ Companion / Relationship / Mailbox
        ├─ Tools / MCP / Skills / Multi-Agent
        └─ Approval / Sandbox / Provider
```

## 适配裁决顺序

两个项目能力冲突时按以下顺序裁决：

1. 安全、审批、沙盒和隐私边界；
2. 记忆与会话数据一致性；
3. fish/daemon 交互可靠性；
4. 人格和陪伴体验；
5. Linux 原生工具覆盖度；
6. 性能与资源占用；
7. 功能数量和视觉/角色化表现。

因此，Miyu 的 Linux 宿主实现可以优先研究，但不能绕过 YunXi 的审批、记忆敏感度、工作区和人格边界。

## 适配层次

### 第一层：重写 Linux 宿主适配层

- fish Enter hook 与 `commandline --tokens-raw` 分类；
- `fish_command_not_found` 兜底；
- 用户级 Unix socket daemon；
- 协议化的审批、用户输入、取消和事件流；
- 会话 origin、Follow、Cancel、断线续跑；
- XDG runtime/state 路径、单例锁和权限收紧；
- zsh/bash hook 的成熟实现。

### 第二层：以 YunXi Tool/MCP/Skills 重新实现 Linux 工具

优先评估 Miyu 已有且符合 YunXi 工具模型的能力：

- Arch/AUR/Arch Wiki 查询；
- Man 手册和本地帮助；
- 文件搜索、内容检索和安全文件操作；
- ProtonDB 与 Linux 游戏兼容性查询；
- 网络搜索、网页读取、天气和汇率等可选 Skills；
- 闹钟、计时和后台任务。

这些能力必须改写成 YunXi Tool/MCP/Skills，而不是直接读取 Miyu 数据库或绕过审批。

### 第三层：选择性吸收产品能力

- Miyu 的 Normal/Dev 双模式映射为 YunXi 的工作/陪伴能力层；
- Miyu 的配置 TUI 映射为 YunXi Linux 配置界面；
- Miyu 的导出/导入能力映射到 YunXi 的会话、记忆和人格导出策略；
- Miyu 的插件开关映射为 Skills/工具白名单。

不直接复制 Miyu 的角色人格、记忆表结构和 Web/QQ/语音平台实现。

## 数据与运行边界

- YunXi Persona/Soul 是唯一人格来源；
- YunXi JSONL 记忆台账是长期记忆权威来源；
- YunXi SQLite 向量索引只做召回排序，不能替代原文和状态检查；
- YunXi Session Store 是会话权威来源；
- Miyu 数据只能通过显式导入器进入 YunXi，禁止启动时隐式合并两个数据库；
- daemon 只在当前用户权限下运行，socket 默认 0600；
- Linux 工具调用仍遵循 YunXi `on-request` 审批和 workspace sandbox；
- Miyu 的 Landlock 宿主能力可以作为 YunXi sandbox backend，但不改变 YunXi 的策略语义。

## 源码适配规范

Miyu 使用 MIT License。直接复用或实质改写 Miyu 源码时：

1. 保留原始版权和 MIT License 文本；
2. 在模块头部标注来源文件和迁移日期；
3. 将 `miyu`、Miyu 数据路径和独立数据库依赖替换为 YunXi adapter；
4. 每个迁移模块增加行为测试，不以“能编译”作为完成标准；
5. 迁移模块不得直接调用 Web、QQ、语音或 Windows 逻辑。

## 审计门与当前状态

在继续适配前，已对固定版本的 Miyu checkout 做全文件清单和关键路径源码审计，见 [`MIYU-SOURCE-AUDIT.md`](MIYU-SOURCE-AUDIT.md) 与 [`audit/miyu-file-inventory.csv`](audit/miyu-file-inventory.csv)。审计确认 daemon、Web 宿主、运行时、IPC、fish hook 和会话生命周期相互耦合；因此“复制 daemon.rs”或“基础 hook 能跑”都不能视为适配完成。

- fish hook：已有实验实现，**未达到 Miyu 行为等价**；缺 `type -q`、复杂 fallback、PTY 回归和完整安装器保护；
- Unix daemon：已有基础 socket 原型，**未达到发布标准**；缺协议版本/帧上限/单例锁/Follow/Cancel/断线语义/持久会话；
- Linux 工具插件：尚未迁移，必须逐个改写为 YunXi Tool/MCP/Skills；
- Miyu 数据导入：尚未实现，禁止直接合并两个数据库；
- zsh/bash：尚未迁移，不在 fish 行为门通过前扩展；
- Web/语音/微信/Windows-only：继续排除 Arch 首版；
- 全量源码拼接：**明确不采用**；任何能力都必须经过 YunXi 适配、测试与边界评审。

下一步只能按审计报告的验收门推进：先协议与 daemon 生命周期，再真实 fish PTY，最后逐工具选择性接入。任何新模块都必须标注“保留 YunXi / adapter / 包裹 / 重写 / 暂缓”，不得以数量驱动扩张。

参考源码：[SHORiN-KiWATA/miyu-agent](https://github.com/SHORiN-KiWATA/miyu-agent)。
