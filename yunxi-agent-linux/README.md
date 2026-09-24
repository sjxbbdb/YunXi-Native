# YunXi Native · Linux Host

这是 YunXi Native 的 Linux 原生宿主。Arch Linux 是第一目标环境；它复用共享 Runtime 的人格、灵魂、记忆、Provider、工具审批和本地会话存储，提供两种终端入口：

1. 独立的 TUI 对话界面；
2. 参考 [Miyu Agent](https://github.com/SHORiN-KiWATA/miyu-agent) 的 fish 接管模式：普通命令仍由 fish 执行，自然语言直接交给 YunXi。

fish 接管会启动一个每用户、按需拉起的 Unix socket daemon。daemon 在第一次自然语言输入时启动，随后持续运行并复用会话，不需要 root，也不把输入发送到云端 shell。

## 产品定位

Linux 版不是 Windows/Web 版的裁剪包，而是“自然语言 → Linux 意图”的翻译层：fish/TUI 接收输入，YunXi 负责理解、规划、审批和执行，Shell 仍然保留最终语义。完整设计哲学见仓库根目录 [`README.md`](../README.md)。

## 当前边界

- Web 服务和浏览器界面
- 麦克风、扬声器和任何语音模型链路
- 微信/iLink 网关、二维码登录和微信凭证存储
- Windows Credential Manager、PowerShell 安装脚本和 Windows 任务调度

因此它不会拉入 `yunxi-agent-cli`、`yunxi-agent-voice` 或 `yunxi-agent-weixin` 作为依赖，也不会生成 Windows 专属功能。fish 接管目前只实现 fish，不会悄悄修改 zsh/bash。

## 已打包的跨平台能力

Linux 版不是“只有一个聊天框”的裁剪版。共享 Runtime 中与 Linux 兼容的能力均保留：

- 对话、会话恢复、上下文压缩和本地 rollout；
- 人格、灵魂、语气、边界、称呼与关系阶段；
- 短期会话状态、长期记忆、候选审批、失效链和本地 SQLite 向量召回；
- 陪伴策略、情绪线索、主动关怀、情书任务与本地信箱；
- Provider 自动选择、离线回退、工具路由、Shell、补丁、Sandbox、MCP、Skills 和多 Agent；
- 工具审批、用户输入、取消、诊断事件和 XDG 本地存储；
- Miyu 风格的 fish 接管与按需常驻 daemon。

这些能力通过同一个 `yunxi-agent-runtime` 进入 TUI 或 fish daemon，不会为 Linux 复制一套人格、记忆或会话逻辑。TUI 中输入 `/capabilities` 可以查看同一份边界摘要。

完整的 Miyu/YunXi 能力差距与迁移边界见 [`CAPABILITY-MATRIX.md`](./CAPABILITY-MATRIX.md)。
大融合的分层迁移与冲突裁决见 [`MIYU-FUSION-PLAN.md`](./MIYU-FUSION-PLAN.md)，源码许可与来源记录见 [`MIYU-LICENSE-NOTICE.md`](./MIYU-LICENSE-NOTICE.md)。
在继续迁移前，必须先阅读 [`MIYU-SOURCE-AUDIT.md`](./MIYU-SOURCE-AUDIT.md)；该报告记录固定版本、全文件清单、关键路径风险和验收门。当前 fish/daemon 仍是实验实现，不代表已经达到 Miyu 的完整行为等价。

## Linux 前置条件

```bash
sudo pacman -S --needed base-devel git rustup
rustup default stable
```

如果要调用在线 Provider，请在当前 shell 配置凭证，例如：

```bash
export DEEPSEEK_API_KEY='你的密钥'
```

没有凭证时程序会自动进入离线 Runtime；也可以显式使用 `--offline`。

## 构建与启动

在主仓库根目录执行：

```bash
cargo build --release -p yunxi-agent-linux
./target/release/yunxi-linux --cwd "$PWD"
```

也可以从任意目录用绝对路径启动：

```bash
/path/to/YunXi-Native/target/release/yunxi-linux --cwd /path/to/workspace
```

在线模式会在检测到凭证后自动启用；强制在线或离线：

```bash
./target/release/yunxi-linux --live
./target/release/yunxi-linux --offline
```

常用参数：

```text
--cwd <PATH>       工作区路径，默认当前目录
--provider <NAME>  Provider profile，例如 deepseek
--model <NAME>     覆盖模型名
--live             强制要求在线凭证
--offline          强制离线 Runtime
```

TUI 内置命令：`/help`、`/clear`、`/status`、`/exit`。工具调用仍遵循 Runtime 的审批策略；按 `Ctrl+C` 可取消当前回合。

## fish 接管（Miyu 风格）

安装 fish hook：

```bash
./target/release/yunxi-linux fish-init
source ~/.config/fish/conf.d/yunxi.fish
```

之后在 fish 中：

- `ls -la`、`cd /tmp`、`git status` 等可识别的命令保持原有 fish 行为；
- `帮我找一下最近修改的 Rust 文件`、`解释一下这个错误` 等自然语言由 YunXi 处理；
- 多行输入和 fish 语法结构会先经过保守分类，无法确定时不执行；
- 工具调用仍会弹出审批，不会因为通过 shell 接管而自动放行；
- `Ctrl+C` 仍由当前 fish/终端负责，关闭 fish 后 daemon 不会继续接收新输入。

hook 的设计目标参考 Miyu：回车时使用 `commandline --tokens-raw` 读取首词，尽量避免在分类阶段触发命令替换、通配符或其他副作用；真正的命令交回 fish，自然语言才送入 `shell-intercept`。`fish_command_not_found` 是第二道兜底。当前实验 hook 尚未完成 `type -q`、复杂多行/嵌套命令、提示符重绘和真实 PTY 回归，不能把这段设计说明当成已验收的行为保证。

卸载：

```bash
./target/release/yunxi-linux remove-shell-hook
```

调试 hook 而不写文件：

```bash
./target/release/yunxi-linux fish-init --print
printf '%s' '解释一下 Cargo.lock' | ./target/release/yunxi-linux shell-classify --shell fish --stdin
```

`shell-classify` 返回码为 0 表示交给 fish，1 表示交给 YunXi。daemon socket 优先放在 `$XDG_RUNTIME_DIR/yunxi/yunxi.sock`，否则放在 `$XDG_STATE_HOME/yunxi/run/yunxi.sock`，目录为 0700、socket 为 0600。daemon 只允许当前用户通过本地 socket 访问。

## 数据位置

人格、灵魂和全局设置默认位于 `$XDG_DATA_HOME/yunxi`，没有设置时使用 `$HOME/.local/share/yunxi`；工作区会话与工作区记忆位于 `<workspace>/.yunxi`。状态与缓存分别使用 `$XDG_STATE_HOME/yunxi`、`$XDG_CACHE_HOME/yunxi`，没有设置时回退到 `$HOME/.local/state/yunxi`、`$HOME/.cache/yunxi`。可通过 `YUNXI_HOME` 改变全局目录。

程序会创建并限制这些目录为 0700。不要把 `.yunxi`、XDG 状态目录或 daemon socket 提交到 Git。

## 验收

```bash
cargo fmt --all --check
cargo test -p yunxi-agent-linux
cargo build --release -p yunxi-agent-linux
```

在 Linux 上还应使用 fish 自己检查 hook 语法（如果系统安装了 fish）：

```bash
fish -n ~/.config/fish/conf.d/yunxi.fish
```

Linux 发行构建只使用本子项目的 `yunxi-linux` 二进制；Windows/Web/语音/微信参考源码位于仓库的 `references/`，不作为依赖构建。
