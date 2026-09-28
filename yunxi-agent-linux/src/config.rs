use yunxi_agent_core::AgentConfig;
use yunxi_agent_persona::PersonaSettings;

/// Keep the Linux host's long-lived context usable without requiring a model-
/// specific config file.  The values are intentionally conservative and can
/// be overridden with environment variables when a provider exposes a larger
/// or smaller context window.
pub(crate) const DEFAULT_CONTEXT_WINDOW_TOKENS: i64 = 32_000;
pub(crate) const DEFAULT_AUTO_COMPACT_THRESHOLD_TOKENS: i64 = 24_000;

pub(crate) fn apply_linux_runtime_settings(config: &mut AgentConfig) {
    let persona = PersonaSettings::load();
    apply_persona_settings(config, &persona);

    let context_window =
        positive_env("YUNXI_CONTEXT_WINDOW_TOKENS").unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS);
    let compact_threshold = positive_env("YUNXI_AUTO_COMPACT_THRESHOLD_TOKENS")
        .unwrap_or(DEFAULT_AUTO_COMPACT_THRESHOLD_TOKENS);
    apply_context_budget(config, Some(context_window), Some(compact_threshold));
}

pub(crate) fn apply_persona_settings(config: &mut AgentConfig, settings: &PersonaSettings) {
    config.companion.enabled = settings.companion_enabled;
    config.companion.cloud_control_enabled = settings.cloud_control_enabled;
    config.companion.love_letters.enabled = settings.love_letters_enabled;
}

pub(crate) fn apply_context_budget(
    config: &mut AgentConfig,
    context_window_tokens: Option<i64>,
    auto_compact_threshold_tokens: Option<i64>,
) {
    let context_window_tokens = context_window_tokens
        .or(config.context_window_tokens)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS)
        .max(1);
    let auto_compact_threshold_tokens = auto_compact_threshold_tokens
        .or(config.auto_compact_threshold_tokens)
        .unwrap_or(DEFAULT_AUTO_COMPACT_THRESHOLD_TOKENS)
        .max(1)
        .min(context_window_tokens);
    config.context_window_tokens = Some(context_window_tokens);
    config.auto_compact_threshold_tokens = Some(auto_compact_threshold_tokens);
}

fn positive_env(name: &str) -> Option<i64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_switches_are_bridged_into_agent_config() {
        let mut config = AgentConfig::new(".");
        let settings = PersonaSettings {
            companion_enabled: true,
            love_letters_enabled: true,
            cloud_control_enabled: true,
            ..PersonaSettings::default()
        };

        apply_persona_settings(&mut config, &settings);

        assert!(config.companion.enabled);
        assert!(config.companion.love_letters.enabled);
        assert!(config.companion.cloud_control_enabled);
    }

    #[test]
    fn context_budget_is_enabled_and_threshold_is_bounded() {
        let mut config = AgentConfig::new(".");

        apply_context_budget(&mut config, Some(8_000), Some(12_000));

        assert_eq!(config.context_window_tokens, Some(8_000));
        assert_eq!(config.auto_compact_threshold_tokens, Some(8_000));
    }
}
