//! Idle-session memory reflection.
//!
//! Reflection is deliberately kept separate from the Linux knowledge store. It
//! turns completed, idle conversations into bounded structured memory
//! candidates, then sends those candidates through the existing memory policy
//! and vector-index sync path.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use yunxi_agent_core::{AgentConfig, AgentError, AgentResult, AgentRunStatus};
use yunxi_agent_persona::{
    LocalChargramEmbedding, MemoryPipeline, MemoryPipelineInput, PersonaSettings, now_millis,
};
use yunxi_agent_provider::{AgentProvider, ProviderMessage, ProviderRequest};
use yunxi_agent_storage::{
    FilePersonaMemoryStore, PersonaMemoryScope, SessionRecord, SessionStore,
    SqliteMemoryVectorStore,
};

const DEFAULT_IDLE_SECONDS: u64 = 30 * 60;
const MAX_IDLE_SECONDS: u64 = 24 * 60 * 60;
const MAX_SESSIONS_PER_REFLECTION: usize = 8;
const MAX_SOURCE_CHARS: usize = 32 * 1024;
const REFLECTION_STATE_FILE: &str = "memory-reflection.json";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
struct ReflectionCheckpoint {
    #[serde(default)]
    last_reflected_updated_at_millis: u128,
    #[serde(default)]
    last_reflected_session_id: String,
}

/// Schedule a non-blocking reflection attempt after the configured idle window.
///
/// The task is intentionally best-effort: a daemon that stays alive will run it,
/// while a short-lived CLI invocation simply leaves the checkpoint untouched for
/// the next invocation. No proactive user-facing message is emitted.
pub fn schedule_idle_memory_reflection(
    provider: Arc<dyn AgentProvider>,
    config: AgentConfig,
    storage: Arc<dyn SessionStore>,
) {
    let settings = PersonaSettings::load();
    if !settings.memory_enabled {
        reflection_debug("disabled: YUNXI_MEMORY_ENABLED is false");
        return;
    }
    if !reflection_enabled() {
        reflection_debug("disabled: YUNXI_MEMORY_REFLECTION_ENABLED is false");
        return;
    }
    if config
        .provider
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
        || config
            .model
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        reflection_debug("waiting: provider and model must both be configured");
        return;
    }
    let delay = Duration::from_secs(reflection_idle_seconds());
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if let Err(error) = reflect_if_idle(provider.as_ref(), &config, storage.as_ref()).await {
            eprintln!("yunxi memory reflection skipped: {error}");
        }
    });
}

async fn reflect_if_idle(
    provider: &dyn AgentProvider,
    config: &AgentConfig,
    storage: &dyn SessionStore,
) -> AgentResult<()> {
    let sessions = storage.list().await?;
    let mut completed = sessions
        .into_iter()
        .filter(|session| session.status == AgentRunStatus::Completed)
        .collect::<Vec<_>>();
    completed.sort_by(|left, right| {
        (left.updated_at_millis, left.id.0.as_str())
            .cmp(&(right.updated_at_millis, right.id.0.as_str()))
    });
    let Some(latest) = completed.last() else {
        return Ok(());
    };
    let latest_updated_at_millis = latest.updated_at_millis;
    let latest_session_id = latest.id.0.clone();
    let now = now_millis();
    let idle_at =
        latest_updated_at_millis.saturating_add(u128::from(reflection_idle_seconds()) * 1000);
    if now < idle_at {
        return Ok(());
    }

    let state_path = reflection_state_path(&config.cwd);
    let checkpoint = load_checkpoint(&state_path);
    let mut pending = completed
        .into_iter()
        .filter(|session| is_after_checkpoint(session, &checkpoint))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(());
    }
    if pending.len() > MAX_SESSIONS_PER_REFLECTION {
        pending = pending.split_off(pending.len() - MAX_SESSIONS_PER_REFLECTION);
    }
    let source = render_source(&pending);
    if source.trim().is_empty() {
        return Ok(());
    }
    let source_id = format!("idle-reflection-{latest_session_id}");
    let request = ProviderRequest::with_messages(
        config.clone(),
        yunxi_agent_core::AgentInput::text("YunXi idle memory reflection"),
        vec![
            ProviderMessage::system(
                "You are YunXi's idle memory reflection subsystem. Return JSON only with a top-level candidates array. Extract only stable, user-grounded, low-risk preferences, project context, personal facts, goals, events, or relationship changes from the supplied completed conversations. Never invent facts, never include secrets or credentials, and keep uncertain personal/relationship/emotional/event information pending through policy. The knowledge base is separate: do not return Linux commands or external knowledge entries.",
            ),
            ProviderMessage::user(format!(
                "Completed conversations that have been idle:\n{source}\n\nReturn structured memory candidates only."
            )),
        ],
    )
    .with_tools_enabled(false);
    let response = tokio::time::timeout(Duration::from_secs(15), provider.complete(request))
        .await
        .map_err(|_| AgentError::Execution {
            message: "idle memory reflection timed out".to_string(),
        })??;
    let provider_response = response
        .message
        .map(|message| message.content)
        .unwrap_or_default();
    if provider_response.trim().is_empty() {
        return Err(AgentError::Execution {
            message: "idle memory reflection returned an empty response".to_string(),
        });
    }

    let workspace_fingerprint = FilePersonaMemoryStore::for_workspace(&config.cwd)
        .workspace_fingerprint()
        .to_string();
    let pipeline = MemoryPipeline::new().run(MemoryPipelineInput {
        prompt: format!("idle memory reflection source: {source_id}"),
        assistant_response: None,
        provider_response: Some(provider_response),
        source_session_id: Some(source_id),
        workspace_fingerprint: Some(workspace_fingerprint),
        memory_enabled: true,
    });
    let store = FilePersonaMemoryStore::for_workspace(&config.cwd);
    for candidate in pipeline.candidates {
        if matches!(
            candidate.write_policy,
            yunxi_agent_persona::MemoryWritePolicy::Discard
                | yunxi_agent_persona::MemoryWritePolicy::Disabled
        ) {
            continue;
        }
        store
            .append_or_merge(&candidate.proposed_record)
            .map_err(|error| AgentError::Execution {
                message: format!("idle memory reflection write failed: {error}"),
            })?;
    }
    let loaded = store.list(PersonaMemoryScope::All);
    SqliteMemoryVectorStore::for_workspace(&config.cwd)
        .sync_records(&loaded.records, &LocalChargramEmbedding::default())
        .map_err(|error| AgentError::Execution {
            message: format!("idle memory reflection vector sync failed: {error}"),
        })?;
    save_checkpoint(
        &state_path,
        &ReflectionCheckpoint {
            last_reflected_updated_at_millis: latest_updated_at_millis,
            last_reflected_session_id: latest_session_id,
        },
    )?;
    Ok(())
}

fn render_source(sessions: &[SessionRecord]) -> String {
    let mut output = String::new();
    for session in sessions {
        let response = session.final_response.as_deref().unwrap_or("");
        let entry = format!(
            "[session {} at {}]\nuser: {}\nassistant: {}\n\n",
            session.id.0,
            session.updated_at_millis,
            session.prompt.trim(),
            response.trim()
        );
        if output.len().saturating_add(entry.len()) > MAX_SOURCE_CHARS {
            break;
        }
        output.push_str(&entry);
    }
    output
}

fn is_after_checkpoint(session: &SessionRecord, checkpoint: &ReflectionCheckpoint) -> bool {
    (session.updated_at_millis, session.id.0.as_str())
        > (
            checkpoint.last_reflected_updated_at_millis,
            checkpoint.last_reflected_session_id.as_str(),
        )
}

fn reflection_state_path(cwd: &Path) -> PathBuf {
    cwd.join(".yunxi").join(REFLECTION_STATE_FILE)
}

fn load_checkpoint(path: &Path) -> ReflectionCheckpoint {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn save_checkpoint(path: &Path, checkpoint: &ReflectionCheckpoint) -> AgentResult<()> {
    let parent = path.parent().ok_or_else(|| AgentError::Execution {
        message: "idle memory reflection state path has no parent".to_string(),
    })?;
    std::fs::create_dir_all(parent).map_err(|error| AgentError::Execution {
        message: format!("failed to create memory reflection state directory: {error}"),
    })?;
    let content =
        serde_json::to_string_pretty(checkpoint).map_err(|error| AgentError::Execution {
            message: format!("failed to serialize memory reflection checkpoint: {error}"),
        })?;
    std::fs::write(path, content).map_err(|error| AgentError::Execution {
        message: format!("failed to save memory reflection checkpoint: {error}"),
    })
}

fn reflection_enabled() -> bool {
    std::env::var("YUNXI_MEMORY_REFLECTION_ENABLED")
        .ok()
        .map(|value| parse_env_bool(&value, true))
        .unwrap_or(true)
}

fn reflection_debug(message: &str) {
    if std::env::var("YUNXI_MEMORY_REFLECTION_DEBUG")
        .ok()
        .map(|value| parse_env_bool(&value, false))
        .unwrap_or(false)
    {
        eprintln!("yunxi memory reflection: {message}");
    }
}

fn parse_env_bool(value: &str, default: bool) -> bool {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn reflection_idle_seconds() -> u64 {
    std::env::var("YUNXI_MEMORY_REFLECTION_IDLE_SECONDS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_IDLE_SECONDS)
        .clamp(60, MAX_IDLE_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use yunxi_agent_core::AgentInputModality;

    fn session(id: &str, updated: u128) -> SessionRecord {
        let mut value = SessionRecord::new(
            PathBuf::from("/tmp/workspace"),
            format!("prompt {id}"),
            Some(format!("response {id}")),
            Vec::new(),
        );
        value.id = yunxi_agent_storage::SessionId::new(id);
        value.updated_at_millis = updated;
        value.input_modality = AgentInputModality::Text;
        value
    }

    #[test]
    fn checkpoint_orders_by_timestamp_then_id() {
        let checkpoint = ReflectionCheckpoint {
            last_reflected_updated_at_millis: 10,
            last_reflected_session_id: "b".to_string(),
        };
        assert!(!is_after_checkpoint(&session("a", 10), &checkpoint));
        assert!(is_after_checkpoint(&session("c", 10), &checkpoint));
        assert!(is_after_checkpoint(&session("a", 11), &checkpoint));
    }

    #[test]
    fn source_is_bounded() {
        let mut value = session("one", 1);
        value.prompt = "x".repeat(MAX_SOURCE_CHARS);
        assert!(render_source(&[value]).len() <= MAX_SOURCE_CHARS);
    }

    #[test]
    fn reflection_boolean_parser_matches_persona_settings() {
        assert!(parse_env_bool("true", false));
        assert!(parse_env_bool("YES", false));
        assert!(!parse_env_bool("off", true));
        assert!(parse_env_bool("invalid", true));
        assert!(!parse_env_bool("invalid", false));
    }
}
