use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::{self, MissedTickBehavior};
use yunxi_agent_core::{
    Agent, AgentConfig, AgentEvent, AgentInput, AgentRunApprovalDecision, AgentRunControl,
    AgentRunResult, AgentRunStatus, AgentRunUserInputResponse,
};
use yunxi_agent_provider::ProviderBootstrap;
use yunxi_agent_runtime::YunXiRuntimeBackend;
use yunxi_agent_tui::{
    ApprovalRequestView, TuiTickAction, UserInputRequestView, YunxiTui, YunxiTuiBanner,
};

#[derive(Debug, Parser)]
#[command(
    name = "yunxi-archlinux",
    version,
    about = "YunXi Agent 的 Arch Linux TUI 版本（仅文本交互）"
)]
struct Args {
    /// 工作区路径，记忆、会话和人格状态会写入该工作区的 .yunxi 目录。
    #[arg(long, value_name = "PATH", default_value = ".")]
    cwd: PathBuf,

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
}

#[derive(Clone, Debug)]
struct ProviderSelection {
    live: bool,
    source: &'static str,
    provider: String,
    model: String,
}

impl ProviderSelection {
    fn resolve(config: &AgentConfig, offline: bool, force_live: bool) -> Result<Self> {
        if offline && force_live {
            anyhow::bail!("--offline 与 --live 不能同时使用");
        }

        let bootstrap = ProviderBootstrap::from_agent_config(config);
        let credentials = bootstrap.credentials_configured();
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let cwd = std::fs::canonicalize(&args.cwd)
        .with_context(|| format!("无法访问工作区: {}", args.cwd.display()))?;

    let mut base_config = AgentConfig::new(cwd.clone());
    if let Some(provider) = &args.provider {
        base_config.provider = Some(provider.clone());
    }
    if let Some(model) = &args.model {
        base_config.model = Some(model.clone());
    }

    let selection = ProviderSelection::resolve(&base_config, args.offline, args.live)?;
    let backend = if selection.live {
        YunXiRuntimeBackend::for_workspace_with_live_provider(&cwd, &base_config)
    } else {
        YunXiRuntimeBackend::for_workspace(&cwd)
    };
    let mut tui = YunxiTui::enter().context("无法进入终端 TUI；请在真实终端中运行")?;
    tui.set_banner(YunxiTuiBanner {
        cwd: cwd.display().to_string(),
        backend: "yunxi-archlinux".to_string(),
        provider_live: selection.live,
        provider_source: selection.source.to_string(),
        model: selection.model.clone(),
        provider: selection.provider.clone(),
    })?;
    if !selection.live {
        tui.push_warning("[offline] 未找到在线凭证，当前使用本地静态 Runtime；不会调用模型")?;
    }
    tui.push_notice(
        "archlinux",
        "仅 TUI 文本模式：/help 查看命令，/exit 退出；不包含 Web、语音和微信模块",
    )?;

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
                    "/help 查看帮助\n/clear 清空当前屏幕\n/status 查看运行状态\n/exit 退出",
                )?;
                continue;
            }
            "/clear" => {
                tui.clear_transcript()?;
                continue;
            }
            "/status" => {
                tui.push_notice(
                    "status",
                    &format!(
                        "provider={} source={} model={} turns={} session={}",
                        selection.provider,
                        selection.source,
                        selection.model,
                        turn_count,
                        session_id.as_deref().unwrap_or("new")
                    ),
                )?;
                continue;
            }
            _ => {}
        }

        turn_count = turn_count.saturating_add(1);
        let mut turn_config = base_config.clone();
        turn_config.session_title = Some(format!("Arch Linux TUI turn {turn_count}"));
        if let Some(parent) = &session_id {
            turn_config.parent_session_id = Some(parent.clone());
        }

        run_turn(&mut tui, &backend, turn_config, prompt, &mut session_id).await?;
    }

    Ok(())
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
        let selection = ProviderSelection::resolve(&config, true, false).expect("offline mode");

        assert!(!selection.live);
        assert_eq!(selection.source, "forced_offline");
        assert_eq!(selection.provider, "offline");
        assert_eq!(selection.model, "static");
    }

    #[test]
    fn live_and_offline_flags_are_rejected_together() {
        let config = AgentConfig::new(".");
        let error = ProviderSelection::resolve(&config, true, true)
            .expect_err("conflicting provider flags must fail");

        assert!(error.to_string().contains("不能同时使用"));
    }
}
