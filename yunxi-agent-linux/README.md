# YunXi Native · Linux Host

这是 YunXi Native 的 Linux 原生宿主。Arch Linux 是第一目标环境；它复用共享 Runtime 的人格、灵魂、记忆、Provider、工具审批和本地会话存储，提供两种终端入口：

1. 独立的 TUI 对话界面；
2. 参考 [Miyu Agent](https://github.com/SHORiN-KiWATA/miyu-agent) 的 fish 接管模式：每个非空提交都先交给 YunXi，再由 Runtime 决定执行路径。

fish 接管会启动一个每用户、按需拉起的 Unix socket daemon。daemon 在第一次自然语言输入时启动，随后持续运行并复用会话，不需要 root，也不把输入发送到云端 shell。

## 产品定位

Linux 版不是 Windows/Web 版的裁剪包，而是“自然语言 → Linux 意图”的翻译层：fish/TUI 接收输入，YunXi 负责理解、规划、审批和执行，fish 保留编辑体验，Runtime 保留最终执行策略。完整设计哲学见仓库根目录 [`README.md`](../README.md)。

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
- Linux 只读主机工具：systemd 状态、man 页面、进程快照和网络状态；
- Miyu 风格的 fish 接管与按需常驻 daemon。

这些能力通过同一个 `yunxi-agent-runtime` 进入 TUI 或 fish daemon，不会为 Linux 复制一套人格、记忆或会话逻辑。TUI 中输入 `/capabilities` 可以查看同一份边界摘要。

完整的 Miyu/YunXi 能力差距与迁移边界见 [`CAPABILITY-MATRIX.md`](./CAPABILITY-MATRIX.md)。
Miyu 参考实现的分层适配与裁决见 [`MIYU-ADAPTER-ROADMAP.md`](./MIYU-ADAPTER-ROADMAP.md)，源码许可与来源记录见 [`MIYU-LICENSE-NOTICE.md`](./MIYU-LICENSE-NOTICE.md)。
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

## Linux 只读工具层（Phase 3 起点）

自然语言请求可以由模型路由到固定的 `linux_readonly` ToolSpec。它只允许五种操作：

- `systemd_status`：读取 user 或 system manager 的 unit 状态；
- `man_page`：以 `MANPAGER=cat` 读取单个本地手册主题；
- `process_list`：读取有界进程快照；
- `network_snapshot`：读取本机接口、路由或 socket 状态。
- `pacman_info` / `pacman_search`：只读查询已安装包元数据或搜索 Arch 包数据库。

这些操作不接受任意命令、路径或 shell 片段，不执行启动/停止服务、杀进程、网络配置、HTTP 探测、包安装/删除/升级、数据库刷新或写文件。实际执行使用固定 argv 的直接进程 runner，经过 YunXi 现有的 ToolPolicy、审批、沙盒诊断与审计事件；缺少 `systemctl`、`man`、`ps`、`ip`、`ss` 或 `pacman` 时返回结构化 `unavailable`。

也可以直接检查 CLI 探针（用于安装和发行版诊断）：

```bash
yunxi-linux linux-tool describe
yunxi-linux linux-tool processes --limit 20
yunxi-linux linux-tool network
yunxi-linux linux-tool systemd-status --unit yunxi-linux.service
yunxi-linux linux-tool man fish
yunxi-linux linux-tool pacman --info fish
yunxi-linux linux-tool pacman --search terminal
```

CLI 探针与模型可见的 `linux_readonly` ToolSpec 共用同一只读边界，但 CLI 输出是诊断入口，不替代 Runtime 的审批链路。

真实 Linux 验收可运行：

```bash
bash yunxi-agent-linux/tests/linux_tool_smoke.sh ./target/release/yunxi-linux
```

该 smoke 会校验五个 ToolSpec 的 JSON 契约、64 KiB 输出边界和参数注入拒绝；目标系统
缺少 `systemctl`、`man`、`ip`、`ss` 或 `pacman` 时允许结果为结构化 `unavailable`，不会把缺少
发行版工具误判为测试失败。

## 显式 project/private 知识空间

Linux 知识库现在支持由用户显式创建的 `project` 与 `private` 空间。它们不会自动扫描
用户目录，也不会默认进入 Linux Planner；只有通过 stdin 或明确指定的单个文件导入才会入库。空间
的 owner、visibility、source 和 version 是访问与 provenance 边界，重复初始化必须完全
匹配，否则命令会拒绝静默覆盖。

```bash
./target/release/yunxi-linux knowledge-space-init \
  --space-id project-demo --kind project --visibility owner \
  --owner local-user --source project-notes --version v1 --cwd .

printf '%s\n' '项目约定：先 dry-run，再申请审批。' | \
  ./target/release/yunxi-linux knowledge-import-stdin \
  --space-id project-demo --document-id project-guide --title 'Project Guide' \
  --source project-notes --version v1 --owner local-user --visibility owner --cwd .

# 单个 UTF-8 文本/Markdown 文件；相对 --cwd 解析，不递归扫描目录
./target/release/yunxi-linux knowledge-import-file docs/project-guide.md \
  --space-id project-demo --document-id project-guide-file --title 'Project Guide (file)' \
  --source project-notes --version v1 --owner local-user --visibility owner --cwd .

./target/release/yunxi-linux knowledge-search 'dry-run' \
  --space-id project-demo --owner local-user --visibility owner --cwd .
./target/release/yunxi-linux knowledge-space-list --cwd .
./target/release/yunxi-linux knowledge-worker --max-jobs 10 --cwd .
./target/release/yunxi-linux knowledge-worker --watch --interval-secs 5 --max-jobs 10 --cwd .
# 只读查看 active/staging 队列，不领取任务也不触发重试
./target/release/yunxi-linux knowledge-worker-status --cwd .
# 多工作区：重复 --workspace，最多 32 个；不扫描其他目录
./target/release/yunxi-linux knowledge-worker --max-jobs 4 \
  --workspace ~/src/project-a --workspace ~/src/project-b
./target/release/yunxi-linux knowledge-worker --watch --interval-secs 5 --max-jobs 4 \
  --workspace ~/src/project-a --workspace ~/src/project-b
./target/release/yunxi-linux knowledge-vector-search '审批' \
  --space-id project-demo --owner local-user --visibility owner --cwd .

./target/release/yunxi-linux knowledge-retract project-guide \
  --space-id project-demo --owner local-user --visibility owner --cwd .
```

`project` 首版只允许 `owner` visibility；`private` 允许 `owner` 或 `private`。stdin 导入
和文件导入都受默认输入上限与 chunking 约束；文件导入只接受工作区内的普通 UTF-8
文本/Markdown 文件，拒绝工作区外路径、`.yunxi` 状态目录和超过 8 MiB 的文件。文档必须
与空间的 source/version 一致；重复 document id
会在事务内替换旧 chunks、向量和 embedding job，不留下孤立索引。长期记忆数据库与
`knowledge.sqlite3` 始终保持物理分离。`knowledge-retract` 也要求显式匹配 space、owner
和 visibility，撤回后 FTS、向量和 embedding job 一起失效。

`knowledge-worker --watch` 是显式 workspace 范围内的常驻轮询器：它复用同一套
lease、退避、重试和 generation 校验，按间隔处理有限数量任务；不会扫描其他
workspace，也不会自动激活 generation。除原有的 `--cwd` 单 workspace 模式外，
还可以重复传入 `--workspace` 建立一个最多 32 个工作区的显式 fleet。fleet 会先对路径
做 canonicalize 和去重，再用跨轮 round-robin 游标调度；`--max-jobs` 是整轮所有工作区
共享的预算。单个 SQLite 损坏、权限或索引失败只会标记该工作区并继续处理其他工作区，
JSON 默认只返回 `workspace_index`，不泄露绝对路径。按 `Ctrl+C` 或 `SIGTERM` 停止时
会输出结构化 stopped 记录。fleet 仍是显式 CLI 边界，不会自行发现新目录，也不会替代
未来需要告警和统一策略的 daemon 调度器。其 round-robin 游标与累计统计写入
`$XDG_STATE_HOME/yunxi/knowledge-worker/`（未设置时为 `~/.local/state/yunxi/knowledge-worker/`）
的独立状态文件；文件原子替换且用户私有，损坏或版本不兼容时只重置游标并返回
结构化 warning，不接触长期记忆或 `knowledge.sqlite3`。

`knowledge-worker-status` 是独立的只读诊断入口：它不会创建缺失的知识数据库，不会初始化
或迁移旧 schema，不会领取任务、回收 lease、提升 retry 或读取长期记忆。输出同时给出
active/staging 队列的计数和同一时钟快照；`status` 为 `idle`、`ready`、`complete` 或
`degraded`，只用于观察，不替代 worker 或 daemon 调度器。

也可以重复传入 `--workspace` 获取最多 32 个明确工作区的 fleet 快照。路径会先
canonicalize 后去重，结果只返回 `workspace_index`、队列计数和脱敏错误码；缺失数据库
返回 `database_present=false`，损坏数据库只影响对应项。该模式不创建目录、不改变
lease/retry/generation，也不触碰长期记忆。真实验收脚本为：

```bash
python3 yunxi-agent-linux/tests/knowledge_worker_status_fleet_smoke.py \
  ./target/release/yunxi-linux
```

候选 generation 的帮助目录可以用批量入口显式收集：先运行
`knowledge-generation-begin`，再运行
`knowledge-stage-catalog --generation <N> --source-version ubuntu-24.04 --cwd .`。
不传 `--command` 时只使用代码中固定的 P0 allowlist；也可以重复传入
`--command cat --command git` 选择子集。命令不会扫描 PATH、不会自动创建或激活
generation、不会启动 embedding worker，也不会写入长期记忆。输出是一个结构化 JSON，
其中每个项目都标记 `ok`、`unavailable` 或 `failed`；不可用/失败项目不会写入 staging。

候选 generation 的 staging → embedding → seal → activate 黑盒闭环也可单独验收：
激活前 FTS/向量查询不可见，未 ready 的代际不能切换，激活后两类索引同时可见，
并确认不会改写长期记忆向量库。

```bash
bash yunxi-agent-linux/tests/knowledge_generation_smoke.sh ./target/release/yunxi-linux
```

真实 Linux P0 帮助/man 目录的多文档验收会同时检查风险 provenance、精确版本过滤、
FTS/向量代际一致性和 `man` 来源：

```bash
bash yunxi-agent-linux/tests/knowledge_catalog_smoke.sh ./target/release/yunxi-linux
```

`knowledge-space-list` 只列出空间元数据，不读取文档正文、chunk 或向量；它用于确认
当前 workspace 的 system/project/private 边界，输出按 `space_id` 稳定排序。

## fish 全量接管（Miyu 风格）

安装 fish hook：

```bash
./target/release/yunxi-linux fish-init
source ~/.config/fish/conf.d/yunxi.fish
```

之后在 fish 中，所有非空提交（包括 `ls -la`、`cd /tmp`、`git status`、自然语言、多行
内容和 fish 语法）都先交给 YunXi；是否执行命令由 Runtime 的工具审批、Sandbox 和工作区
策略决定。fish 只保留行编辑、历史、补全、空输入和编辑态取消。

- 不再存在 fish 的保守分类模式；hook 不调用 `shell-classify`，也不在本地执行提交内容；
- 工具调用仍会弹出审批，不会因为通过 shell 接管而自动放行；
- 输入编辑态的 `Ctrl+C` 由 fish 本地取消；已送入 YunXi 的回合会向 daemon 发送
  `Cancel`，等待 `cancelled` 终态后返回，关闭 fish 后 daemon 也不会继续接收新输入。
- YunXi 正在等待工具审批或补充输入时，交互终端的 `Ctrl+C` 同样会发送当前回合的
  `Cancel`；输入采用可轮询的 `/dev/tty`，取消时先回收读取任务，不会遗留后台线程吞掉
  下一条 fish 输入。真实 PTY 验收脚本为
  `tests/shell_prompt_cancel_smoke.sh`。

`--takeover` 仍被接受为旧脚本的兼容参数，但没有第二种模式。每个交互式 fish 进程
携带独立的 `fish-<pid>` session id，因此两个终端即使位于同一目录，也不会误用同一个
YunXi Runtime 会话；手动调用 `shell-intercept` 时仍可用 `YUNXI_SHELL_SESSION` 提供兼容
session id。真实 fish + PTY smoke 已覆盖 shell-looking 输入、中文自然语言、Ctrl+J、多行、
命令替换、重定向、管道、窗口 resize 和输入态 Ctrl+C；`tests/shell_prompt_cancel_smoke.sh`
另行覆盖审批等待态取消；复杂嵌套命令和提示符重绘矩阵仍未完成，不能把这段设计说明当成
已验收的全部行为保证。

真实 PTY 回归可运行：

```bash
bash yunxi-agent-linux/tests/fish_pty_smoke.sh ./target/release/yunxi-linux
# 旧脚本的兼容参数不会改变行为
bash yunxi-agent-linux/tests/fish_pty_smoke.sh ./target/release/yunxi-linux --takeover
bash yunxi-agent-linux/tests/shell_prompt_cancel_smoke.sh ./target/release/yunxi-linux
```

卸载：

```bash
./target/release/yunxi-linux remove-shell-hook
```

调试 hook 而不写文件：

```bash
./target/release/yunxi-linux fish-init --print
./target/release/yunxi-linux fish-init --print --takeover
```

`--takeover` 只是旧脚本兼容参数，两个 `fish-init --print` 输出相同的全量接管 hook。
历史遗留的 `shell-classify` CLI 仅作为离线诊断保留，不被 fish hook 调用。daemon socket
优先放在 `$XDG_RUNTIME_DIR/yunxi/yunxi.sock`，否则放在 `$XDG_STATE_HOME/yunxi/run/yunxi.sock`，
目录为 0700、socket 为 0600。daemon 只允许当前用户通过本地 socket 访问。

IPC 已提供有界的回合回放：`Turn` 会先返回 `run_accepted`，随后可见输出以 `event(run_id, seq, frame)` 发送；客户端重连后使用 `follow(run_id, after_seq)` 获取缺失事件。`Turn` 的 `delivery` 默认是 `attached`（兼容旧客户端，连接断开会取消并丢弃回合）；显式指定 `detached_output_only` 后，daemon 会把运行与 Unix socket 解耦，客户端可以断开，再用 active Follow 接收 backlog+live 事件直到 `Done`。detached 模式只发布 `Thread`、`Message`、`Done`，审批和 user-input 请求会在 runtime 内明确拒绝，不发送可恢复 prompt。回放按回合数、事件数和事件总字节数限制；未知 run、过期游标或 active Follow lag 会返回结构化 `resync_required`。detached cancel 不在本切片范围内。

在进入 Runtime 之前，daemon 还会对 `Turn` 的语义字段做独立上限校验，避免合法的大 frame
被当作无限大的提示词或路径继续处理：`prompt` ≤ 64 KiB、`cwd` ≤ 4 KiB、`request_id`
和 `session_id` ≤ 512 字节、`provider` 和 `model` ≤ 256 字节。超限请求只返回结构化
`Error`，不会先发送 `run_accepted`，也不会创建回合；这与 24 MiB 的传输 frame 上限是两层
不同的边界。

Unix socket 连接必须在 5 秒内完成 `Hello` 和首个请求；空闲或半连接不会无限占用 daemon
的连接任务。超时只关闭该连接，不影响其他客户端和 daemon 主循环。

### systemd --user（可选）

unit 模板位于 `packaging/systemd/yunxi-linux.service`，也可以由 CLI 输出：

```bash
install -Dm755 target/release/yunxi-linux ~/.local/bin/yunxi-linux
mkdir -p ~/.config/systemd/user
yunxi-linux systemd-unit > ~/.config/systemd/user/yunxi-linux.service
systemctl --user daemon-reload
systemctl --user enable --now yunxi-linux.service
```

没有 systemd 的环境不受影响，继续使用手动 daemon 或 fish hook 的按需启动。

## 数据位置

人格、灵魂和全局设置默认位于 `$XDG_DATA_HOME/yunxi`，没有设置时使用 `$HOME/.local/share/yunxi`；工作区会话与工作区记忆位于 `<workspace>/.yunxi`。状态与缓存分别使用 `$XDG_STATE_HOME/yunxi`、`$XDG_CACHE_HOME/yunxi`，没有设置时回退到 `$HOME/.local/state/yunxi`、`$HOME/.cache/yunxi`。可通过 `YUNXI_HOME` 改变全局目录。

程序会创建并限制这些目录为 0700。daemon lock metadata 会先写入临时文件并同步，再以不可覆盖的硬链接抢占最终路径；读取到空/损坏 metadata 时会拒绝删除，避免并发启动误删活动锁。不要把 `.yunxi`、XDG 状态目录或 daemon socket 提交到 Git。

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

可执行真实 PTY smoke（需要 `fish`、`python3` 和 release binary）：

```bash
bash yunxi-agent-linux/tests/fish_pty_smoke.sh ./target/release/yunxi-linux
```

可执行真实 Unix socket daemon smoke（不需要模型凭据，不会执行真实工具）：

```bash
bash yunxi-agent-linux/tests/daemon_ipc_smoke.sh ./target/release/yunxi-linux
```

它会启动真实 daemon，验证版本握手、Ping、未知回合的 Follow 重同步、回合失败与超限
`Turn` 的结构化 `Error` 帧（超限请求不会产生 `run_accepted`）、错误后 daemon 仍可 Ping，
空闲握手连接的超时关闭，以及 SIGTERM 后 socket 清理。脚本使用临时 XDG 目录，结束后会
自动删除测试状态；同时会发送超大 frame 和截断 JSON，确认坏连接只被丢弃而不会拖垮
daemon 或影响后续 Ping。脚本还会真实 `SIGKILL` 一个 daemon，再并发启动多个候选进程，
确认陈旧 socket/lock 可回收、最终只有一个 daemon 持有 lock 并响应 Ping，随后能正常清理。
它还会用离线静态 Runtime 完成一个真实回合，断开客户端后按 `run_id` 从游标 0 回放，
逐帧校验事件顺序和 `Done` 终止帧。

project/private 知识空间的 stdin 导入、owner/visibility 隔离、重复导入和向量闭环可用：

```bash
bash yunxi-agent-linux/tests/knowledge_project_private_smoke.sh ./target/release/yunxi-linux
```

知识查询延迟可用同一套临时知识库测量（输出冷查询与后续 warm-ish 查询的
p50/p95，不设置跨机器硬阈值）：

```bash
bash yunxi-agent-linux/tests/knowledge_latency_smoke.sh ./target/release/yunxi-linux
```

Linux 宿主的进程启动、daemon socket 就绪和常驻 RSS 可用性能基线脚本测量。脚本只
输出 JSON，不设置跨机器硬阈值；它会启动并清理临时用户 daemon，不读写真实的
`~/.yunxi` 数据：

```bash
bash yunxi-agent-linux/tests/performance_smoke.sh ./target/release/yunxi-linux
```

命令帮助采集器的完整 allowlist、固定 argv 和缺失工具回退可用真实 Linux smoke 验证：

```bash
bash yunxi-agent-linux/tests/knowledge_help_smoke.sh ./target/release/yunxi-linux
```

手册页采集器的固定 `man --locale=C -P cat` argv、版本 provenance 和非法 topic/section
拒绝也可单独验收：

```bash
bash yunxi-agent-linux/tests/knowledge_man_smoke.sh ./target/release/yunxi-linux
```

Linux 发行构建只使用本子项目的 `yunxi-linux` 二进制；Windows/Web/语音/微信参考源码位于仓库的 `references/`，不作为依赖构建。
