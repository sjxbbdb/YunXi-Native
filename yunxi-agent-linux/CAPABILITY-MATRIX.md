# YunXi Native · Linux 能力边界矩阵

这份矩阵用于比较 [Miyu Agent](https://github.com/SHORiN-KiWATA/miyu-agent) 与 YunXi Agent，并决定 YunXi Native 哪些能力可参考、哪些能力必须通过适配层重写、哪些能力明确不纳入。

状态含义：

- **已有**：主 YunXi Runtime 已经具备，Linux 版可以直接复用。
- **已接入**：Arch 子项目已经有入口或基础实现。
- **可移植**：Miyu 已验证过，适合移植，但要接入 YunXi 的 Runtime 和安全边界。
- **需重设计**：概念相近，但两边的数据模型或交互边界不同，不能直接复制。
- **暂不纳入**：与当前 Linux 版目标冲突，或属于另一平台/设备链路。

## 1. 入口与交互

| 能力 | Miyu | YunXi | Linux 决策 |
|---|---|---|---|
| TUI 对话 | 已有，普通模式与 Dev 模式 | 已有，统一 Runtime 事件流 | 已接入，保留 YunXi TUI |
| fish 接管 | 已有，完整 Enter hook、原始首词判断、多行、运行时命令识别、兜底 | 主项目此前没有 Linux 原生 fish 宿主 | 已有实验实现；复杂语法和完整 PTY 矩阵仍待验收 |
| zsh 接管 | 已有，单行集成 | 暂无 Linux 版实现 | 后续可移植，不与 fish 共用未经验证的 hook |
| bash 接管 | 已有，单行集成 | 暂无 Linux 版实现 | 后续可移植 |
| Web UI | 已有 | 已有 | 暂不纳入 Linux 版 |
| 通讯平台 | QQ 等接入 | 微信/iLink 接入 | 暂不纳入 Linux 版 |
| 语音唤醒与播报 | 可选 `miyu-voice` | 独立 Voice Runtime | 暂不纳入 Linux 版 |
| 配置 TUI | 已有 | 主要由 CLI/Web 配置 | 可增加 YunXi Linux 配置入口 |

## 2. fish 接管与 daemon

| 能力 | Miyu | YunXi 当前状态 | 差距/边界 |
|---|---|---|---|
| 原始首词分类 | `commandline --tokens-raw`，避免命令替换和 glob 副作用 | 已按同样思路实现 | 需要继续覆盖更多 fish 语法边界 |
| 普通命令放行 | 可识别命令原样交给 fish | 已实现 | 保持 fish 作为最终执行者 |
| 自然语言转发 | 未识别命令转 daemon | 已实现 | 由 YunXi Runtime 处理人格、记忆和工具 |
| `fish_command_not_found` | 第二道兜底 | 已实现保守分支 | 需要覆盖复合命令、未知命令和 127 退出码 |
| 多行输入 | 完整处理 | 基础实现 | 后续补齐提示符重绘和复杂语法判断 |
| Ctrl+J 换行 | 已有 | 已接入 | 保持一致 |
| 剪贴板/附件 | fish hook 内置粘贴处理 | 暂无 Linux 版附件协议 | 需要先定义 YunXi 附件模型 |
| 历史记录 | 接管输入可写入 Miyu 历史 | Runtime 有会话历史 | 需要统一 fish 历史与 YunXi 会话记录的边界 |
| 提示符与光标 | Miyu 处理光标隐藏、提示符重绘、AI 输入回放 | 基础输出已实现 | 可移植，但需单独做终端兼容性测试 |
| Unix socket | 已有成熟 IPC、协议版本和单例生命周期 | 已实现版本化 socket daemon、有界完成回合回放、Ping、Cancel 契约 | 活动回合断线续跑、过期游标与真实客户端矩阵仍需验收 |
| 断线继续 | 可重连客户端继续；one-shot CLI/shellhook 断线取消 | 当前连接断开时生命周期不完整 | 按客户端类型分别实现，不能统一写成“断线继续” |
| 会话续接 | 终端会话、命名会话、Normal/Dev 车道 | YunXi 父子 session 与本地存储 | 需要建立 fish origin → YunXi session 的持久映射 |

## 3. 人格、记忆与陪伴

| 能力 | Miyu | YunXi | Linux 决策 |
|---|---|---|---|
| 默认人格 | 二次元角色人格 | YunXi Agent / 云熙人格与灵魂 | 保留 YunXi，不替换成 Miyu 人格 |
| 人格编辑 | 可创建人格、选择功能插件 | profile、soul、voice、boundary 等结构化层 | 复用 YunXi Persona |
| 灵魂文件 | 以角色设定和功能开关为主 | 独立 soul 内容与稳定内在规则 | 保留原样与本地优先边界 |
| 短期记忆 | 短期日记 | 短期会话状态，承接上下文语气 | 保留 YunXi 机制 |
| 长期记忆 | 短期日记、长期日记、知识点；SQLite 与分词召回 | JSONL 权威台账 + SQLite 本地向量索引 | 保留 YunXi 透明记忆与向量召回 |
| 记忆整理 | 后台线程按阈值整理、时间衰减 | 成功回复后候选抽取、状态/敏感度/审批 | 可参考 Miyu 后台整理，但不能绕过 YunXi 审批 |
| 关系阶段 | 不是主模型 | relationship stage、她界面、关系档案 | 保留 YunXi |
| 情书信箱 | 无同等核心模块 | Companion mailbox、情书任务 | 保留 YunXi |
| 情绪/陪伴 | 角色扮演与插件行为 | Rule-first companion、追问、主动关怀、安静时段 | 保留 YunXi |

## 4. 工具、插件与 Linux 能力

| 能力 | Miyu | YunXi | Linux 决策 |
|---|---|---|---|
| Shell 执行 | Linux 原生工具与插件 | Shell Tool Runtime | 保留 YunXi 审批与 workspace 边界 |
| 文件操作 | 读写、搜索、查找、删除 | 文件工具、补丁和变更事件 | 保留 YunXi 工具模型 |
| 补丁/代码修改 | 有开发模式和工具链 | Patch、Skills、多 Agent | 保留 YunXi |
| MCP | 有 MCP/插件桥接 | MCP Runtime | 直接复用 YunXi |
| Skills | 有 persona 级技能资源 | Skills crate 与技能路由 | 直接复用 YunXi |
| 多 Agent | Dev/后台任务/子代理 | Multi-agent Runtime | 直接复用 YunXi，后续接 daemon 后台任务 |
| Linux/Arch 专用工具 | AUR、Arch Wiki、PKGBUILD、Man、ProtonDB、游戏兼容性 | 当前没有同等专用插件集合 | 可增加为 YunXi Linux Skills，不直接复制 Miyu 插件状态模型 |
| Linux 只读主机 ToolSpec | systemd、Man、process、network 通过插件/工具层 | `linux_readonly` 固定 argv ToolSpec，沿用 Approval/Sandbox/Audit | 已接入四类观察能力；修改类工具仍需单独设计 |
| 网络搜索/网页读取 | 内置或可选搜索服务 | 由 MCP/工具能力承载 | 先保持 YunXi Provider/工具边界 |
| 天气/汇率/闹钟 | 作为内置插件 | 不是当前核心 Runtime 能力 | 后续作为可选 Skills，不进入第一版核心 |
| 生图/搜图/视觉 | 由插件和多模态模型提供 | 非 Arch TUI 第一阶段目标 | 暂不纳入 |

## 5. 安全、存储与运行方式

| 能力 | Miyu | YunXi | Linux 决策 |
|---|---|---|---|
| 工具授权 | 插件开关、会话沙盒和交互确认 | Approval-first，默认 `on-request` | 以 YunXi 审批为权威 |
| 沙盒 | Linux Landlock 等宿主能力 | WorkspaceWrite、sandbox policy、exec runner | 可吸收 Miyu 的 Landlock 宿主层，但不改变 YunXi 策略语义 |
| daemon 权限 | 当前用户 daemon、Unix socket；同时可由 Web 宿主打开 HTTP 端口 | 当前用户 daemon、socket 0600 | Arch TUI 版不使用 root、不开放 TCP；若未来有 Web 必须另行评审 |
| 数据库 | SQLite 集中状态，支持导出/导入 | 会话、记忆、信箱等分层存储 | 继续采用 YunXi 分层，daemon 负责串行化访问 |
| 路径 | `~/.miyu` 体系 | XDG data/state/cache + workspace `.yunxi` | 使用 XDG 标准，不迁移成 Miyu 路径 |
| 隐私 | 导出时明确提示密钥风险 | 本地优先、脱敏诊断、密钥不写日志 | 保留 YunXi 隐私边界 |
| 故障策略 | daemon/插件独立化 | fail-soft、离线 Provider 回退 | 两者结合，模块失败不阻断基础对话 |

## 6. 结论：什么应该合并，什么不能合并

### 直接采用 Miyu 思路

- fish Enter hook 和 `commandline --tokens-raw` 分类方式（包括 `type -q`、多行和 command-not-found 边界）；
- `fish_command_not_found` 作为第二道兜底；
- 用户级 Unix socket daemon 的协议、单例锁和生命周期模型；
- daemon 与客户端分离，支持审批/用户输入事件；
- Linux 上使用 XDG runtime/state 路径；
- 已增加协议版本、单例锁、Follow（仅已完成回合的有界回放）和 Cancel；活动回合断线恢复仍待实现。

### 保留 YunXi 作为唯一真相源

- Persona、Soul、Companion、Relationship 和 Mailbox；
- 个人档案、短期状态和长期向量记忆的分层策略；
- Provider 自动选择与离线回退；
- Tool、MCP、Skills、多 Agent、Approval 和 Sandbox 语义；
- 工作区边界、敏感记忆审批、失效链和透明 JSONL 台账。

### 不直接复制 Miyu

- Miyu 的角色人格和插件开关不能覆盖 YunXi 灵魂文件；
- Miyu 的记忆表结构不能直接替代 YunXi 的记忆候选、敏感度和向量索引；
- Miyu 的 Linux 专用插件不能直接绕过 YunXi 工具审批；
- Web、语音、微信不进入当前 Arch 版第一阶段；
- 不为了“能力数量”引入与 Linux 目标无关的生图、社交平台和桌面 UI。

## 7. 当前实施状态

- Arch TUI：已有。
- XDG 存储目录：已有。
- fish hook 基础接管：已有保守模式与显式 takeover 模式，均已通过真实 fish + PTY smoke；尚未完成 Miyu 行为等价和完整 PTY 矩阵。
- 知识 worker：已有单 workspace `--cwd` 与显式多 workspace `--workspace` fleet；fleet 只处理调用方明确列出的工作区，跨轮 round-robin、单工作区故障隔离和脱敏 JSON 已覆盖；daemon 级持久化调度仍待实现。
- Unix socket daemon：已补齐协议版本、frame 上限、单例锁、Ping、Cancel，以及已完成回合的有界 Follow 回放；活动回合断线续跑仍未实现。
- daemon 活动回合断线续跑、过期游标和持久化会话映射：待实现。
- Miyu Linux 专用 Skills：待评估，不在核心 Runtime 中硬编码。

这份矩阵不是把两个项目合并成一个产品，而是定义 YunXi Native 的适配边界：宿主层可以借鉴成熟实现，Runtime、人格、记忆和安全语义仍由 YunXi 负责，避免 Linux 版在扩展时失去一致性。
