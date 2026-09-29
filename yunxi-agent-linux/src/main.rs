use anyhow::{Context, Result};
use clap::Parser;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::{self, MissedTickBehavior};
use yunxi_agent_core::{
    Agent, AgentConfig, AgentEvent, AgentInput, AgentRunApprovalDecision, AgentRunControl,
    AgentRunResult, AgentRunStatus, AgentRunUserInputResponse,
};
use yunxi_agent_persona::{PersonaSettings, yunxi_home_dir};
use yunxi_agent_provider::ProviderBootstrap;
use yunxi_agent_runtime::YunXiRuntimeBackend;
use yunxi_agent_storage::FileSessionStore;
use yunxi_agent_tools::CompositeToolRuntime;
use yunxi_agent_tui::{
    ApprovalRequestView, OnboardingOutcome, OnboardingStep, TuiTickAction, UserInputRequestView,
    YunxiTui, YunxiTuiBanner,
};

mod config;
mod onboarding;
mod shell;

use onboarding::{OnboardingPaths, WizardDriver};
use shell::LinuxShellCommand;

/// 开场动画的宿主预算（帧）。
///
/// TUI 侧还有自己的全局预算（`WELCOME_ANIMATION_FRAMES` = 120 帧 ≈ 4.0s），
/// `play_intro()` 取两者的小值，所以这里给 135（≈4.5s）只是表达了
/// 「方案要求 4-5 秒」的上界，真正的时长以 TUI 的编译期断言为准。
const INTRO_BUDGET_FRAMES: usize = 135;

#[derive(Debug, Parser)]
#[command(
    name = "yunxi-linux",
    version,
    about = "YunXi Native 的 Linux 文本宿主（TUI + fish 接管）"
)]
struct Args {
    #[command(subcommand)]
    command: Option<LinuxShellCommand>,

    /// 工作区路径，记忆、会话和人格状态会写入该工作区的 .yunxi 目录。
    #[arg(long, value_name = "PATH", default_value = ".")]
    cwd: PathBuf,

    /// 把模型的推理过程以一行摘要显示在会话里（默认不显示）。
    ///
    /// 默认关闭是刻意的：转录本只留正文与工具状态，推理进详情。
    #[arg(long)]
    reasoning: bool,

    /// 强制使用本地离线 Runtime，不调用 Provider。
    #[arg(long)]
    offline: bool,

    /// 强制要求在线 Provider 凭证存在；未配置时直接退出。
    #[arg(long)]
    live: bool,

    /// Provider profile，例如 deepseek 或 openai-compatible。
    #[arg(long)]
    provider: Option<String>,

    /// 覆盖 Provider 模型名。
    #[arg(long)]
    model: Option<String>,

    /// 上下文窗口估算上限（token）；未指定时读取 YUNXI_CONTEXT_WINDOW_TOKENS。
    #[arg(long, value_name = "TOKENS")]
    context_window_tokens: Option<i64>,

    /// 自动压缩阈值（token）；未指定时读取 YUNXI_AUTO_COMPACT_THRESHOLD_TOKENS。
    #[arg(long, value_name = "TOKENS")]
    auto_compact_threshold_tokens: Option<i64>,

    /// 打印 Linux/XDG 与工作区路径后退出，不进入 TUI。
    #[arg(long)]
    print_paths: bool,
}

#[derive(Clone, Debug)]
struct ProviderSelection {
    live: bool,
    source: &'static str,
    provider: String,
    model: String,
}

impl ProviderSelection {
    /// shell 子命令用的入口：只看进程环境（保持既有行为不变）。
    fn resolve(config: &AgentConfig, offline: bool, force_live: bool) -> Result<Self> {
        Self::resolve_with_env(config, offline, force_live, &process_environment)
    }

    /// 解析本次会话用在线还是离线 Provider。
    ///
    /// `env` 是**分层环境查找**：进程环境优先，其次 `~/.config/yunxi/environment`。
    /// 判据②（[`onboarding::credentials_available`]）已经承认那个文件是合法凭证来源，
    /// 运行时就必须用同一套来源 —— 否则会出现「判据说已配置、这一秒却按离线跑」
    /// 的自相矛盾：向导刚把 key 写进文件，用户却发现自己还在静态 Runtime 里。
    fn resolve_with_env<F>(
        config: &AgentConfig,
        offline: bool,
        force_live: bool,
        env: &F,
    ) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        if offline && force_live {
            anyhow::bail!("--offline 与 --live 不能同时使用");
        }

        let bootstrap = ProviderBootstrap::from_agent_config_with_env(config, env);
        let credentials = bootstrap.credentials_configured_with_env(env);
        if force_live && !credentials {
            anyhow::bail!(
                "在线 Provider {} 未配置凭证；请设置 YUNXI_PROVIDER_API_KEY、DEEPSEEK_API_KEY 或 OPENAI_API_KEY",
                bootstrap.config.name
            );
        }

        let live = !offline && (force_live || credentials);
        Ok(Self {
            live,
            source: if offline {
                "forced_offline"
            } else if force_live {
                "forced_live"
            } else if live {
                "auto_live"
            } else {
                "auto_offline"
            },
            provider: if live {
                bootstrap.config.name
            } else {
                "offline".to_string()
            },
            model: if live {
                bootstrap.config.model
            } else {
                "static".to_string()
            },
        })
    }
}

/// 宿主侧看到的向导就是 TUI 的那一个入口（方案第三节「宿主导入接口」）。
///
/// 这层 `impl` 里没有任何逻辑：步骤怎么排、验证失败怎么重试、跳过提示什么，
/// 全在 `onboarding` 模块里，换成假向导就能单测。这里只做转发，
/// 加上「验证失败要说给用户听」这一件事（`push_warning` 落在会话区）。
impl WizardDriver for YunxiTui {
    fn run_onboarding(&mut self, steps: Vec<OnboardingStep>) -> Result<OnboardingOutcome> {
        YunxiTui::run_onboarding(self, steps)
    }

    fn report_retry(&mut self, message: &str) -> Result<()> {
        self.push_warning(message)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(command) = args.command {
        return shell::run_command(command).await;
    }
    let cwd = std::fs::canonicalize(&args.cwd)
        .with_context(|| format!("无法访问工作区: {}", args.cwd.display()))?;
    let paths = prepare_storage_layout(&cwd)?;

    if args.print_paths {
        println!("yunxi_home={}", paths.yunxi_home.display());
        println!("workspace_state={}", paths.workspace_state.display());
        println!("xdg_state={}", paths.xdg_state.display());
        println!("xdg_cache={}", paths.xdg_cache.display());
        return Ok(());
    }

    let mut base_config = AgentConfig::new(cwd.clone());
    config::apply_linux_runtime_settings(&mut base_config);
    config::apply_context_budget(
        &mut base_config,
        args.context_window_tokens,
        args.auto_compact_threshold_tokens,
    );
    if let Some(provider) = &args.provider {
        base_config.provider = Some(provider.clone());
    }
    if let Some(model) = &args.model {
        base_config.model = Some(model.clone());
    }

    let mut tui = YunxiTui::enter().context("无法进入终端 TUI；请在真实终端中运行")?;
    let banner_enabled = tui_banner_enabled();

    // ── 首次配置向导 ────────────────────────────────────────────────
    //
    // 方案第一节：三者全满足才算「已配置」——
    //   ① `~/.local/state/yunxi/onboarding-complete`（新标记，不复用 first-run-complete）
    //   ② Provider 凭证（进程环境或 `~/.config/yunxi/environment`）
    //   ③ `PersonaSettings` 存在（人格选过）
    // 未配置就走向导；`YUNXI_TUI_BANNER=0` 时整个开场（向导 + 动画）都不出现。
    let onboarding_paths = OnboardingPaths::resolve(&paths.xdg_state);
    let onboarding_status = onboarding::detect_from_process(&onboarding_paths);
    let mut onboarding_notice: Option<String> = None;
    if banner_enabled && !onboarding_status.is_configured() {
        match onboarding::run_first_time_flow(&mut tui, &onboarding_paths, &cwd, |probe| {
            let cwd = cwd.clone();
            async move { onboarding::verify_provider(&probe, &cwd).await }
        })
        .await?
        {
            onboarding::FlowOutcome::Completed(report) => {
                onboarding_notice = Some(onboarding::completion_notice(&report));
            }
            // 跳过不写任何东西（包括完成标记），只把「哪些没配、后果、怎么补」讲清楚。
            onboarding::FlowOutcome::Skipped(report) => {
                onboarding_notice = Some(report.notice);
            }
        }
    }

    // 向导可能刚把凭证写进 environment 文件，运行时按同一套来源重新解析一次。
    let environment_vars = onboarding::read_environment_vars(&onboarding_paths.environment_file);
    let env_lookup = |name: &str| -> Option<String> {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| environment_vars.get(name).cloned())
    };

    let selection =
        ProviderSelection::resolve_with_env(&base_config, args.offline, args.live, &env_lookup)?;
    let backend = if selection.live {
        // 与 `for_workspace_with_live_provider` 同一条装配路径，只是把凭证来源
        // 换成「进程环境 + environment 文件」，这样向导写完 key 当场就能用上。
        let bootstrap = ProviderBootstrap::from_agent_config_with_env(&base_config, &env_lookup);
        YunXiRuntimeBackend::with_parts(
            bootstrap.into_openai_transport_provider(),
            CompositeToolRuntime::default(),
            FileSessionStore::for_workspace(&cwd),
        )
        .with_inherited_child_provider()
    } else {
        YunXiRuntimeBackend::for_workspace(&cwd)
    };
    tui.set_banner(YunxiTuiBanner {
        cwd: cwd.display().to_string(),
        backend: "yunxi-linux".to_string(),
        provider_live: selection.live,
        provider_source: selection.source.to_string(),
        model: selection.model.clone(),
        provider: selection.provider.clone(),
    })?;
    tui.set_welcome_enabled(banner_enabled)?;
    tui.set_reasoning_expanded(args.reasoning)?;
    // The checklist is rendered inside the welcome card, so it is only handed to
    // the TUI when the card is on.  "First run" itself is independent of the card:
    // with YUNXI_TUI_BANNER=0 there is no card, and the orientation notice below
    // is then the only thing introducing the scoped surface.
    let checklist = claim_first_run_checklist(&paths, &cwd, &selection)?;
    let first_run = checklist.is_some();
    if let (true, Some(checklist)) = (banner_enabled, checklist) {
        tui.set_welcome_checklist(checklist)?;
    }
    if !selection.live {
        tui.push_warning("[offline] 未找到在线凭证，当前使用本地静态 Runtime；不会调用模型")?;
    }
    // The host scope note is onboarding, not a per-session banner: on a 100-column
    // terminal it used to wrap onto two rows at the top of every conversation.
    // Introduce the scoped surface once, then leave the transcript to the user.
    // The same facts stay reachable through /help and /capabilities.
    if first_run {
        tui.push_notice(
            "linux",
            "本机为 Linux 原生 TUI · /help 查看命令 · 不含 Web / 语音 / 微信模块",
        )?;
    }

    // ── 开场动画 ────────────────────────────────────────────────────
    //
    // 方案第一节：已配置（或刚配完）→ 4-5 秒开场动画（任意键跳过）→ 会话界面。
    // 动画预算由 TUI 侧决定（编译期断言 4-5 秒），这里只写宿主的追加上限。
    // `YUNXI_TUI_BANNER=0` 时既没有欢迎卡也没有动画，宿主连调都不调。
    if banner_enabled {
        let _skipped = tui.play_intro(INTRO_BUDGET_FRAMES)?;
    }
    // 向导的结论放在动画之后说：先让开场把界面带进会话态，再把「配好了什么」
    // 或「跳过了什么、代价是什么」留在最上面这一条。
    if let Some(notice) = onboarding_notice {
        tui.push_notice("onboarding", &notice)?;
    }

    let mut session_id: Option<String> = None;
    let mut turn_count = 0usize;

    loop {
        let Some(prompt) = tui.read_prompt("yunxi> ")? else {
            break;
        };
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            continue;
        }

        match prompt.as_str() {
            "/exit" | "/quit" => break,
            "/help" => {
                tui.push_notice(
                    "help",
                    "/help 查看帮助\n/capabilities 查看 Linux 版能力\n/clear 清空当前屏幕\n/status 查看运行状态\n/exit 退出",
                )?;
                continue;
            }
            "/capabilities" => {
                tui.push_notice(
                    "capabilities",
                    "对话与会话 · 人格与灵魂 · 分层记忆与本地向量召回 · 陪伴策略与关系阶段 · 情书信箱 · Provider\n工具路由 · Shell/补丁 · Sandbox · MCP · Skills · 多 Agent · 审批与用户输入 · XDG 本地存储\nLinux 原生：fish 自然语言接管、每用户 Unix socket daemon、会话续接\n明确排除：Web、语音、微信/iLink、Windows 独占功能",
                )?;
                continue;
            }
            "/clear" => {
                tui.clear_transcript()?;
                continue;
            }
            "/status" => {
                let cwd = std::env::current_dir()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                tui.push_notice(
                    "status",
                    &render_status_panel(
                        &cwd,
                        &selection,
                        &base_config,
                        turn_count,
                        session_id.as_deref(),
                    ),
                )?;
                continue;
            }
            _ => {}
        }

        turn_count = turn_count.saturating_add(1);
        let mut turn_config = base_config.clone();
        turn_config.session_title = Some(format!("YunXi Linux TUI turn {turn_count}"));
        if let Some(parent) = &session_id {
            turn_config.parent_session_id = Some(parent.clone());
        }

        tui.begin_turn()?;
        let turn_result = run_turn(&mut tui, &backend, turn_config, prompt, &mut session_id).await;
        tui.end_turn()?;
        turn_result?;
    }

    Ok(())
}

#[derive(Clone, Debug)]
struct StoragePaths {
    yunxi_home: PathBuf,
    workspace_state: PathBuf,
    xdg_state: PathBuf,
    xdg_cache: PathBuf,
}

fn prepare_storage_layout(cwd: &std::path::Path) -> Result<StoragePaths> {
    let yunxi_home = yunxi_home_dir();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg_state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|path| path.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from(".yunxi-state"))
        .join("yunxi");
    let xdg_cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|path| path.join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".yunxi-cache"))
        .join("yunxi");
    let workspace_state = cwd.join(".yunxi");

    for directory in [&yunxi_home, &workspace_state, &xdg_state, &xdg_cache] {
        fs::create_dir_all(directory)
            .with_context(|| format!("无法创建 YunXi 目录: {}", directory.display()))?;
        restrict_directory_permissions(directory)?;
    }

    Ok(StoragePaths {
        yunxi_home,
        workspace_state,
        xdg_state,
        xdg_cache,
    })
}

fn restrict_directory_permissions(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let _ = path;
    Ok(())
}

/// 真实进程环境（shell 子命令那条路径用的就是它，语义与改动前一致）。
fn process_environment(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn tui_banner_enabled() -> bool {
    !std::env::var("YUNXI_TUI_BANNER").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

fn claim_first_run_checklist(
    paths: &StoragePaths,
    cwd: &std::path::Path,
    selection: &ProviderSelection,
) -> Result<Option<Vec<String>>> {
    let marker = paths.xdg_state.join("first-run-complete");
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("无法写入首次启动标记: {}", marker.display()));
        }
    };
    writeln!(file, "completed")
        .with_context(|| format!("无法写入首次启动标记: {}", marker.display()))?;

    let provider = if selection.live {
        format!("{} · 已就绪", selection.provider)
    } else {
        "离线 Runtime · 已就绪".to_string()
    };
    Ok(Some(vec![
        format!("  工作区                 ✓ {}", cwd.display()),
        format!("  YunXi 状态目录         ✓ {}", paths.xdg_state.display()),
        format!(
            "  Provider / 模型        ✓ {} / {}",
            provider, selection.model
        ),
        "  会话与记忆目录         ✓ 已初始化".to_string(),
        "  默认知识库             - 未配置，稍后可接入".to_string(),
        "  完成。直接输入目标即可开始，/help 查看帮助".to_string(),
    ]))
}

/// `/status` answers two different questions, so it renders two blocks.
///
/// The first block is diagnosis: which binary, which workspace, which provider,
/// and how the current session is identified.  The second block is the feature
/// panel — the one place a user can see which optional subsystems are actually
/// on, and what to set to turn the rest on.
///
/// Every switch is read from the same source the runtime reads: `PersonaSettings`
/// for the persona switches, and the already-resolved `AgentConfig` for what
/// reached the companion subsystem.  Reporting the parsed configuration instead
/// of the raw environment is deliberate — it is what revealed that the Linux host
/// never bridged `YUNXI_COMPANION_ENABLED` into `AgentConfig.companion`, and a
/// status panel that reads the environment again would have hidden that.
fn render_status_panel(
    cwd: &str,
    selection: &ProviderSelection,
    config: &AgentConfig,
    turn_count: usize,
    session_id: Option<&str>,
) -> String {
    let settings = PersonaSettings::load();
    let soul_path = yunxi_home_dir().join("persona").join("soul.txt");

    // "on" is stated plainly; "off" carries its own enablement instructions so the
    // panel doubles as the entry point for every optional subsystem.
    let enabled = |on: bool, hint: &str| -> String {
        if on {
            "● 已开启".to_string()
        } else {
            format!("○ 未开启   {hint}")
        }
    };

    let mut lines = vec![
        format!(
            "版本       v{} · turns={} · session={}",
            env!("CARGO_PKG_VERSION"),
            turn_count,
            session_id.unwrap_or("new")
        ),
        format!("工作区     {cwd}"),
        format!(
            "Provider   {} · {} · {}",
            selection.provider, selection.model, selection.source
        ),
        String::new(),
        "功能".to_string(),
        format!(
            "  人格     {}",
            enabled(
                settings.persona_enabled,
                "设 YUNXI_PERSONA_ENABLED=true 开启"
            )
        ),
        format!(
            "  灵魂     ● 内置   profile 内建；{} 可整体覆盖",
            soul_path.display()
        ),
        format!(
            "  记忆     {}",
            enabled(
                settings.memory_enabled,
                "设 YUNXI_MEMORY_ENABLED=true 开启；开启后 /status 之外的 memory-pending 可查看候选"
            )
        ),
        format!(
            "  陪伴     {}",
            enabled(
                config.companion.enabled,
                "设 YUNXI_COMPANION_ENABLED=true 开启"
            )
        ),
        format!(
            "  情书     {}",
            enabled(
                config.companion.love_letters.enabled,
                "需先开启陪伴；再设 YUNXI_LOVE_LETTERS_ENABLED=true（信箱密钥缺失时会在工作区自动生成）"
            )
        ),
        format!(
            "  云端控制 {}",
            enabled(
                config.companion.cloud_control_enabled,
                "设 YUNXI_CLOUD_CONTROL_ENABLED=true 开启"
            )
        ),
    ];

    let window = config
        .context_window_tokens
        .map(|value| value.to_string())
        .unwrap_or_else(|| "未设定".to_string());
    let compact = config
        .auto_compact_threshold_tokens
        .map(|value| value.to_string())
        .unwrap_or_else(|| "未设定（不自动压缩）".to_string());
    lines.push(String::new());
    lines.push(format!("上下文     窗口={window} · 压缩阈值={compact}"));

    lines.join("\n")
}

async fn run_turn(
    tui: &mut YunxiTui,
    backend: &YunXiRuntimeBackend,
    config: AgentConfig,
    prompt: String,
    session_id: &mut Option<String>,
) -> Result<()> {
    let turn_agent = Agent::new(config);
    let (control, mut stream) = AgentRunControl::streaming();
    let mut control_slot = Some(control);
    let run_control = control_slot
        .as_ref()
        .expect("run control is present before starting a turn")
        .clone();
    let mut turn = Box::pin(async move {
        turn_agent
            .run_with_backend_stream(backend, AgentInput::text(prompt), run_control)
            .await
            .context("YunXi Runtime 执行失败")
    });
    let mut result: Option<AgentRunResult> = None;
    let mut events_open = true;
    let mut approvals_open = true;
    let mut user_inputs_open = true;
    let mut tick = time::interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        if result.is_some() && !events_open && !approvals_open && !user_inputs_open {
            break;
        }

        tokio::select! {
            event = stream.events.recv(), if events_open => {
                match event {
                    Some(event) => {
                        if let AgentEvent::ThreadStarted { thread_id } = &event {
                            *session_id = Some(thread_id.clone());
                        }
                        tui.push_agent_event(&event)?;
                    }
                    None => events_open = false,
                }
            }
            request = stream.approvals.recv(), if approvals_open => {
                match request {
                    Some(request) => {
                        let decision = tui.request_approval(ApprovalRequestView {
                            id: request.id.clone(),
                            tool_name: request.tool_name.clone(),
                            cwd: request.cwd.clone(),
                            command: request.command.clone(),
                            reason: request.reason.clone(),
                            risk_label: None,
                        })?;
                        let _ = request.respond_to.send(AgentRunApprovalDecision {
                            approved: decision.approved,
                            reason: decision.reason,
                        });
                    }
                    None => approvals_open = false,
                }
            }
            request = stream.user_inputs.recv(), if user_inputs_open => {
                match request {
                    Some(request) => {
                        let response = tui.request_user_input(UserInputRequestView {
                            id: request.id.clone(),
                            prompt: request.prompt.clone(),
                        })?;
                        let _ = request.respond_to.send(AgentRunUserInputResponse {
                            value: response.value,
                        });
                    }
                    None => user_inputs_open = false,
                }
            }
            _ = tick.tick() => {
                if tui.tick()? == TuiTickAction::CancelCurrentTurn
                    && let Some(control) = &control_slot
                {
                    control.cancel();
                    tui.push_warning("[cancelled] cancellation requested")?;
                }
            }
            turn_result = &mut turn, if result.is_none() => {
                result = Some(turn_result?);
                control_slot = None;
            }
        }
    }

    while let Ok(event) = stream.events.try_recv() {
        if let AgentEvent::ThreadStarted { thread_id } = &event {
            *session_id = Some(thread_id.clone());
        }
        tui.push_agent_event(&event)?;
    }

    if let Some(result) = result {
        if result.status == AgentRunStatus::Cancelled {
            tui.push_warning("[cancelled] 当前回合已取消")?;
        }
        if !result
            .events
            .iter()
            .any(|event| matches!(event, AgentEvent::Message { .. }))
            && let Some(response) = result.final_response
        {
            tui.push_agent_event(&AgentEvent::Message {
                content: response,
                stream: None,
            })?;
        }
    }
    tui.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_mode_never_selects_a_live_provider() {
        let config = AgentConfig::new(".").with_provider("deepseek");
        let selection = ProviderSelection::resolve_with_env(&config, true, false, &no_environment)
            .expect("offline mode");

        assert!(!selection.live);
        assert_eq!(selection.source, "forced_offline");
        assert_eq!(selection.provider, "offline");
        assert_eq!(selection.model, "static");
    }

    #[test]
    fn live_and_offline_flags_are_rejected_together() {
        let config = AgentConfig::new(".");
        let error = ProviderSelection::resolve_with_env(&config, true, true, &no_environment)
            .expect_err("conflicting provider flags must fail");

        assert!(error.to_string().contains("不能同时使用"));
    }

    /// 测试用的空环境：不读进程环境，也不读 environment 文件。
    fn no_environment(_name: &str) -> Option<String> {
        None
    }

    /// 只提供 `YUNXI_PROVIDER_API_KEY` 的假环境（模拟 environment 文件里的凭证）。
    fn key_only(name: &str) -> Option<String> {
        (name == onboarding::ENV_API_KEY).then(|| "sk-from-file".to_string())
    }

    #[test]
    fn provider_selection_honours_credentials_from_the_environment_file() {
        let config = AgentConfig::new(".");

        // 进程环境与文件都没有 → 离线
        let offline = ProviderSelection::resolve_with_env(&config, false, false, &no_environment)
            .expect("auto offline");
        assert!(!offline.live);
        assert_eq!(offline.source, "auto_offline");

        // 判据②承认 environment 文件是凭证来源，运行时就必须认它
        let live = ProviderSelection::resolve_with_env(&config, false, false, &key_only)
            .expect("auto live");
        assert!(live.live);
        assert_eq!(live.source, "auto_live");
        assert_ne!(live.provider, "offline");
    }

    #[test]
    fn first_run_checklist_is_claimed_once_and_kept_out_of_session_data() {
        let root =
            std::env::temp_dir().join(format!("yunxi-first-run-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let xdg_state = root.join("state");
        fs::create_dir_all(&xdg_state).expect("state directory");
        let paths = StoragePaths {
            yunxi_home: root.join("home"),
            workspace_state: root.join("workspace"),
            xdg_state,
            xdg_cache: root.join("cache"),
        };
        let selection = ProviderSelection {
            live: false,
            source: "forced_offline",
            provider: "offline".to_string(),
            model: "static".to_string(),
        };

        let first =
            claim_first_run_checklist(&paths, std::path::Path::new("/tmp/workspace"), &selection)
                .expect("first claim");
        assert!(first.is_some());
        assert!(
            first
                .as_ref()
                .expect("checklist")
                .iter()
                .any(|line| line.contains("会话与记忆目录"))
        );

        let second =
            claim_first_run_checklist(&paths, std::path::Path::new("/tmp/workspace"), &selection)
                .expect("second claim");
        assert!(second.is_none());
        assert!(paths.xdg_state.join("first-run-complete").is_file());
        assert!(!paths.workspace_state.join("first-run-complete").exists());

        fs::remove_dir_all(root).expect("cleanup");
    }
}
