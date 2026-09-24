//! Miyu-inspired fish integration for the YunXi Linux native host.
//!
//! The hook deliberately makes one conservative decision before fish expands a
//! command line: a real command remains fish's responsibility, while prose is
//! handed to the YunXi daemon.  This is the important boundary; the hook never
//! evaluates command substitutions or globs merely to classify a line.
//!
//! The classification and fallback behavior are adapted from Miyu Agent's
//! `crates/miyu-base/src/shell/fish.rs` (MIT License).  This module routes the
//! accepted prose into YunXi Runtime instead of Miyu's own engine.

use anyhow::{Context, Result, bail};
use clap::Subcommand;
#[cfg(unix)]
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::io::Write;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::io::AsyncWriteExt;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use tokio::sync::{Mutex, mpsc};
#[cfg(unix)]
use tokio::time::sleep;
#[cfg(unix)]
use yunxi_agent_core::{
    Agent, AgentConfig, AgentEvent, AgentInput, AgentRunApprovalDecision, AgentRunControl,
    AgentRunStatus, AgentRunUserInputResponse,
};
#[cfg(unix)]
use yunxi_agent_runtime::YunXiRuntimeBackend;

const HOOK_MARKER: &str = "# YunXi Agent fish hook";

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum LinuxShellCommand {
    /// Install the Miyu-style fish Enter hook.
    FishInit {
        /// Print the hook instead of writing it.
        #[arg(long)]
        print: bool,
    },
    /// Remove a hook previously installed by `fish-init`.
    RemoveShellHook,
    /// Return exit 0 for a real shell command and exit 1 for prose.
    ShellClassify {
        #[arg(long, default_value = "fish")]
        shell: String,
        #[arg(long)]
        stdin: bool,
    },
    /// Route one line of prose through the resident daemon.
    ShellIntercept {
        #[arg(long, default_value = "fish")]
        shell: String,
        #[arg(long)]
        stdin: bool,
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        live: bool,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Hidden long-lived process used by shell-intercept.
    #[command(hide = true)]
    Daemon,
}

pub(crate) async fn run_command(command: LinuxShellCommand) -> Result<()> {
    match command {
        LinuxShellCommand::FishInit { print } => install_fish_hook(print),
        LinuxShellCommand::RemoveShellHook => remove_fish_hook(),
        LinuxShellCommand::ShellClassify { shell, stdin } => {
            let input = read_shell_input(stdin)?;
            if is_shell_command(&input, &shell) {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        LinuxShellCommand::ShellIntercept {
            shell,
            stdin,
            cwd,
            offline,
            live,
            provider,
            model,
        } => {
            let input = read_shell_input(stdin)?;
            run_shell_intercept(shell, cwd, input, offline, live, provider, model).await
        }
        LinuxShellCommand::Daemon => run_daemon().await,
    }
}

fn read_shell_input(stdin: bool) -> Result<String> {
    if stdin {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .context("读取 shell 标准输入失败")?;
        return Ok(input.trim_end_matches(['\r', '\n']).to_string());
    }
    bail!("shell 命令必须使用 --stdin；这样可以避免参数重新解析和命令替换")
}

fn install_fish_hook(print: bool) -> Result<()> {
    let binary = std::env::var_os("YUNXI_LINUX_BINARY")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .context("无法定位 yunxi-linux 可执行文件")?;
    let hook = fish_hook(&binary);
    if print {
        print!("{hook}");
        return Ok(());
    }

    let path = fish_hook_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建 fish 配置目录失败: {}", parent.display()))?;
    }
    if path.exists() {
        let existing = fs::read_to_string(&path).unwrap_or_default();
        if !existing.contains(HOOK_MARKER) {
            bail!(
                "拒绝覆盖非 YunXi 文件: {}；请先备份/移动它，或手动合并 fish hook",
                path.display()
            );
        }
    }
    fs::write(&path, hook).with_context(|| format!("写入 fish hook 失败: {}", path.display()))?;
    println!("已安装 fish hook: {}", path.display());
    println!("重启 fish，或执行: source {}", path.display());
    Ok(())
}

fn remove_fish_hook() -> Result<()> {
    let path = fish_hook_path()?;
    if !path.exists() {
        println!("fish hook 不存在: {}", path.display());
        return Ok(());
    }
    let existing = fs::read_to_string(&path).unwrap_or_default();
    if !existing.contains(HOOK_MARKER) {
        bail!("{} 不是 YunXi 生成的 hook，未删除", path.display());
    }
    fs::remove_file(&path).with_context(|| format!("删除 fish hook 失败: {}", path.display()))?;
    println!("已删除 fish hook: {}", path.display());
    Ok(())
}

fn fish_hook_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("Linux fish 集成需要 HOME")?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    Ok(config.join("fish").join("conf.d").join("yunxi.fish"))
}

fn fish_quote_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

fn fish_hook(binary: &Path) -> String {
    let binary = fish_quote_path(binary);
    format!(
        r#"{HOOK_MARKER}
# 由 `yunxi-linux fish-init` 生成；普通 shell 命令始终交回 fish。
set -g __yunxi_binary "{binary}"

function __yunxi_head_is_plain_word
    test -n "$argv[1]"; or return 1
    string match -qr '[\x27"$()~{{}}%;&|<>#^!\\\s]' -- "$argv[1]"; and return 1
    return 0
end

function __yunxi_first_token_raw
    set -l tokens (commandline --input="$argv[1]" --tokens-raw 2>/dev/null)
    while test (count $tokens) -gt 0
        set -l token $tokens[1]
        if string match -qr '^[A-Za-z_][A-Za-z0-9_]*=' -- "$token"
            set -e tokens[1]
            continue
        end
        printf '%s' "$token"
        return 0
    end
    return 1
end

function __yunxi_buffer_is_multiline
    test (string split \n -- "$argv[1]" | count) -gt 1
end

function __yunxi_hand_to_ai
    set -g __yunxi_pending_buffer "$argv[1]"
    commandline -b -- ""
    commandline -f execute
end

function __yunxi_execute_or_continue
    commandline --is-valid
    set -l valid_status $status
    if test $valid_status -eq 2
        commandline -i \n
        commandline -f repaint
    else
        commandline -f execute
    end
end

function __yunxi_insert_newline
    commandline -f expand-abbr
    commandline -i \n
end

function __yunxi_accept_line
    status is-interactive; or return
    commandline -f expand-abbr
    set -l buffer (commandline -b | string collect)
    set -l trimmed (string trim -- "$buffer")
    if test -z "$trimmed"
        __yunxi_execute_or_continue
        return
    end
    if not __yunxi_buffer_is_multiline "$buffer"
        set -l head (__yunxi_first_token_raw "$buffer")
        if __yunxi_head_is_plain_word "$head"
            printf '%s' "$buffer" | "$__yunxi_binary" shell-classify --shell fish --stdin >/dev/null 2>/dev/null
            if test $status -eq 1
                __yunxi_hand_to_ai "$buffer"
                return
            end
        end
        __yunxi_execute_or_continue
        return
    end
    printf '%s' "$buffer" | "$__yunxi_binary" shell-classify --shell fish --stdin >/dev/null 2>/dev/null
    if test $status -eq 1
        __yunxi_hand_to_ai "$buffer"
    else
        __yunxi_execute_or_continue
    end
end

bind enter __yunxi_accept_line
bind \r __yunxi_accept_line
bind -M insert enter __yunxi_accept_line
bind -M insert \r __yunxi_accept_line
bind ctrl-j __yunxi_insert_newline
bind \cj __yunxi_insert_newline
bind -M insert ctrl-j __yunxi_insert_newline
bind -M insert \cj __yunxi_insert_newline

function __yunxi_on_prompt --on-event fish_prompt
    set -q __yunxi_pending_buffer; or return
    set -l buffer $__yunxi_pending_buffer
    set -e __yunxi_pending_buffer
    printf '\n'
    printf '%s' "$buffer" | "$__yunxi_binary" shell-intercept --shell fish --cwd "$PWD" --stdin
end

function fish_command_not_found
    status is-interactive; or return 127
    set -l current_line (status current-commandline 2>/dev/null | string collect)
    if test -n "$current_line"
        printf '\n'
        printf '%s' "$current_line" | "$__yunxi_binary" shell-intercept --shell fish --cwd "$PWD" --stdin
        return 127
    end
    return 127
end
"#
    )
}

/// Conservative classifier modelled on Miyu's first-token decision.
pub(crate) fn is_shell_command(input: &str, _shell: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return true;
    }
    let first_line = trimmed.lines().next().unwrap_or_default().trim_start();
    if first_line.starts_with('#') {
        return true;
    }
    let tokens = raw_tokens(trimmed);
    let Some(mut head) = tokens.into_iter().next() else {
        return true;
    };
    let mut index = 1usize;
    let all_tokens = raw_tokens(trimmed);
    while is_assignment(&head) {
        if index >= all_tokens.len() {
            return true;
        }
        head = all_tokens[index].clone();
        index += 1;
    }
    if head.is_empty() || has_shell_syntax(&head) {
        return false;
    }
    let ambiguous = [
        "time", "test", "date", "which", "type", "command", "history",
    ];
    if ambiguous.contains(&head.as_str())
        && trimmed
            .chars()
            .any(|c| c == '?' || ('\u{4e00}'..='\u{9fff}').contains(&c))
    {
        return false;
    }
    if fish_builtin(&head) || executable_token(&head) {
        return true;
    }
    false
}

fn raw_tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            current.push(ch);
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().enumerate().all(|(i, c)| {
            c == '_' || c.is_ascii_alphanumeric() && (i > 0 || c.is_ascii_alphabetic())
        })
}

fn has_shell_syntax(token: &str) -> bool {
    token.chars().any(|ch| {
        matches!(
            ch,
            '$' | '('
                | ')'
                | '~'
                | '{'
                | '}'
                | '%'
                | ';'
                | '&'
                | '|'
                | '<'
                | '>'
                | '#'
                | '^'
                | '!'
                | '\\'
        )
    })
}

fn fish_builtin(token: &str) -> bool {
    matches!(
        token,
        "alias"
            | "and"
            | "argparse"
            | "abbr"
            | "begin"
            | "bind"
            | "break"
            | "builtin"
            | "case"
            | "cd"
            | "command"
            | "commandline"
            | "contains"
            | "continue"
            | "dirh"
            | "dirs"
            | "disown"
            | "echo"
            | "else"
            | "end"
            | "eval"
            | "exec"
            | "exit"
            | "false"
            | "fg"
            | "fish"
            | "for"
            | "function"
            | "functions"
            | "history"
            | "if"
            | "jobs"
            | "kill"
            | "math"
            | "not"
            | "printf"
            | "pwd"
            | "random"
            | "read"
            | "realpath"
            | "return"
            | "set"
            | "source"
            | "status"
            | "string"
            | "suspend"
            | "switch"
            | "test"
            | "time"
            | "true"
            | "type"
            | "ulimit"
            | "umask"
            | "vared"
            | "wait"
            | "while"
    )
}

fn executable_token(token: &str) -> bool {
    let path = Path::new(token);
    if path.components().count() > 1 {
        return is_executable(path);
    }
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| is_executable(&dir.join(token)))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o111 != 0;
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ClientFrame {
    Ping,
    Turn {
        cwd: String,
        prompt: String,
        session_id: Option<String>,
        offline: bool,
        live: bool,
        provider: Option<String>,
        model: Option<String>,
    },
    ApprovalResponse {
        id: Option<String>,
        approved: bool,
        reason: Option<String>,
    },
    UserInputResponse {
        id: Option<String>,
        value: Option<String>,
    },
}

#[cfg(unix)]
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ServerFrame {
    Ready,
    Thread {
        thread_id: String,
    },
    Message {
        content: String,
    },
    Approval {
        id: Option<String>,
        tool_name: String,
        reason: String,
        command: Option<String>,
        cwd: String,
    },
    UserInput {
        id: Option<String>,
        prompt: String,
    },
    Done {
        status: String,
    },
    Error {
        message: String,
    },
}

#[cfg(unix)]
async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &ServerFrame,
) -> Result<()> {
    let payload = serde_json::to_vec(frame).context("编码 YunXi shell IPC 消息失败")?;
    writer.write_all(&payload).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(unix)]
async fn send_client_frame<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &ClientFrame,
) -> Result<()> {
    let payload = serde_json::to_vec(frame).context("编码 YunXi shell 请求失败")?;
    writer.write_all(&payload).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(unix)]
fn socket_path() -> Result<PathBuf> {
    let base = if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime).join("yunxi")
    } else {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|p| p.join(".local").join("state")))
            .unwrap_or_else(|| PathBuf::from(".yunxi-state"))
            .join("yunxi")
            .join("run")
    };
    fs::create_dir_all(&base)?;
    restrict_mode(&base, 0o700)?;
    Ok(base.join("yunxi.sock"))
}

#[cfg(unix)]
fn restrict_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    let _ = (path, mode);
    Ok(())
}

#[cfg(unix)]
async fn run_shell_intercept(
    shell: String,
    cwd: PathBuf,
    prompt: String,
    offline: bool,
    live: bool,
    provider: Option<String>,
    model: Option<String>,
) -> Result<()> {
    if shell != "fish" {
        bail!("当前仅实现 Miyu 风格 fish 接管，收到 shell={shell}");
    }
    let cwd =
        fs::canonicalize(&cwd).with_context(|| format!("无法访问工作区: {}", cwd.display()))?;
    let socket = ensure_daemon().await?;
    let stream = UnixStream::connect(&socket).await?;
    let (reader, mut writer) = stream.into_split();
    send_client_frame(
        &mut writer,
        &ClientFrame::Turn {
            cwd: cwd.display().to_string(),
            prompt,
            session_id: std::env::var("YUNXI_SHELL_SESSION").ok(),
            offline,
            live,
            provider,
            model,
        },
    )
    .await?;
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let frame: ServerFrame =
            serde_json::from_str(&line).context("解析 YunXi shell IPC 消息失败")?;
        match frame {
            ServerFrame::Message { content } => {
                println!("{content}");
            }
            ServerFrame::Thread { thread_id } => {
                // The daemon owns continuity. Keep this visible for diagnostics without
                // forcing fish to mutate the user's environment.
                eprintln!("[yunxi session {thread_id}]");
            }
            ServerFrame::Approval {
                id,
                tool_name,
                reason,
                command,
                cwd,
            } => {
                eprintln!(
                    "\n[YunXi 请求审批] tool={tool_name} cwd={cwd}\n{reason}{}\n允许? [y/N] ",
                    command
                        .map(|value| format!("\ncommand: {value}"))
                        .unwrap_or_default()
                );
                let approved = read_yes_no()?;
                send_client_frame(
                    &mut writer,
                    &ClientFrame::ApprovalResponse {
                        id,
                        approved,
                        reason: Some(
                            if approved {
                                "fish shell user approved"
                            } else {
                                "fish shell user denied"
                            }
                            .to_string(),
                        ),
                    },
                )
                .await?;
            }
            ServerFrame::UserInput { id, prompt } => {
                let value = read_terminal_line(&format!("\n[YunXi 需要输入] {prompt}\n> "))?;
                send_client_frame(
                    &mut writer,
                    &ClientFrame::UserInputResponse {
                        id,
                        value: Some(value.trim_end().to_string()),
                    },
                )
                .await?;
            }
            ServerFrame::Done { status } => {
                if status != "completed" {
                    eprintln!("[yunxi {status}]");
                }
                return Ok(());
            }
            ServerFrame::Error { message } => bail!("{message}"),
            ServerFrame::Ready => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_yes_no() -> Result<bool> {
    let answer = read_terminal_line("")?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(unix)]
fn read_terminal_line(prompt: &str) -> Result<String> {
    use std::fs::OpenOptions;
    use std::io::BufRead;

    if let Ok(mut tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") {
        tty.write_all(prompt.as_bytes())?;
        tty.flush()?;
        let mut reader = io::BufReader::new(tty);
        let mut value = String::new();
        reader.read_line(&mut value)?;
        return Ok(value.trim_end().to_string());
    }
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value.trim_end().to_string())
}

#[cfg(not(unix))]
async fn run_shell_intercept(
    _shell: String,
    _cwd: PathBuf,
    _prompt: String,
    _offline: bool,
    _live: bool,
    _provider: Option<String>,
    _model: Option<String>,
) -> Result<()> {
    bail!("fish 接管仅支持 Unix/Linux；Linux 构建不会在 Windows 上启用它")
}

#[cfg(unix)]
async fn ensure_daemon() -> Result<PathBuf> {
    let socket = socket_path()?;
    if UnixStream::connect(&socket).await.is_ok() {
        return Ok(socket);
    }
    let binary = std::env::var_os("YUNXI_LINUX_BINARY")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .context("无法定位 yunxi-linux 可执行文件")?;
    let _ = std::process::Command::new(binary)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    for _ in 0..40 {
        sleep(Duration::from_millis(50)).await;
        if UnixStream::connect(&socket).await.is_ok() {
            return Ok(socket);
        }
    }
    bail!("YunXi shell daemon 未能在 2 秒内启动；检查 XDG_RUNTIME_DIR 与用户权限")
}

#[cfg(not(unix))]
async fn run_daemon() -> Result<()> {
    bail!("YunXi shell daemon 仅支持 Unix/Linux")
}

#[cfg(unix)]
async fn run_daemon() -> Result<()> {
    let socket = socket_path()?;
    if UnixStream::connect(&socket).await.is_ok() {
        return Ok(());
    }
    if socket.exists() {
        fs::remove_file(&socket)
            .with_context(|| format!("删除失效 YunXi socket 失败: {}", socket.display()))?;
    }
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("绑定 YunXi shell socket 失败: {}", socket.display()))?;
    restrict_mode(&socket, 0o600)?;
    let sessions = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    loop {
        let (stream, _) = listener.accept().await?;
        let sessions = Arc::clone(&sessions);
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, sessions).await {
                eprintln!("yunxi daemon connection error: {error:#}");
            }
        });
    }
}

#[cfg(unix)]
async fn handle_connection(
    stream: UnixStream,
    sessions: Arc<Mutex<HashMap<String, String>>>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<ClientFrame>(16);
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(frame) = serde_json::from_str::<ClientFrame>(&line) {
                if tx.send(frame).await.is_err() {
                    break;
                }
            }
        }
    });
    let Some(first) = rx.recv().await else {
        return Ok(());
    };
    match first {
        ClientFrame::Ping => {
            write_frame(&mut writer, &ServerFrame::Ready).await?;
        }
        ClientFrame::Turn {
            cwd,
            prompt,
            session_id,
            offline,
            live,
            provider,
            model,
        } => {
            run_daemon_turn(
                &mut writer,
                &mut rx,
                sessions,
                cwd,
                prompt,
                session_id,
                offline,
                live,
                provider,
                model,
            )
            .await?;
        }
        ClientFrame::ApprovalResponse { .. } | ClientFrame::UserInputResponse { .. } => {
            write_frame(
                &mut writer,
                &ServerFrame::Error {
                    message: "请求顺序无效".to_string(),
                },
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(unix)]
async fn run_daemon_turn<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    rx: &mut mpsc::Receiver<ClientFrame>,
    sessions: Arc<Mutex<HashMap<String, String>>>,
    cwd: String,
    prompt: String,
    requested_session: Option<String>,
    offline: bool,
    live: bool,
    provider: Option<String>,
    model: Option<String>,
) -> Result<()> {
    let cwd_path = PathBuf::from(&cwd);
    let mut config = AgentConfig::new(cwd_path.clone());
    if let Some(provider) = provider {
        config.provider = Some(provider);
    }
    if let Some(model) = model {
        config.model = Some(model);
    }
    let selection = crate::ProviderSelection::resolve(&config, offline, live)?;
    let backend = if selection.live {
        YunXiRuntimeBackend::for_workspace_with_live_provider(&cwd_path, &config)
    } else {
        YunXiRuntimeBackend::for_workspace(&cwd_path)
    };
    let prior = if requested_session.is_some() {
        requested_session
    } else {
        sessions.lock().await.get(&cwd).cloned()
    };
    config.session_title = Some("YunXi Linux fish shell".to_string());
    config.parent_session_id = prior;
    let agent = Agent::new(config);
    let (control, mut stream) = AgentRunControl::streaming();
    let run_control = control.clone();
    let mut turn =
        Box::pin(agent.run_with_backend_stream(&backend, AgentInput::text(prompt), run_control));
    let mut thread_id = None;
    let mut result = None;
    loop {
        if result.is_some() {
            break;
        }
        tokio::select! {
            event = stream.events.recv() => match event {
                Some(AgentEvent::ThreadStarted { thread_id: id }) => {
                    thread_id = Some(id.clone());
                    write_frame(writer, &ServerFrame::Thread { thread_id: id }).await?;
                }
                Some(AgentEvent::Message { content, .. }) => {
                    write_frame(writer, &ServerFrame::Message { content }).await?;
                }
                Some(_) => {}
                None => {}
            },
            request = stream.approvals.recv() => if let Some(request) = request {
                write_frame(writer, &ServerFrame::Approval {
                    id: request.id.clone(),
                    tool_name: request.tool_name.clone(),
                    reason: request.reason.clone(),
                    command: request.command.clone(),
                    cwd: request.cwd.clone(),
                }).await?;
                let decision = await_approval(rx, request.id.as_deref()).await?;
                let _ = request.respond_to.send(AgentRunApprovalDecision { approved: decision.0, reason: decision.1 });
            },
            request = stream.user_inputs.recv() => if let Some(request) = request {
                write_frame(writer, &ServerFrame::UserInput { id: request.id.clone(), prompt: request.prompt.clone() }).await?;
                let value = await_user_input(rx, request.id.as_deref()).await?;
                let _ = request.respond_to.send(AgentRunUserInputResponse { value });
            },
            frame = rx.recv() => if let Some(ClientFrame::Turn { .. }) = frame {
                control.cancel();
            },
            turn_result = &mut turn => {
                result = Some(turn_result.context("YunXi Runtime 执行失败")?);
            }
        }
    }
    let result = result.expect("turn result is set before leaving loop");
    if !result
        .events
        .iter()
        .any(|event| matches!(event, AgentEvent::Message { .. }))
        && let Some(content) = result.final_response
    {
        write_frame(writer, &ServerFrame::Message { content }).await?;
    }
    let status = match result.status {
        AgentRunStatus::Completed => "completed",
        AgentRunStatus::Failed => "failed",
        AgentRunStatus::Cancelled => "cancelled",
    };
    if let Some(thread_id) = thread_id {
        sessions.lock().await.insert(cwd, thread_id);
    }
    write_frame(
        writer,
        &ServerFrame::Done {
            status: status.to_string(),
        },
    )
    .await?;
    Ok(())
}

#[cfg(unix)]
async fn await_approval(
    rx: &mut mpsc::Receiver<ClientFrame>,
    expected: Option<&str>,
) -> Result<(bool, Option<String>)> {
    while let Some(frame) = rx.recv().await {
        if let ClientFrame::ApprovalResponse {
            id,
            approved,
            reason,
        } = frame
            && id.as_deref() == expected
        {
            return Ok((approved, reason));
        }
    }
    Ok((false, Some("shell client disconnected".to_string())))
}

#[cfg(unix)]
async fn await_user_input(
    rx: &mut mpsc::Receiver<ClientFrame>,
    expected: Option<&str>,
) -> Result<Option<String>> {
    while let Some(frame) = rx.recv().await {
        if let ClientFrame::UserInputResponse { id, value } = frame
            && id.as_deref() == expected
        {
            return Ok(value);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_commands_and_prose_like_miyu() {
        #[cfg(unix)]
        assert!(is_shell_command("ls -la", "fish"));
        assert!(is_shell_command("echo hi", "fish"));
        assert!(is_shell_command("cd /tmp", "fish"));
        assert!(is_shell_command("FOO=bar echo hi", "fish"));
        assert!(is_shell_command("for item in a b", "fish"));
        assert!(!is_shell_command("你好，帮我找一下项目文件", "fish"));
        assert!(!is_shell_command("time 是什么命令？", "fish"));
        assert!(!is_shell_command("第一行\n第二行", "fish"));
    }

    #[test]
    fn hook_keeps_the_real_miyu_boundaries() {
        let hook = fish_hook(Path::new("/home/user/.local/bin/yunxi-linux"));
        assert!(hook.contains("commandline --input=\"$argv[1]\" --tokens-raw"));
        assert!(hook.contains("shell-classify --shell fish --stdin"));
        assert!(hook.contains("shell-intercept --shell fish --cwd \"$PWD\" --stdin"));
        assert!(hook.contains("fish_command_not_found"));
        assert!(hook.contains("bind enter __yunxi_accept_line"));
        assert!(hook.contains("bind ctrl-j __yunxi_insert_newline"));
    }
}
