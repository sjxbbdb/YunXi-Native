# YunXi Agent · Arch Linux TUI

这是 YunXi Agent 的 Arch Linux 专用 TUI 子项目。它复用主仓库的文本 Runtime、人格、记忆、Provider、工具审批和本地会话存储，但只编译一个终端交互程序。

## 明确不包含

- Web 服务和浏览器界面
- 麦克风、扬声器和任何语音模型链路
- 微信/iLink 网关、二维码登录和微信凭证存储
- Windows Credential Manager、PowerShell 安装脚本和 Windows 任务调度

因此它不会拉入 `yunxi-agent-cli`、`yunxi-agent-voice` 或 `yunxi-agent-weixin` 作为依赖，也不会生成 Windows 专属功能。

## Arch Linux 前置条件

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
cargo build --release -p yunxi-agent-archlinux
./target/release/yunxi-archlinux --cwd "$PWD"
```

也可以从任意目录用绝对路径启动：

```bash
/path/to/YunXi-Agent/target/release/yunxi-archlinux --cwd /path/to/workspace
```

在线模式会在检测到凭证后自动启用；强制在线或离线：

```bash
./target/release/yunxi-archlinux --live
./target/release/yunxi-archlinux --offline
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

## 数据位置

人格、记忆和全局设置默认位于 `$HOME/.yunxi`；工作区会话与工作区记忆位于 `<workspace>/.yunxi`。可通过 `YUNXI_HOME` 改变全局目录。

## 验收

```bash
cargo fmt --all --check
cargo test -p yunxi-agent-archlinux
cargo build --release -p yunxi-agent-archlinux
```

完整主仓库仍包含其他平台和入口；Arch 发行构建只使用本子项目的 `yunxi-archlinux` 二进制。
