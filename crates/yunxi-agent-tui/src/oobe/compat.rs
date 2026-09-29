//! Miyu 前端照搬时需要的**适配层**。
//!
//! `references/REFERENCE-SOURCES.md` 要求任何改编都要「pass a YunXi adapter
//! boundary」——这个模块就是那道边界。移植过来的 `oobe` 代码按 Miyu 的类型名
//! 书写（`AppConfig` / `MiyuPaths` / `FeatureItem` …），这里提供**同名的 YunXi
//! 侧定义**，于是移植只需要改导入路径，不用逐处改写语义。
//!
//! 为什么不全量搬 `miyu-base`：它不含测试是 **29,564 行**，其中 config 10,837、
//! paths 2,934、shell 2,276、models_cache 1,464 —— 大量是 Miyu 特有的后端
//! （多用户 home、legacy 迁移、shell 接管安装器、模型缓存）。而 `oobe/ui`
//! 实际只用到其中极少几个字段。镜像这几个字段比搬 29k 行划算得多，也正好是
//! 「适配边界」的本意。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ═══════════════════════ paths ═══════════════════════

/// 镜像 `miyu_base::paths::MiyuPaths`。
///
/// 字段与 Miyu 同名同义，只是根目录换成 YunXi 的 XDG 布局。
#[derive(Clone, Debug)]
pub struct MiyuPaths {
    pub root_dir: PathBuf,
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
    pub fish_hook_file: PathBuf,
    pub bash_hook_file: PathBuf,
    pub zsh_hook_file: PathBuf,
    pub scripts_dir: PathBuf,
}

impl MiyuPaths {
    /// 从 YunXi 自己的 XDG 布局推导。**不读 Miyu 的环境变量**。
    pub fn new() -> std::io::Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let xdg = |key: &str, fallback: PathBuf| {
            std::env::var_os(key)
                .map(PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(fallback)
        };
        let config_dir = xdg("XDG_CONFIG_HOME", home.join(".config")).join("yunxi");
        let data_dir = xdg("XDG_DATA_HOME", home.join(".local/share")).join("yunxi");
        let cache_dir = xdg("XDG_CACHE_HOME", home.join(".cache")).join("yunxi");
        let state_dir = xdg("XDG_STATE_HOME", home.join(".local/state")).join("yunxi");
        let scripts_dir = data_dir.join("scripts");
        Ok(Self {
            root_dir: data_dir.clone(),
            config_file: config_dir.join("environment"),
            fish_hook_file: config_dir.join("fish-hook.fish"),
            bash_hook_file: config_dir.join("bash-hook.sh"),
            zsh_hook_file: config_dir.join("zsh-hook.zsh"),
            scripts_dir,
            config_dir,
            data_dir,
            cache_dir,
            state_dir,
        })
    }

    pub fn create_dirs(&self) -> std::io::Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.state_dir,
            &self.scripts_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn personas_dir(&self) -> PathBuf {
        self.data_dir.join("persona")
    }

    pub fn documents_dir(&self) -> PathBuf {
        self.data_dir.join("documents")
    }
}

/// 镜像 `miyu_base::paths::program_available`。
pub fn program_available(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(name);
        candidate.is_file()
            || candidate.with_extension("exe").is_file()
            || candidate.with_extension("cmd").is_file()
    })
}

// ═══════════════════════ sandbox ═══════════════════════

/// 镜像 `miyu_base::sandbox::probe`：探到的沙盒能力。`None` = 这台机器没有。
#[derive(Clone, Debug)]
pub struct SandboxFacts {
    /// 内核/发行版给出的沙盒名（bwrap / landlock / seatbelt …）。
    pub kind: String,
    /// 人类可读的一句说明。
    pub note: String,
}

/// 探一次沙盒能力。YunXi 用 bubblewrap 与 Landlock。
pub fn probe() -> Option<SandboxFacts> {
    if program_available("bwrap") {
        return Some(SandboxFacts {
            kind: "bubblewrap".to_string(),
            note: "bwrap 可用".to_string(),
        });
    }
    // Landlock 是内核能力，没有二进制可探；这里只认能确认的那一档。
    None
}

// ═══════════════════════ config ═══════════════════════

/// 镜像 `miyu_base::config::ProviderConfig`（只保留前端用到的字段）。
#[derive(Clone, Debug, Default)]
pub struct ProviderConfig {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
}

/// 镜像 `miyu_base::config::McpServerConfig`。
#[derive(Clone, Debug, Default)]
pub struct McpServerConfig {
    pub id: String,
    pub display_name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub timeout_seconds: u64,
}

/// 镜像 `miyu_base::config::ActiveProviderModelConfig`。
#[derive(Clone, Debug, Default)]
pub struct ActiveProviderModelConfig {
    pub model: String,
}

/// 镜像 `miyu_base::config::ToolsConfig`（前端只读它的开关）。
#[derive(Clone, Debug, Default)]
pub struct ToolsConfig {
    pub network: bool,
    pub vision: bool,
}

/// 镜像 `miyu_base::config::McpConfig`。
#[derive(Clone, Debug, Default)]
pub struct McpConfig {
    pub servers: Vec<McpServerConfig>,
}

/// 镜像 `miyu_base::config::AppConfig`。
///
/// **只镜像 `oobe/ui` 真正读写的字段**。Miyu 那份是 10,837 行、覆盖整个产品的
/// 配置面；前端用不到的（embedding / cache / display / 迁移相关）一律不搬。
#[derive(Clone, Debug, Default)]
pub struct AppConfig {
    pub config_version: u32,
    pub active_provider: String,
    pub providers: Vec<ProviderConfig>,
    pub tools: ToolsConfig,
    pub mcp: McpConfig,
    /// 人格屏保存后的 scope；功能屏按它读写 persona.toml。
    pub active_persona_scope: String,
    /// 原始 JSONC 文本（照原样保留，写回时不丢字段）。
    pub jsonc: String,
}

impl AppConfig {
    /// 从 YunXi 的既有配置构造。
    pub fn from_yunxi(provider: &str, model: &str) -> Self {
        Self {
            config_version: 1,
            active_provider: provider.to_string(),
            providers: vec![ProviderConfig {
                id: provider.to_string(),
                display_name: provider.to_string(),
                models: vec![model.to_string()],
                ..ProviderConfig::default()
            }],
            ..AppConfig::default()
        }
    }
}

/// 镜像 `miyu_base::config::persona_scope_name`。
pub fn persona_scope_name(scope: &str) -> &str {
    if scope.is_empty() { "default" } else { scope }
}

/// 镜像 `miyu_base::config::PersonaManifest`（人格清单，前端只读它的名字与内置件）。
#[derive(Clone, Debug, Default)]
pub struct PersonaManifest {
    pub id: String,
    pub name: String,
    pub builtin_plugins: Vec<String>,
    pub builtin_skills: Vec<String>,
}

// ═══════════════════════ feature_catalog ═══════════════════════

pub mod feature_catalog {
    use super::{AppConfig, PersonaManifest};

    /// 镜像 `FeatureKind`。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum FeatureKind {
        Subsystem,
        Plugin,
        Script,
        Skill,
        Mcp,
        Machine,
    }

    /// 镜像 `FeatureItem`。
    #[derive(Clone, Debug, Default)]
    pub struct FeatureItem {
        pub kind_id: usize,
        pub id: String,
        pub name: String,
        pub hint: String,
        pub on: bool,
        pub builtin: bool,
        pub settings: bool,
        pub machine_on: Option<bool>,
    }

    /// 镜像 `FeatureSources`。
    #[derive(Clone, Debug, Default)]
    pub struct FeatureSources {
        pub scripts: Vec<String>,
        pub skills: Vec<String>,
    }

    /// 镜像 `CatalogScope`。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum CatalogScope {
        Onboarding,
        Settings,
    }

    impl CatalogScope {
        pub fn everything(self) -> bool {
            matches!(self, CatalogScope::Settings)
        }
    }

    /// 镜像 `catalog()`。YunXi 的能力清单按自己的子系统列。
    pub fn catalog(
        _manifest: &PersonaManifest,
        _sources: &FeatureSources,
        _default_persona: bool,
        _scope: CatalogScope,
        _config: Option<&AppConfig>,
    ) -> Vec<FeatureItem> {
        // 引导里摆的能力项：与 YunXi 自己的子系统一一对应。
        [
            (1usize, "memory", "分层记忆", "记住你说过的事，按需召回"),
            (2, "knowledge", "知识库", "把你的文档接入本地向量检索"),
            (3, "companion", "陪伴", "关系阶段与主动关心"),
            (4, "love_letters", "情书", "定时的信件与信箱"),
        ]
        .into_iter()
        .map(|(kind_id, id, name, hint)| FeatureItem {
            kind_id,
            id: id.to_string(),
            name: name.to_string(),
            hint: hint.to_string(),
            on: false,
            builtin: true,
            settings: true,
            machine_on: None,
        })
        .collect()
    }
}

// ═══════════════════════ 引导流程用到的几个判定 ═══════════════════════

/// 镜像 `miyu_core::skills::is_default_persona`：当前是不是出厂人格。
///
/// Miyu 据此决定引导里摆哪些能力项（自定义人格下内置件默认不勾）。YunXi 的
/// 等价物是「有没有自定义 soul / profile」。
pub fn is_default_persona(config: &AppConfig) -> bool {
    config.active_persona_scope.is_empty() || config.active_persona_scope == "default"
}

/// 镜像 `crate::feature_sources::collect`：探外装件（脚本、技能、语音）。
///
/// Miyu 会去扫目录、探二进制；YunXi 的能力来自自己的子系统，所以这里返回空清单 ——
/// 「谁调谁给」那条约定反过来用：YunXi 不需要外装件这一层。
pub fn feature_sources_collect(
    _config: &AppConfig,
    _paths: &MiyuPaths,
) -> feature_catalog::FeatureSources {
    feature_catalog::FeatureSources::default()
}

/// 镜像 `miyu_base::default_models::OPENCODE_PROVIDER_ID`。
pub const OPENCODE_PROVIDER_ID: &str = "opencode";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_come_from_yunxi_xdg_not_miyu() {
        // 适配层绝不能读 Miyu 的目录；同时把 HOME 指到临时目录后，
        // 所有路径都必须落在它下面。
        let home = std::env::temp_dir().join("yunxi-compat-paths");
        // SAFETY: 单测试内读，且只是探测路径拼接。
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("XDG_DATA_HOME");
            std::env::remove_var("XDG_STATE_HOME");
            std::env::remove_var("XDG_CACHE_HOME");
        }
        let paths = MiyuPaths::new().expect("paths");
        assert!(
            paths.config_dir.starts_with(&home),
            "{:?}",
            paths.config_dir
        );
        assert!(paths.data_dir.starts_with(&home), "{:?}", paths.data_dir);
        assert!(paths.state_dir.starts_with(&home), "{:?}", paths.state_dir);
        // 根目录不能是 Miyu 的 `~/.miyu`。
        let text = paths.root_dir.to_string_lossy().to_string();
        assert!(!text.contains(".miyu"), "适配层读到了 Miyu 的目录: {text}");
        assert!(text.contains("yunxi"), "适配层没落到 YunXi 目录: {text}");
    }

    #[test]
    fn catalog_lists_yunxi_subsystems() {
        let items = feature_catalog::catalog(
            &PersonaManifest::default(),
            &feature_catalog::FeatureSources::default(),
            true,
            feature_catalog::CatalogScope::Onboarding,
            None,
        );
        assert!(items.len() >= 4, "引导至少要摆出记忆/知识库/陪伴/情书");
        assert!(items.iter().any(|item| item.id == "memory"));
        assert!(items.iter().any(|item| item.id == "knowledge"));
    }

    #[test]
    fn probe_never_panics_on_a_missing_binary() {
        // 探不到就返回 None，不 panic、不返回假阳性。
        let _ = probe();
        assert!(!program_available("definitely-not-a-real-binary-xyz"));
    }
}
