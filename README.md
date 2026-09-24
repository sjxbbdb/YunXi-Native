# YunXi Native

> **The natural-language control plane for Linux.**
>
> 让云熙成为 Linux 的原生 Agent：不再要求人先记住命令，而是让终端理解人的意图，同时保留 Linux 的开放、可组合、可审计和可控。

YunXi Native 是 YunXi Agent 的 Linux 产品线。它不是把 Windows/Web 版本缩小后重新打包，也不是把 Miyu 代码拼在一起，而是重新设计 Linux 的交互入口：TUI 和 fish 终端是第一界面，YunXi Runtime 是理解、规划、授权和执行的核心。

## 特别鸣谢与技术来源

YunXi Native 明确引用了 **Shorin（SHORiN-KiWATA）开发的 [Miyu Agent](https://github.com/SHORiN-KiWATA/miyu-agent)** 的部分 Linux 原生架构与源码实现。感谢 Shorin 对 Linux Agent 交互方式的探索与开源贡献；Miyu 为我们重新思考“让 Agent 接管终端”提供了重要的工程参照。

当前重点参考与适配的部分包括：

- fish 接管、首词分类、普通命令放行与自然语言兜底；
- `fish_command_not_found` 的第二道转发路径；
- 用户级 daemon、Unix socket、单例生命周期与事件回放模型；
- XDG runtime/state 路径、权限收紧、断线与会话生命周期设计；
- 与上述机制直接相关的源码组织方式和行为测试思路。

Miyu 的完整固定源码快照保存在 [`references/miyu-agent/`](references/miyu-agent/)，当前参考版本为 `0.6.2`、commit `04a23ccbfc1ee081ec8e2d82090edfa553552456`。Miyu 的 MIT License 和版权信息一并保留。我们采用“参考源码 → YunXi 适配层 → 独立行为测试”的方式吸收这些成果，不直接覆盖 YunXi 的 Persona、Soul、记忆、审批、沙盒或数据边界。

> 当前状态：Linux-native foundation / experimental。Arch Linux 是第一目标平台；daemon、fish 接管和 IPC 正在按验收门推进，尚未宣称生产级稳定。

## 这解决什么问题

Linux 的能力很强，但交互通常要求用户记住大量命令、参数、路径、管道和工具差异。YunXi Native 增加一层“自然语言 → Linux 意图”的翻译层：

```text
自然语言输入
      ↓
fish / TUI 接入
      ↓
YunXi 意图解析与执行计划
      ↓
审批、沙盒、工作区和风险检查
      ↓
Shell / 文件系统 / 进程 / 包管理 / MCP / Skills
      ↓
终端中的流式结果、命令摘要与可追踪记录
```

已知的 Shell 命令仍然交给 Shell；只有自然语言意图进入 YunXi。系统操作不会因为“接管终端”而绕过审批，也不会把用户输入盲目拼接成命令。

## 新的设计哲学

- **Terminal-first**：终端不是备用界面，而是产品主界面。
- **Natural language as shell surface**：自然语言是 Shell 的另一种表达方式，不是旁边的聊天窗口。
- **Translate, don’t obscure**：YunXi 可以替用户规划命令，但要说明将做什么、影响什么以及执行结果。
- **Approval before impact**：删除、安装、系统配置和网络动作必须有明确授权、预览或 dry-run。
- **Composable by default**：拥抱 Unix 管道、MCP、Skills、脚本和第三方工具，不封闭成单一功能列表。
- **Local and transparent**：人格、记忆、会话、授权和执行事件优先本地保存，失败要可解释。
- **Personality serves interaction**：云熙的 Persona/Soul 仍然保留，但它在 Linux 中表现为理解上下文、协助完成工作和守住边界，而不是装饰性的聊天角色。

## 架构

```text
fish / TUI / future terminal clients
                ↓ Unix socket
        Linux Host + daemon + IPC
                ↓
        YunXi Runtime (唯一真相源)
        ├─ Persona / Soul
        ├─ Memory / Vector Recall
        ├─ Provider / Session
        ├─ Tool / MCP / Skills / Multi-agent
        └─ Approval / Sandbox / Workspace
```

Linux Host 负责终端接入、daemon 生命周期、IPC、事件回放和 Linux 路径；Runtime 负责真正的 Agent 能力。二者不能互相复制人格、记忆或权限逻辑。

## 仓库结构

```text
crates/
├─ yunxi-agent-core          # Agent 事件、输入、回合和控制
├─ yunxi-agent-runtime       # Provider、记忆、工具和会话编排
├─ yunxi-agent-persona       # Persona / Soul / 本地身份
├─ yunxi-agent-storage       # 本地状态与记忆存储
├─ yunxi-agent-tools         # Shell、文件、补丁、MCP、Skills
├─ yunxi-agent-tui           # 终端交互
└─ ...                       # 共享跨平台能力

yunxi-agent-linux/           # Linux 原生入口
├─ src/main.rs               # TUI 与 Linux CLI
├─ src/shell.rs              # fish 接管、daemon 与 socket 原型
├─ README.md                 # Linux 子项目说明
├─ audit/                    # Miyu 源码审计工具与文件清单
└─ MIYU-SOURCE-AUDIT.md      # 适配前审计与验收门

references/
├─ yunxi-agent-windows/      # Windows YunXi 源码只读快照
├─ miyu-agent/               # Miyu 0.6.2 固定 commit 只读快照
└─ REFERENCE-SOURCES.md      # 来源、许可证和使用边界
```

`references/` 不参与 Cargo 构建，也不自动进入运行时。它们只用于理解设计、核对行为和定位可迁移的 Linux 思路。

## 构建环境

第一目标环境是 Arch Linux：

```bash
sudo pacman -S --needed base-devel git rustup fish
rustup default stable
```

如果要使用在线 Provider，请在当前 shell 配置对应凭证；没有凭证时可以使用离线 Runtime。

## 构建与运行

```bash
cargo fmt --all --check
cargo test --locked
cargo build --release -p yunxi-agent-linux
./target/release/yunxi-linux --cwd "$PWD"
```

常用参数：

```text
--cwd <PATH>       工作区路径，默认当前目录
--provider <NAME>  Provider profile
--model <NAME>     覆盖模型名
--live             强制使用在线 Provider
--offline          强制使用离线 Runtime
```

## fish 接管

安装实验性 hook：

```bash
./target/release/yunxi-linux fish-init
source ~/.config/fish/conf.d/yunxi.fish
```

目标交互：

```text
ls -la                         → 交给 fish
git status                     → 交给 fish
帮我找出最近修改的 Rust 文件     → 交给 YunXi
解释一下这个编译错误             → 交给 YunXi
```

当前 hook 仍处于 Linux-native foundation 阶段。真正发布前还必须通过：function/alias、glob/命令替换、多行、Ctrl+J、`command_not_found`、嵌套命令返回 127、终端尺寸变化、PTY 断线和 daemon 重启测试。

## 安全边界

- 默认不使用 root，不开放 TCP daemon。
- 普通 Shell 命令仍由 Shell 执行，YunXi 不替换 Shell 的最终语义。
- 文件、进程、安装和网络操作必须经过 YunXi Tool/Approval/Sandbox。
- Persona、Soul、长期记忆和向量索引不由 Miyu 数据库覆盖。
- AUR、sudo、系统服务和公开网络动作需要额外的人工确认与审计记录。
- daemon 断线语义必须区分可重连客户端与一次性 shell hook，不能统一“断线继续”。

## Windows 版与 Linux 版

Windows 版继续保留 Web、语音、微信/iLink、PowerShell、Windows 凭据和任务调度等平台能力；Linux 版的开发重点转向终端、daemon、fish、IPC、Linux 工具和开放生态。

两边共享 YunXi Runtime 的人格、记忆、Provider、工具和安全原则，但不共享平台宿主实现。Linux 版不会把 Windows/Web/语音模块作为依赖带进来。

## 技术参考

- [YunXi Windows 源码快照](references/yunxi-agent-windows/)
- [Miyu 源码快照](references/miyu-agent/)
- [参考来源与许可证说明](references/REFERENCE-SOURCES.md)
- [Miyu 源码审计报告](yunxi-agent-linux/MIYU-SOURCE-AUDIT.md)
- [能力边界矩阵](yunxi-agent-linux/CAPABILITY-MATRIX.md)

Miyu Agent 以 MIT License 发布；其原始许可证保留在 `references/miyu-agent/LICENSE`。任何未来的实质性改写都必须保留来源说明、许可证和行为测试。

## 当前开发顺序

1. 完成 Linux Host、Unix socket、协议版本和 daemon 单例生命周期。
2. 完成真实 fish PTY 行为矩阵，避免误执行、重复执行和会话串线。
3. 接入 Linux 文件、进程、包管理、Man、Arch/AUR 等工具，每个工具单独审查权限。
4. 建立 MCP/Skills/脚本扩展规范。
5. 通过稳定性和安全验收后，再拆分发布节奏和发行包。

这不是“给聊天 Agent 加一个 Linux 模式”，而是 YunXi 的 Linux 原生产品线。
