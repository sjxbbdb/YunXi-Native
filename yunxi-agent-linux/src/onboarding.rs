//! 首次配置向导的**宿主侧**实现（方案第三节 · T3）。
//!
//! 分工（方案第三节「约定」）：
//! - TUI 侧（`yunxi_agent_tui::onboarding`）只负责渲染与键盘收集，返回
//!   [`OnboardingAnswers`] / [`OnboardingOutcome`]，**不碰文件系统、不碰网络**；
//! - 本模块负责「已配置」判据、五个步骤的定义、API key 当场验证、
//!   写 `~/.config/yunxi/environment`（保持 600）、写 `PersonaSettings`、
//!   把用户卡片写进**既有记忆系统**、写完成标记、以及跳过时的后果提示。
//!
//! 对外只有一处依赖 TUI 的运行时能力：[`WizardDriver`]（由
//! `YunxiTui::run_onboarding` 实现）。抽成 trait 的目的不是抽象癖，而是让
//! 「五步怎么排、验证失败怎么重试、跳过时提示什么」能在单测里用假向导跑通，
//! 不必为了测一句提示而开一个真终端。
//!
//! # 为什么要分两段跑向导
//!
//! 方案要求第 1 步「当场发一次最小请求验证，连不通就让用户改，**别放过去**」。
//! 而冻结的 TUI 契约是「一次 `run_onboarding` 只在最后返回一次答案」，没有
//! 「中途回调宿主验证」的入口。于是宿主把向导拆成两段：
//!
//! 1. **连接段**：API key / base url / 模型名 → 立即验证；失败就带着原因重开这一段，
//!    直到验证通过（或用户在段内按 Esc 跳过）；
//! 2. **人格段**：人格 / 关于你 / 记忆库 / 知识库 → 收集完一次性落盘。
//!
//! 这样坏 key 根本没机会走到第 2 步，也不需要改 TUI 的接口。

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use yunxi_agent_core::{AgentConfig, AgentError};
use yunxi_agent_persona::memory::{MemoryKind, MemoryRecord, MemoryScope, MemoryStatus};
use yunxi_agent_persona::{
    DEFAULT_PROFILE_ID, PersonaProfileStore, PersonaSettings, now_millis, yunxi_home_dir,
};
use yunxi_agent_provider::{
    AgentProvider, ProviderAuth, ProviderBootstrap, ProviderConfig, ProviderRequest,
};
use yunxi_agent_storage::{FilePersonaMemoryStore, PersonaMemoryScope};
use yunxi_agent_tui::{
    ChoiceOption, OnboardingOutcome, OnboardingStep, OnboardingStepId, StepKind,
};

// ─────────────────────────── 一、路径与常量 ───────────────────────────

/// 判据①的完成标记文件名（放在 `~/.local/state/yunxi/` 下）。
///
/// 刻意**不复用** `first-run-complete`：后者只表示「跑过一次检查」，
/// 与「配置真的完成了」是两件事（方案第一节）。
pub const MARKER_FILE: &str = "onboarding-complete";

/// shell 环境文件名；`~/.config/yunxi/env` 通常是指向它的软链。
pub const ENV_FILE_NAME: &str = "environment";
/// shell 环境文件的别名（软链名）。
pub const ENV_FILE_ALIAS: &str = "env";

/// `PersonaSettings` 的文件名（`<yunxi_home>/persona/config.toml`）。
pub const PERSONA_SETTINGS_FILE: &str = "config.toml";
/// 宿主自己的向导记录（知识库开关等 `PersonaSettings` 没有字段的项）。
pub const PERSONA_ONBOARDING_FILE: &str = "onboarding.toml";

/// 凭证变量名（`ProviderBootstrap` 读的就是它，见 provider crate）。
pub const ENV_API_KEY: &str = "YUNXI_PROVIDER_API_KEY";
/// base url 覆盖变量名。
pub const ENV_BASE_URL: &str = "YUNXI_PROVIDER_BASE_URL";
/// 模型名变量名；provider 读的是 `YUNXI_AGENT_MODEL`（不是 `YUNXI_PROVIDER_MODEL`）。
pub const ENV_MODEL: &str = "YUNXI_AGENT_MODEL";

/// environment 文件必须保持的权限位。
pub const ENV_FILE_MODE: u32 = 0o600;

/// 验证用的最短请求文本。
const PROBE_PROMPT: &str = "你好";
/// 验证请求的超时（毫秒）。向导是交互场景，不能让用户干等两分钟。
const PROBE_TIMEOUT_MILLIS: u64 = 20_000;

/// 向导第一段（连接模型）的标题，跳过提示里要用。
pub const PHASE_PROVIDER: &str = "连接模型";
/// 向导第二段（人格与偏好）的标题。
pub const PHASE_PROFILE: &str = "人格与偏好";

/// 向导涉及的全部落盘路径。
///
/// 每个字段都可以显式构造，测试因此不必改进程环境就能指向临时目录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OnboardingPaths {
    /// `~/.local/state/yunxi`
    pub state_dir: PathBuf,
    /// `~/.config/yunxi`
    pub config_dir: PathBuf,
    /// `~/.config/yunxi/environment`（或已存在的 `env` 软链）
    pub environment_file: PathBuf,
    /// `<yunxi_home>/persona/config.toml`（`PersonaSettings`）
    pub persona_settings: PathBuf,
    /// `<yunxi_home>/persona/onboarding.toml`（宿主自己的向导记录）
    pub persona_onboarding: PathBuf,
    /// `~/.local/state/yunxi/onboarding-complete`
    pub marker: PathBuf,
}

impl OnboardingPaths {
    /// 用宿主已经算好的 state 目录解析其余路径。
    ///
    /// `yunxi_home_dir()` 与 `XDG_CONFIG_HOME` 都是进程环境变量，与
    /// `main.rs::prepare_storage_layout` 用的是同一套规则，因此两边看到的
    /// 一定是同一个目录。
    pub fn resolve(state_dir: impl Into<PathBuf>) -> Self {
        let state_dir = state_dir.into();
        let config_dir = xdg_config_dir();
        let home = yunxi_home_dir();
        Self {
            marker: state_dir.join(MARKER_FILE),
            persona_settings: home.join("persona").join(PERSONA_SETTINGS_FILE),
            persona_onboarding: home.join("persona").join(PERSONA_ONBOARDING_FILE),
            environment_file: pick_environment_file(&config_dir),
            state_dir,
            config_dir,
        }
    }

    /// 测试与显式注入用：给定三个根目录算出全部路径。
    pub fn with_roots(
        state_dir: impl Into<PathBuf>,
        config_dir: impl Into<PathBuf>,
        yunxi_home: impl Into<PathBuf>,
    ) -> Self {
        let state_dir = state_dir.into();
        let config_dir = config_dir.into();
        let yunxi_home = yunxi_home.into();
        Self {
            marker: state_dir.join(MARKER_FILE),
            persona_settings: yunxi_home.join("persona").join(PERSONA_SETTINGS_FILE),
            persona_onboarding: yunxi_home.join("persona").join(PERSONA_ONBOARDING_FILE),
            environment_file: config_dir.join(ENV_FILE_NAME),
            state_dir,
            config_dir,
        }
    }
}

/// `~/.config/yunxi`（尊重 `XDG_CONFIG_HOME`）。
pub fn xdg_config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("yunxi")
}

/// `environment` 优先；只有别名 `env` 存在时用它（保持用户既有的软链布局）。
fn pick_environment_file(config_dir: &Path) -> PathBuf {
    let primary = config_dir.join(ENV_FILE_NAME);
    if primary.is_file() {
        return primary;
    }
    let alias = config_dir.join(ENV_FILE_ALIAS);
    if alias.is_file() {
        return alias;
    }
    primary
}

// ─────────────────────────── 二、「已配置」判据 ───────────────────────────

/// 判据缺失项。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissingItem {
    /// ① `onboarding-complete` 标记
    Marker,
    /// ② Provider 凭证
    Credentials,
    /// ③ `PersonaSettings`
    PersonaSettings,
}

impl MissingItem {
    /// 一句话说明「缺了什么」。
    pub fn label(self) -> &'static str {
        match self {
            Self::Marker => "完成标记",
            Self::Credentials => "Provider 凭证",
            Self::PersonaSettings => "人格设置",
        }
    }

    /// 缺了它的**具体后果**（跳过提示要用，不能含糊）。
    pub fn consequence(self) -> &'static str {
        match self {
            Self::Marker => "完成标记未写入：下次启动 yunxi-linux 仍会进入配置向导。",
            Self::Credentials => {
                "未配置 API key：云熙将无法调用模型，只会走本地静态 Runtime（离线固定回复，不会真的回答你）。"
            }
            Self::PersonaSettings => {
                "未选择人格：沿用内置默认人格 yunxi_companion_strong，且 memory_enabled 默认 false，记忆库不会开启。"
            }
        }
    }

    /// 怎么补。
    pub fn fix_hint(self) -> &'static str {
        match self {
            Self::Marker => "重新运行 yunxi-linux 走完向导即可写入标记。",
            Self::Credentials => {
                "重新运行 yunxi-linux 走向导的第 1 步，或手工编辑 ~/.config/yunxi/environment（权限保持 600）。"
            }
            Self::PersonaSettings => {
                "重新运行 yunxi-linux 走向导的人格一步，或编辑 <yunxi_home>/persona/config.toml。"
            }
        }
    }
}

/// 三条判据的实际观测结果。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConfigStatus {
    /// ① `~/.local/state/yunxi/onboarding-complete` 存在
    pub marker: bool,
    /// ② `YUNXI_PROVIDER_API_KEY` 有值（进程环境或 environment 文件）
    pub credentials: bool,
    /// ③ `PersonaSettings` 文件存在
    pub persona_settings: bool,
}

impl ConfigStatus {
    /// **三者全满足**才算已配置（方案第一节）。
    pub fn is_configured(&self) -> bool {
        self.marker && self.credentials && self.persona_settings
    }

    /// 缺失项列表（按上面的判据顺序）。
    pub fn missing(&self) -> Vec<MissingItem> {
        let mut items = Vec::new();
        if !self.marker {
            items.push(MissingItem::Marker);
        }
        if !self.credentials {
            items.push(MissingItem::Credentials);
        }
        if !self.persona_settings {
            items.push(MissingItem::PersonaSettings);
        }
        items
    }
}

/// 按三条判据检测（显式传入进程环境里的 key，便于测试）。
pub fn detect(paths: &OnboardingPaths, env_api_key: Option<&str>) -> ConfigStatus {
    ConfigStatus {
        marker: paths.marker.is_file(),
        credentials: credentials_available(env_api_key, &paths.environment_file),
        persona_settings: paths.persona_settings.is_file(),
    }
}

/// 用当前进程环境检测。
pub fn detect_from_process(paths: &OnboardingPaths) -> ConfigStatus {
    detect(paths, std::env::var(ENV_API_KEY).ok().as_deref())
}

/// 判据②：进程环境有值，**或** environment 文件里有非空的 `YUNXI_PROVIDER_API_KEY=`。
pub fn credentials_available(env_api_key: Option<&str>, environment_file: &Path) -> bool {
    env_api_key.is_some_and(|value| !value.trim().is_empty())
        || read_environment_api_key(environment_file).is_some_and(|value| !value.trim().is_empty())
}

/// 从 environment 文件里取 `YUNXI_PROVIDER_API_KEY` 的值（后出现的定义生效，与 shell 一致）。
pub fn read_environment_api_key(path: &Path) -> Option<String> {
    read_environment_vars(path)
        .remove(ENV_API_KEY)
        .filter(|value| !value.trim().is_empty())
}

/// 把 environment 文件解析成 `变量名 → 值`。
///
/// 只认 `KEY=value` 与 `export KEY=value`，注释行忽略；值支持单/双引号。
/// 解析失败的行直接跳过 —— 这个文件是用户手写的，注释和空行都属于正常内容。
pub fn read_environment_vars(path: &Path) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    let Ok(content) = fs::read_to_string(path) else {
        return vars;
    };
    for line in content.lines() {
        if let Some((key, value)) = parse_assignment(line) {
            vars.insert(key, value);
        }
    }
    vars
}

/// 解析一行 shell 赋值，返回 `(变量名, 值)`；不是赋值就返回 `None`。
fn parse_assignment(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let trimmed = trimmed
        .strip_prefix("export ")
        .map(str::trim_start)
        .unwrap_or(trimmed);
    let (key, rest) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty() || !is_shell_name(key) {
        return None;
    }
    Some((key.to_string(), parse_shell_value(rest)))
}

/// 取赋值右侧的值：剥掉成对引号；裸值遇到 ` #` 就当行尾注释。
fn parse_shell_value(rest: &str) -> String {
    let rest = rest.trim_start();
    for quote in ['"', '\''] {
        if let Some(body) = rest.strip_prefix(quote) {
            return match body.split_once(quote) {
                Some((value, _)) => value.to_string(),
                None => body.to_string(),
            };
        }
    }
    match rest.split_once(" #") {
        Some((value, _)) => value.trim_end().to_string(),
        None => rest.trim_end().to_string(),
    }
}

fn is_shell_name(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

// ─────────────────────────── 三、environment 落盘（600） ───────────────────────────

/// 就地更新（或追加）若干变量，写完仍是 600，且**不动任何既有内容**。
///
/// 这是 `~/.config/yunxi/environment` 的唯一写入口，规则：
/// - 同名变量**就地替换**那一行（保留文件里原有的注释、空行、其它变量与顺序）；
/// - 文件里没有的变量**追加**到末尾；
/// - 落盘走「同目录临时文件 + `rename`」（原子替换），临时文件一开始就是 600，
///   写完再 `set_permissions` 复核一次，权限不是 600 直接报错；
/// - 路径是软链时先解析到真实文件，避免把软链本身换掉（`env -> environment`）。
pub fn upsert_environment(path: &Path, updates: &[(&str, String)]) -> Result<()> {
    let target = resolve_write_target(path)?;
    let existing = match fs::read_to_string(&target) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("无法读取环境文件: {}", target.display()));
        }
    };

    let mut lines: Vec<String> = if existing.is_empty() {
        Vec::new()
    } else {
        existing.lines().map(str::to_string).collect()
    };
    let mut handled = vec![false; updates.len()];
    for line in lines.iter_mut() {
        let Some((key, _)) = parse_assignment(line) else {
            continue;
        };
        if let Some(index) = updates.iter().position(|(name, _)| *name == key) {
            *line = render_shell_assignment(updates[index].0, &updates[index].1);
            handled[index] = true;
        }
    }
    for (index, (name, value)) in updates.iter().enumerate() {
        if !handled[index] {
            lines.push(render_shell_assignment(name, value));
        }
    }

    let mut content = lines.join("\n");
    content.push('\n');
    write_private_file(&target, &content)?;
    Ok(())
}

/// 渲染一行 shell 赋值；值里有特殊字符时才加引号（保持既有文件的朴素风格）。
fn render_shell_assignment(name: &str, value: &str) -> String {
    if value.is_empty() {
        return format!("{name}=");
    }
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "._-/:@+".contains(ch))
    {
        return format!("{name}={value}");
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`");
    format!("{name}=\"{escaped}\"")
}

/// 软链就写到它指向的真实文件，普通文件原样返回。
fn resolve_write_target(path: &Path) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            // 不能用 `canonicalize`：它要求目标**已经存在**，而用户常见的布局是
            // 先建软链 `env -> environment`、文件还没写。那种悬空软链上
            // `canonicalize` 必然失败，向导会在第一次写配置时就报错。
            //
            // 改为读链接本身并解析相对目标（与 `ln -s` 的语义一致）：
            // 目标是相对路径时按**软链所在目录**解析。
            let target =
                fs::read_link(path).with_context(|| format!("无法读取软链: {}", path.display()))?;
            if target.is_absolute() {
                Ok(target)
            } else {
                let base = path.parent().unwrap_or_else(|| Path::new("."));
                Ok(base.join(target))
            }
        }
        _ => Ok(path.to_path_buf()),
    }
}

/// 写一个 0600 的文件：同目录临时文件 + rename，写完复核权限。
fn write_private_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建目录: {}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "yunxi".to_string());
    let temporary = path.with_file_name(format!(".{file_name}.yunxi-tmp-{}", std::process::id()));

    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(ENV_FILE_MODE);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("无法写入临时文件: {}", temporary.display()))?;
    let result = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("无法写入临时文件: {}", temporary.display()));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(ENV_FILE_MODE))
            .with_context(|| format!("无法设置权限: {}", temporary.display()))?;
    }
    fs::rename(&temporary, path).with_context(|| format!("无法替换文件: {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(ENV_FILE_MODE))
            .with_context(|| format!("无法设置权限: {}", path.display()))?;
        let mode = fs::metadata(path)
            .with_context(|| format!("无法读取权限: {}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        if mode != ENV_FILE_MODE {
            bail!(
                "环境文件权限必须是 {:o}，实际是 {:o}: {}",
                ENV_FILE_MODE,
                mode,
                path.display()
            );
        }
    }
    Ok(())
}

/// 文件权限（测试与状态展示用）。
pub fn file_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return fs::metadata(path)
            .ok()
            .map(|metadata| metadata.permissions().mode() & 0o777);
    }
    #[allow(unreachable_code)]
    None
}

// ─────────────────────────── 四、五步的定义 ───────────────────────────

/// 第一段：先连上模型（方案第 1 步，三个小问）。
pub fn api_steps() -> Vec<OnboardingStep> {
    vec![
        OnboardingStep::masked(
            OnboardingStepId::ApiKey,
            "先连上模型",
            "没有凭证后面全是空谈。粘贴你的 API key，回车后我会当场发一条最小的请求验证它。",
            "sk-...",
        ),
        OnboardingStep::new(
            OnboardingStepId::ApiBaseUrl,
            "接口地址",
            "自建网关或代理才需要改这里；回车确认后同样会参与验证。",
            StepKind::Text {
                masked: false,
                placeholder: "https://api.deepseek.com/v1".to_string(),
            },
        ),
        OnboardingStep::new(
            OnboardingStepId::ApiModel,
            "模型名",
            "服务端真实存在的模型名，验证请求会带着它发出去。",
            StepKind::Text {
                masked: false,
                placeholder: "deepseek-chat".to_string(),
            },
        ),
    ]
}

/// 第二段：人格 / 关于你 / 记忆库 / 知识库（方案第 2-5 步）。
pub fn profile_steps(profile_options: Vec<ChoiceOption>) -> Vec<OnboardingStep> {
    vec![
        OnboardingStep::choice(
            OnboardingStepId::Profile,
            "挑一个人格",
            "人格决定我说话的语气，以及我会主动在意什么。",
            profile_options,
        ),
        OnboardingStep::text(
            OnboardingStepId::UserCard,
            "关于你",
            "称呼 / 身份 / 偏好三句会写进长期记忆，以后不用反复自我介绍。",
        ),
        OnboardingStep::yes_no(
            OnboardingStepId::Memory,
            "记忆库",
            "关掉也能正常对话，只是我记不住你的习惯和偏好。",
            true,
        ),
        OnboardingStep::yes_no(
            OnboardingStepId::Knowledge,
            "知识库",
            "现在不接也行，之后随时可以用 yunxi-linux knowledge-man 采集。",
            false,
        ),
    ]
}

/// 从 persona registry 读可选人格，转成向导选项。
///
/// 列表顺序即展示顺序：内置 profile 永远排第一（`PersonaProfileStore::list()`
/// 的契约），因此不做任何选择时默认人格就是内置的那个。
pub fn persona_options() -> Vec<ChoiceOption> {
    match PersonaProfileStore::list() {
        Ok(profiles) if !profiles.is_empty() => profiles
            .into_iter()
            .map(|profile| {
                let label = if profile.id == DEFAULT_PROFILE_ID {
                    format!("云熙 · 强陪伴（内置 {}）", profile.display_name)
                } else {
                    profile.display_name.clone()
                };
                ChoiceOption::new(
                    profile.id.clone(),
                    label,
                    summarize_profile(&profile.id, &profile.layers.identity),
                )
            })
            .collect(),
        _ => vec![ChoiceOption::new(
            DEFAULT_PROFILE_ID,
            "云熙 · 强陪伴（内置）",
            "内置人格：话多一点，会主动关心你今天过得怎么样。",
        )],
    }
}

/// 选项说明：取人格 self-identity 的首行，太长就截断（列表里放不下一整段）。
fn summarize_profile(id: &str, identity: &str) -> String {
    let first_line = identity
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if first_line.is_empty() {
        return format!("人格 profile：{id}");
    }
    let mut summary = String::new();
    for character in first_line.chars() {
        if summary.chars().count() >= 34 {
            summary.push('…');
            break;
        }
        summary.push(character);
    }
    summary
}

/// 步骤的中文短名（提示里指名道姓用）。
pub fn step_label(id: &OnboardingStepId) -> String {
    match id {
        OnboardingStepId::ApiKey => "API key".to_string(),
        OnboardingStepId::ApiBaseUrl => "接口地址".to_string(),
        OnboardingStepId::ApiModel => "模型名".to_string(),
        OnboardingStepId::Profile => "人格".to_string(),
        OnboardingStepId::UserCard => "关于你（称呼 / 身份 / 偏好）".to_string(),
        OnboardingStepId::UserName => "称呼".to_string(),
        OnboardingStepId::UserIdentity => "身份".to_string(),
        OnboardingStepId::UserPreference => "偏好".to_string(),
        OnboardingStepId::Memory => "记忆库".to_string(),
        OnboardingStepId::Knowledge => "知识库".to_string(),
        OnboardingStepId::Custom(name) if name.is_empty() => "未命名步骤".to_string(),
        OnboardingStepId::Custom(name) => name.clone(),
    }
}

// ─────────────────────────── 五、验证 API key ───────────────────────────

/// 待验证的凭证三元组。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderProbe {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

/// 验证成功后的证据（用在提示里，让用户相信真的连通了）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyReport {
    /// 实际请求用的模型名
    pub model: String,
    /// 服务端返回的正文摘要（已截断）
    pub reply: String,
}

/// 用**用户填的** key / base url / model 发一条最短请求。
///
/// 走的是 runtime 同一条 provider 栈（`ProviderBootstrap` + reqwest 传输），
/// 所以「验证通过」与「会话真的能用」是同一件事，不存在两套判断标准。
/// 关掉工具调用，把请求压到最小；只发一句「你好」，不做任何会话写入。
pub async fn verify_provider(probe: &ProviderProbe, cwd: &Path) -> Result<VerifyReport> {
    let api_key = probe.api_key.trim();
    if api_key.is_empty() {
        bail!("API key 为空：没有凭证无法调用模型");
    }
    let model = probe.model.trim();
    if model.is_empty() {
        bail!("模型名为空：请填写服务端真实存在的模型名");
    }
    let base_url = normalize_base_url(&probe.base_url);
    if base_url.is_empty() {
        bail!("接口地址为空：请填写 provider 的 base url");
    }

    let config = ProviderConfig::openai_compatible(model)
        .with_base_url(base_url.clone())
        .with_stream(false)
        .with_timeout_millis(Some(PROBE_TIMEOUT_MILLIS));
    let bootstrap = ProviderBootstrap {
        config,
        auth: ProviderAuth::ApiKey(api_key.to_string()),
    };
    let provider = bootstrap.into_openai_transport_provider();

    let agent_config = AgentConfig::new(cwd).with_model(model);
    let request = ProviderRequest::new(
        agent_config,
        yunxi_agent_core::AgentInput::text(PROBE_PROMPT),
    )
    .with_tools_enabled(false);

    match provider.complete(request).await {
        Ok(response) => {
            let reply = response
                .message
                .as_ref()
                .map(|message| message.content.trim().to_string())
                .unwrap_or_default();
            Ok(VerifyReport {
                model: model.to_string(),
                reply: truncate_chars(&reply, 40),
            })
        }
        Err(error) => Err(anyhow::anyhow!("{}", describe_provider_error(&error))),
    }
}

/// base url 规范化：去掉尾部 `/`；用户只填域名时补 `/v1`（OpenAI 兼容端点惯例）。
pub fn normalize_base_url(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    // 已经带路径（https://host/v1、https://host/openai/v1 …）就原样使用。
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    if without_scheme.contains('/') {
        return trimmed.to_string();
    }
    format!("{trimmed}/v1")
}

fn truncate_chars(value: &str, limit: usize) -> String {
    let mut output = String::new();
    for character in value.chars().take(limit) {
        output.push(character);
    }
    if value.chars().count() > limit {
        output.push('…');
    }
    output
}

/// 把 provider 错误翻译成「用户下一步该改什么」。
///
/// 这一层的存在意义就是方案里那句「失败要能重试，不要直接把错误吞掉」：
/// `AgentError` 的 `Display` 是给日志看的，用户需要的是「是 key 错了还是
/// 地址错了」。所以在重试前把它翻成人话。
pub fn describe_provider_error(error: &AgentError) -> String {
    match error {
        AgentError::Provider {
            status: Some(401 | 403),
            ..
        } => "验证失败：API key 被服务端拒绝（HTTP 401/403）。请检查 key 是否复制完整、是否属于这个接口地址。"
            .to_string(),
        AgentError::Provider {
            status: Some(404), ..
        } => "验证失败：接口地址或模型名不对（HTTP 404）。base url 通常要指到 /v1 这一层，模型名必须是服务端真实存在的。"
            .to_string(),
        AgentError::Provider {
            status: Some(429), ..
        } => "验证失败：被服务端限流（HTTP 429）。稍等一会儿再试，或换一个 key。".to_string(),
        AgentError::Provider {
            status: Some(status),
            ..
        } if (500..600).contains(status) => format!(
            "验证失败：服务端错误（HTTP {status}），不是本地配置的问题，稍后重试即可。"
        ),
        AgentError::Provider {
            status: Some(status),
            ..
        } => format!("验证失败：服务端返回 HTTP {status}。{error}"),
        other => format!(
            "验证失败：连不上接口地址（{other}）。检查地址是否可达、机器是否能出网，或换个网络再试。"
        ),
    }
}

// ─────────────────────────── 六、向导驱动 ───────────────────────────

/// 宿主对向导的全部运行时依赖。
///
/// 真实实现就是 `YunxiTui::run_onboarding`（T2，方案第三节「宿主导入接口」），
/// 见 `main.rs` 里的 `impl WizardDriver for YunxiTui`。
pub trait WizardDriver {
    /// 跑一次向导，返回用户走完的答案或跳过时所在的那一步。
    fn run_onboarding(&mut self, steps: Vec<OnboardingStep>) -> Result<OnboardingOutcome>;

    /// 验证失败、重开向导之前，把原因告诉用户（落在会话区）。
    fn report_retry(&mut self, message: &str) -> Result<()>;
}

/// 向导收集到的答案（宿主侧镜像；字段与 TUI 的 `OnboardingAnswers` 一一对应）。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Answers {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub profile: String,
    pub user_name: String,
    pub user_identity: String,
    pub user_preference: String,
    pub memory_enabled: bool,
    pub knowledge_enabled: bool,
}

impl Answers {
    /// 合并第二段（人格段）的答案；连接段已经填好的 API 三元组保持不变。
    pub fn merge(&mut self, other: Answers) {
        if !other.api_key.trim().is_empty() {
            self.api_key = other.api_key;
        }
        if !other.base_url.trim().is_empty() {
            self.base_url = other.base_url;
        }
        if !other.model.trim().is_empty() {
            self.model = other.model;
        }
        if !other.profile.trim().is_empty() {
            self.profile = other.profile;
        }
        if !other.user_name.trim().is_empty() {
            self.user_name = other.user_name;
        }
        if !other.user_identity.trim().is_empty() {
            self.user_identity = other.user_identity;
        }
        if !other.user_preference.trim().is_empty() {
            self.user_preference = other.user_preference;
        }
        self.memory_enabled = other.memory_enabled;
        self.knowledge_enabled = other.knowledge_enabled;
    }

    /// 转成 TUI 的答案结构（测试与调试用）。
    pub fn probe(&self) -> ProviderProbe {
        ProviderProbe {
            api_key: self.api_key.trim().to_string(),
            base_url: normalize_base_url(&self.base_url),
            model: self.model.trim().to_string(),
        }
    }
}

impl From<yunxi_agent_tui::OnboardingAnswers> for Answers {
    fn from(value: yunxi_agent_tui::OnboardingAnswers) -> Self {
        Self {
            api_key: value.api_key,
            base_url: value.base_url,
            model: value.model,
            profile: value.profile,
            user_name: value.user_name,
            user_identity: value.user_identity,
            user_preference: value.user_preference,
            memory_enabled: value.memory_enabled,
            knowledge_enabled: value.knowledge_enabled,
        }
    }
}

/// 一次首启流程的结果。
#[derive(Clone, Debug, PartialEq)]
pub enum FlowOutcome {
    /// 五步走完并且全部落盘
    Completed(PersistReport),
    /// 用户跳过：什么都没写（包括完成标记）
    Skipped(SkipReport),
}

/// 跳过时给用户看的东西。
#[derive(Clone, Debug, PartialEq)]
pub struct SkipReport {
    /// 在哪一段跳过的（[`PHASE_PROVIDER`] / [`PHASE_PROFILE`]）
    pub at_phase: &'static str,
    /// 停在哪一步
    pub at_step: String,
    /// 具体到「哪些没配、后果是什么、怎么补」的提示
    pub notice: String,
}

/// 跑完整的首启流程：连接段（含验证重试）→ 人格段 → 落盘。
///
/// `verify` 是注入的验证器：生产用 [`verify_provider`]，测试用假实现。
///
/// 验证器**按值**收 [`ProviderProbe`]：返回的 future 要能持有 key/base/model，
/// 否则闭包得为返回值声明生命周期，调用点会写成一堆 `for<'a>` 噪声。
pub async fn run_first_time_flow<W, V, Fut>(
    wizard: &mut W,
    paths: &OnboardingPaths,
    cwd: &Path,
    verify: V,
) -> Result<FlowOutcome>
where
    W: WizardDriver,
    V: Fn(ProviderProbe) -> Fut,
    Fut: std::future::Future<Output = Result<VerifyReport>>,
{
    let mut answers = Answers::default();

    // ── 第一段：连接模型。验证不过就重开这一段，绝不带着坏 key 往下走 ──
    loop {
        match wizard.run_onboarding(api_steps())? {
            OnboardingOutcome::Skipped { at_step } => {
                return Ok(FlowOutcome::Skipped(skip_report(
                    paths,
                    PHASE_PROVIDER,
                    &at_step,
                    &PhaseProgress {
                        credentials_verified: false,
                        persona_collected: false,
                    },
                )));
            }
            OnboardingOutcome::Completed(collected) => {
                let collected = Answers::from(collected);
                let probe = collected.probe();
                match verify(probe).await {
                    Ok(report) => {
                        wizard.report_retry(&format!(
                            "已连通：{} 回复「{}」。接着问几个关于你的问题。",
                            report.model, report.reply
                        ))?;
                        answers.merge(collected);
                        break;
                    }
                    Err(error) => {
                        // 具体的失败原因 + 可重试的出口，都摆到用户面前。
                        wizard.report_retry(&format!(
                            "{error}\n改完 key 或接口地址后重新填一次（在这一步按 Esc 可以跳过整个向导）。"
                        ))?;
                    }
                }
            }
        }
    }

    // ── 第二段：人格 / 关于你 / 记忆库 / 知识库 ──
    match wizard.run_onboarding(profile_steps(persona_options()))? {
        OnboardingOutcome::Skipped { at_step } => Ok(FlowOutcome::Skipped(skip_report(
            paths,
            PHASE_PROFILE,
            &at_step,
            &PhaseProgress {
                credentials_verified: true,
                persona_collected: false,
            },
        ))),
        OnboardingOutcome::Completed(collected) => {
            answers.merge(Answers::from(collected));
            let report = persist(paths, cwd, &answers)?;
            Ok(FlowOutcome::Completed(report))
        }
    }
}

/// 本次流程已经走到哪儿了（决定跳过提示里哪些话是真的）。
struct PhaseProgress {
    /// API key 已经通过验证（但**未必落盘**）
    credentials_verified: bool,
    /// 人格段的答案已经拿到
    persona_collected: bool,
}

/// 跳过提示：指名道姓地说「哪些没配、后果是什么、怎么补」。
///
/// 依据是**磁盘上真实的判据结果**，而不是「用户在第几步按了 Esc」——
/// 两者可能不一致（例如上次已经配过凭证、这次只是没走完人格段），
/// 提示必须说真话。
pub fn skip_report(
    paths: &OnboardingPaths,
    at_phase: &'static str,
    at_step: &OnboardingStepId,
    progress: &PhaseProgress,
) -> SkipReport {
    let status = detect_from_process(paths);
    let mut lines = vec![
        format!(
            "[onboarding] 首次配置向导在「{}」这一步被跳过（{}）；本次没有写入任何配置。",
            step_label(at_step),
            at_phase
        ),
        String::new(),
        "还没配好的东西，以及它们的后果：".to_string(),
    ];

    if progress.credentials_verified {
        lines
            .push("  · API key 这一次验证通过了，但没有落盘：下次启动还要重新填一遍。".to_string());
    }
    if !status.credentials {
        lines.push(format!("  · {}", MissingItem::Credentials.consequence()));
    }
    if !progress.persona_collected {
        lines.push("  · 人格未选择：沿用当前人格设置（默认 yunxi_companion_strong）。".to_string());
        lines.push(
            "  · 称呼 / 身份 / 偏好未写入记忆：云熙不会记住该怎么称呼你，下次还得自我介绍。"
                .to_string(),
        );
        lines.push(
            "  · 记忆库与知识库开关未改动：沿用现有设置（默认记忆关闭、知识库未接入）。"
                .to_string(),
        );
    }
    if !status.persona_settings {
        lines.push(format!(
            "  · {}",
            MissingItem::PersonaSettings.consequence()
        ));
    }
    if !status.marker {
        lines.push(format!("  · {}", MissingItem::Marker.consequence()));
    }

    lines.push(String::new());
    lines.push("怎么补：".to_string());
    lines.push("  · 重新运行 yunxi-linux，会再次进入这个向导；".to_string());
    lines.push(format!(
        "  · 或手工编辑 {}（权限保持 600），至少写上 {ENV_API_KEY}=<你的 key>；",
        paths.environment_file.display()
    ));
    lines.push("  · 会话里输入 /status 可以随时查看当前开关与 Provider 状态。".to_string());

    SkipReport {
        at_phase,
        at_step: step_label(at_step),
        notice: lines.join("\n"),
    }
}

// ─────────────────────────── 七、落盘 ───────────────────────────

/// 落盘结果（全部路径都回给调用方，便于提示与断言）。
#[derive(Clone, Debug, PartialEq)]
pub struct PersistReport {
    /// 向导收集到的完整答案（提示里要用它的开关，**不含凭证输出**）
    pub answers: Answers,
    /// 写过的 environment 文件
    pub environment_file: PathBuf,
    /// 实际写入的变量名（**只有名字，没有值**，避免凭证进日志）
    pub environment_keys: Vec<String>,
    /// 写入后的 `PersonaSettings`
    pub settings: PersonaSettings,
    /// 写进记忆系统的记录（顺序：称呼 / 身份 / 偏好）
    pub memories: Vec<MemoryRecord>,
    /// 知识库开关记录文件
    pub knowledge_choice: PathBuf,
    /// 完成标记
    pub marker: PathBuf,
    /// 标记的写入时间
    pub completed_at_millis: u128,
}

/// 走完向导后的全部落盘动作。
///
/// 顺序有讲究：先写凭证（能用最重要）、再写人格与开关、再写记忆、
/// **最后**写完成标记 —— 标记是判据①，一旦写了就代表「别的东西都好了」，
/// 中途任何一步失败都不该留下它。
pub fn persist(paths: &OnboardingPaths, cwd: &Path, answers: &Answers) -> Result<PersistReport> {
    let mut updates: Vec<(&str, String)> = Vec::new();
    if !answers.api_key.trim().is_empty() {
        updates.push((ENV_API_KEY, answers.api_key.trim().to_string()));
    }
    let base_url = normalize_base_url(&answers.base_url);
    if !base_url.is_empty() {
        updates.push((ENV_BASE_URL, base_url));
    }
    if !answers.model.trim().is_empty() {
        updates.push((ENV_MODEL, answers.model.trim().to_string()));
    }
    if updates.is_empty() {
        bail!("向导结果是空的：既没有 API key 也没有接口地址，拒绝写 environment");
    }
    upsert_environment(&paths.environment_file, &updates)?;
    let environment_keys = updates
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect::<Vec<_>>();

    let settings = apply_persona_settings(paths, &answers.profile, answers.memory_enabled)?;
    let knowledge_choice = write_knowledge_choice(paths, answers.knowledge_enabled)?;

    let store = FilePersonaMemoryStore::for_workspace(cwd);
    let memories = write_user_memories(&store, answers, now_millis())?;

    let completed_at_millis = now_millis();
    write_completion_marker(paths, answers, &settings, completed_at_millis)?;

    Ok(PersistReport {
        answers: answers.clone(),
        environment_file: paths.environment_file.clone(),
        environment_keys,
        settings,
        memories,
        knowledge_choice,
        marker: paths.marker.clone(),
        completed_at_millis,
    })
}

/// 写 `PersonaSettings`：人格 id + 记忆库开关。
///
/// 刻意不用 `PersonaSettings::save()` 的「整文件重写」：那个实现会把文件里
/// 它不认识的键全部丢掉。这里改成**就地更新我自己管的那两行**，
/// 用户手写的注释与未知键都留着。
pub fn apply_persona_settings(
    paths: &OnboardingPaths,
    profile: &str,
    memory_enabled: bool,
) -> Result<PersonaSettings> {
    let profile = if profile.trim().is_empty() {
        DEFAULT_PROFILE_ID.to_string()
    } else {
        profile.trim().to_string()
    };
    if let Err(reason) = yunxi_agent_persona::validate_profile_id(&profile) {
        bail!("人格 profile id 非法（{profile}）：{reason}");
    }

    let existing = match fs::read_to_string(&paths.persona_settings) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("无法读取人格设置: {}", paths.persona_settings.display())
            });
        }
    };
    let mut lines: Vec<String> = if existing.is_empty() {
        Vec::new()
    } else {
        existing.lines().map(str::to_string).collect()
    };
    let updates = [
        ("active_profile", format!("\"{profile}\"")),
        ("memory_enabled", memory_enabled.to_string()),
        ("persona_enabled", "true".to_string()),
    ];
    let mut handled = [false; 3];
    for line in lines.iter_mut() {
        let Some((key, _)) = parse_assignment(line) else {
            continue;
        };
        if let Some(index) = updates.iter().position(|(name, _)| *name == key) {
            *line = format!("{} = {}", updates[index].0, updates[index].1);
            handled[index] = true;
        }
    }
    for (index, (name, value)) in updates.iter().enumerate() {
        if !handled[index] {
            lines.push(format!("{name} = {value}"));
        }
    }
    let mut content = lines.join("\n");
    content.push('\n');
    // 人格设置不是凭证，但也是隐私（关系到用户选了哪种陪伴人格），照样 600。
    write_private_file(&paths.persona_settings, &content)?;

    Ok(PersonaSettings::parse_lossy(&content))
}

/// 记录知识库开关。
///
/// `PersonaSettings` 没有知识库字段（本组不允许改 `crates/`），所以宿主把它
/// 记在同一个目录下的 `onboarding.toml` 里 —— 不新增概念，也不冒充运行时开关。
pub fn write_knowledge_choice(paths: &OnboardingPaths, knowledge_enabled: bool) -> Result<PathBuf> {
    let content = format!(
        "# yunxi-linux 首次配置向导留下的记录（不是运行时开关）\n\
         # 知识库目前通过 `yunxi-linux knowledge-man <topic>` / `knowledge-search <query>` 使用。\n\
         knowledge_enabled = {knowledge_enabled}\n\
         recorded_at_millis = {}\n",
        now_millis()
    );
    write_private_file(&paths.persona_onboarding, &content)?;
    Ok(paths.persona_onboarding.clone())
}

/// 把用户卡片写进**既有记忆系统**（不新增概念）。
///
/// 三条记录都落在 `GlobalUser` 作用域（用户本人，不绑定工作区），
/// 状态直接是 `Active` —— 用户是**亲自填的**，再进 `Pending` 等确认是多余的打扰。
pub fn write_user_memories(
    store: &FilePersonaMemoryStore,
    answers: &Answers,
    now: u128,
) -> Result<Vec<MemoryRecord>> {
    let mut written = Vec::new();
    for (index, (kind, content)) in user_memory_contents(answers).into_iter().enumerate() {
        let mut record = MemoryRecord::new(
            format!("mem-onboarding-{now}-{index}"),
            MemoryScope::GlobalUser,
            kind,
            content,
            now,
        )
        // `MemoryRecord::new()` 默认是 Pending（见 persona::memory），必须显式改 Active。
        .with_status(MemoryStatus::Active)
        .with_scores(0.9, 0.8);
        record.source.extractor = Some("onboarding-wizard".to_string());
        store
            .append_or_merge(&record)
            .with_context(|| format!("写入记忆失败: {}", record.content))?;
        written.push(record);
    }
    Ok(written)
}

/// 用户卡片 → 记忆记录的内容与种类（空答案不产生记录）。
fn user_memory_contents(answers: &Answers) -> Vec<(MemoryKind, String)> {
    let mut records = Vec::new();
    let name = answers.user_name.trim();
    if !name.is_empty() {
        records.push((
            MemoryKind::PersonalFact,
            format!("用户的称呼是「{name}」，希望被这样称呼。"),
        ));
    }
    let identity = answers.user_identity.trim();
    if !identity.is_empty() {
        records.push((
            MemoryKind::PersonalFact,
            format!("用户自述身份：{identity}"),
        ));
    }
    let preference = answers.user_preference.trim();
    if !preference.is_empty() {
        records.push((MemoryKind::Preference, format!("用户偏好：{preference}")));
    }
    records
}

/// 读回用户级记忆（宿主提示与测试用）。
pub fn load_user_memories(store: &FilePersonaMemoryStore) -> Vec<MemoryRecord> {
    // 作用域必须用 `All`：写入时用的是 `MemoryScope::GlobalUser`，它并不落在
    // `PersonaMemoryScope::Global` 那个桶里，用 `Global` 查会永远读到 0 条。
    // 「是不是用户记忆」由下面的 scope 过滤负责，不由查询作用域负责。
    store
        .list(PersonaMemoryScope::All)
        .records
        .into_iter()
        .filter(|record| matches!(record.scope, MemoryScope::GlobalUser))
        .collect()
}

/// 写完成标记（判据①）。
pub fn write_completion_marker(
    paths: &OnboardingPaths,
    answers: &Answers,
    settings: &PersonaSettings,
    now: u128,
) -> Result<()> {
    let content = format!(
        "# yunxi-linux 首次配置完成标记（判据①）。删掉它会重新进入配置向导。\n\
         completed\n\
         completed_at_millis = {now}\n\
         active_profile = \"{}\"\n\
         memory_enabled = {}\n\
         knowledge_enabled = {}\n",
        settings.active_profile, settings.memory_enabled, answers.knowledge_enabled
    );
    write_private_file(&paths.marker, &content)
}

/// 给会话区的一句完成提示（**不含任何凭证**）。
pub fn completion_notice(report: &PersistReport) -> String {
    let mut lines = vec![
        "[onboarding] 首次配置完成，已写入：".to_string(),
        format!("  · 凭证与模型   {}", report.environment_file.display()),
        format!(
            "  · 人格         {}（记忆库 {}）",
            report.settings.active_profile,
            if report.settings.memory_enabled {
                "开"
            } else {
                "关"
            }
        ),
    ];
    if !report.memories.is_empty() {
        lines.push(format!(
            "  · 关于你       {} 条记录已进长期记忆（称呼 / 身份 / 偏好）",
            report.memories.len()
        ));
    }
    if !report.answers.knowledge_enabled {
        lines.push(
            "  · 知识库       本次未接入；之后用 `yunxi-linux knowledge-man <主题>` 采集，`knowledge-search <关键词>` 检索"
                .to_string(),
        );
    }
    lines.push(format!("  · 完成标记     {}", report.marker.display()));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write as IoWrite};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use yunxi_agent_tui::OnboardingAnswers;

    /// 改进程环境变量（`YUNXI_HOME`）的测试必须串行，且用完还原。
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(label: &str) -> Self {
            let unique = format!(
                "yunxi-onboarding-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            );
            let root = std::env::temp_dir().join(unique);
            fs::create_dir_all(&root).expect("create temp root");
            Self { root }
        }

        fn paths(&self) -> OnboardingPaths {
            // 被测代码会往 config 目录里写 `environment`，父目录必须先存在，
            // 否则测试会在 `fs::write` 上拿到 "No such file or directory"。
            fs::create_dir_all(self.root.join("config")).expect("create config dir");
            OnboardingPaths::with_roots(
                self.root.join("state"),
                self.root.join("config"),
                self.root.join("home"),
            )
        }

        fn workspace(&self) -> PathBuf {
            let workspace = self.root.join("workspace");
            fs::create_dir_all(&workspace).expect("create workspace");
            workspace
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// 在隔离的 `YUNXI_HOME` 下跑一段同步逻辑（记忆存储的全局根目录跟着它走）。
    fn with_isolated_home<T>(home: &Path, body: impl FnOnce() -> T) -> T {
        let _guard = HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os("YUNXI_HOME");
        // SAFETY: 这个锁保证同一进程里只有这一个测试在改 YUNXI_HOME，且结束前还原。
        unsafe {
            std::env::set_var("YUNXI_HOME", home);
        }
        let output = body();
        match previous {
            Some(value) => unsafe { std::env::set_var("YUNXI_HOME", value) },
            None => unsafe { std::env::remove_var("YUNXI_HOME") },
        }
        output
    }

    // ─────────── 判据 ───────────

    #[test]
    fn configured_requires_all_three_criteria() {
        let tree = TempTree::new("criteria");
        let paths = tree.paths();

        // 三缺一都不算已配置 —— 这里把组合穷举一遍。
        let combinations = [
            (false, false, false, false),
            (true, false, false, false),
            (false, true, false, false),
            (false, false, true, false),
            (true, true, false, false),
            (true, false, true, false),
            (false, true, true, false),
            (true, true, true, true),
        ];
        for (marker, credentials, persona_settings, expected) in combinations {
            if marker {
                fs::create_dir_all(paths.marker.parent().expect("marker parent"))
                    .expect("create state dir");
                fs::write(&paths.marker, "completed\n").expect("write marker");
            } else {
                let _ = fs::remove_file(&paths.marker);
            }
            fs::create_dir_all(paths.environment_file.parent().expect("config dir"))
                .expect("create config dir");
            if credentials {
                fs::write(&paths.environment_file, format!("{ENV_API_KEY}=sk-test\n"))
                    .expect("write env file");
            } else {
                let _ = fs::remove_file(&paths.environment_file);
            }
            if persona_settings {
                fs::create_dir_all(paths.persona_settings.parent().expect("persona dir"))
                    .expect("create persona dir");
                fs::write(&paths.persona_settings, "memory_enabled = true\n")
                    .expect("write settings");
            } else {
                let _ = fs::remove_file(&paths.persona_settings);
            }

            let status = detect(&paths, None);
            assert_eq!(
                status,
                ConfigStatus {
                    marker,
                    credentials,
                    persona_settings
                },
                "组合 (marker={marker}, credentials={credentials}, persona={persona_settings})"
            );
            assert_eq!(
                status.is_configured(),
                expected,
                "组合 (marker={marker}, credentials={credentials}, persona={persona_settings})"
            );
            assert_eq!(
                status.missing().len(),
                3 - [marker, credentials, persona_settings]
                    .iter()
                    .filter(|value| **value)
                    .count()
            );
        }
    }

    #[test]
    fn credentials_come_from_process_env_or_the_environment_file() {
        let tree = TempTree::new("credentials");
        let paths = tree.paths();

        // 只有进程环境：算有凭证（文件不存在也不影响）。
        assert!(credentials_available(
            Some("sk-env"),
            &paths.environment_file
        ));
        assert!(!credentials_available(Some("   "), &paths.environment_file));
        assert!(!credentials_available(None, &paths.environment_file));

        // 只有文件：`export` 前缀、单双引号、行尾注释都要能解析。
        fs::write(
            &paths.environment_file,
            "# 注释里的 YUNXI_PROVIDER_API_KEY 不算\n\
             OTHER=1\n\
             export YUNXI_PROVIDER_API_KEY='sk-file'\n",
        )
        .expect("write env file");
        assert_eq!(
            read_environment_api_key(&paths.environment_file).as_deref(),
            Some("sk-file")
        );
        assert!(credentials_available(None, &paths.environment_file));

        // 后出现的定义生效（与 shell source 的语义一致）。
        fs::write(
            &paths.environment_file,
            format!("{ENV_API_KEY}=sk-old\n{ENV_API_KEY}=sk-new # 注释\n"),
        )
        .expect("write env file");
        assert_eq!(
            read_environment_api_key(&paths.environment_file).as_deref(),
            Some("sk-new")
        );

        // 空值不算凭证。
        fs::write(&paths.environment_file, format!("{ENV_API_KEY}=\n")).expect("write env file");
        assert!(!credentials_available(None, &paths.environment_file));
    }

    // ─────────── environment 落盘 ───────────

    #[test]
    fn environment_update_keeps_600_and_preserves_existing_content() {
        let tree = TempTree::new("environment");
        let paths = tree.paths();
        fs::create_dir_all(paths.environment_file.parent().expect("config dir"))
            .expect("create config dir");
        let original = "# YunXi Native · Provider 凭证\n\
                        # 注释不能被弄丢\n\
                        \n\
                        YUNXI_PROVIDER_BASE_URL=https://old.example/v1\n\
                        YUNXI_PROVIDER_API_KEY=sk-old\n\
                        OPENAI_API_KEY=sk-legacy\n\
                        YUNXI_PROVIDER_STREAM=0\n";
        fs::write(&paths.environment_file, original).expect("write original");

        upsert_environment(
            &paths.environment_file,
            &[
                (ENV_API_KEY, "sk-new".to_string()),
                (ENV_BASE_URL, "https://new.example/v1".to_string()),
                (ENV_MODEL, "mock-model".to_string()),
            ],
        )
        .expect("upsert environment");

        let content = fs::read_to_string(&paths.environment_file).expect("read back");
        // 原有内容一个都不能少
        for kept in [
            "# YunXi Native · Provider 凭证",
            "# 注释不能被弄丢",
            "OPENAI_API_KEY=sk-legacy",
            "YUNXI_PROVIDER_STREAM=0",
        ] {
            assert!(
                content.contains(kept),
                "丢了原有内容: {kept}\n---\n{content}"
            );
        }
        // 同名变量就地更新，不追加重复定义
        assert!(content.contains("YUNXI_PROVIDER_API_KEY=sk-new"));
        assert!(content.contains("YUNXI_PROVIDER_BASE_URL=https://new.example/v1"));
        assert!(!content.contains("sk-old"));
        assert_eq!(content.matches(ENV_API_KEY).count(), 1);
        // 新变量追加在末尾
        assert!(content.contains("YUNXI_AGENT_MODEL=mock-model"));
        // 权限仍是 600
        assert_eq!(file_mode(&paths.environment_file), Some(0o600));

        // 再来一次（模拟重复走向导）不应该产生重复行、也不该掉权限
        upsert_environment(
            &paths.environment_file,
            &[(ENV_API_KEY, "sk-third".to_string())],
        )
        .expect("upsert again");
        let content = fs::read_to_string(&paths.environment_file).expect("read back");
        assert_eq!(content.matches(ENV_API_KEY).count(), 1);
        assert!(content.contains("YUNXI_PROVIDER_API_KEY=sk-third"));
        assert_eq!(file_mode(&paths.environment_file), Some(0o600));
    }

    #[test]
    fn environment_creation_is_600_and_writes_through_the_symlink() {
        let tree = TempTree::new("environment-symlink");
        let paths = tree.paths();
        let config_dir = paths.environment_file.parent().expect("config dir");
        fs::create_dir_all(config_dir).expect("create config dir");
        let alias = config_dir.join(ENV_FILE_ALIAS);
        let target = config_dir.join(ENV_FILE_NAME);

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, &alias).expect("create symlink");
            // 通过软链写入：真实文件被更新，软链本身还在
            upsert_environment(&alias, &[(ENV_API_KEY, "sk-via-alias".to_string())])
                .expect("upsert via alias");
            assert!(
                fs::symlink_metadata(&alias)
                    .expect("alias metadata")
                    .file_type()
                    .is_symlink(),
                "软链被替换掉了"
            );
            assert_eq!(file_mode(&target), Some(0o600));
            let content = fs::read_to_string(&target).expect("read target");
            assert!(content.contains("YUNXI_PROVIDER_API_KEY=sk-via-alias"));
        }

        // 从零创建（软链不存在的情况）也必须 600
        let fresh = config_dir.join("fresh");
        upsert_environment(&fresh, &[(ENV_API_KEY, "sk-fresh".to_string())]).expect("upsert fresh");
        assert_eq!(file_mode(&fresh), Some(0o600));
    }

    // ─────────── 人格设置 / 知识库记录 / 标记 ───────────

    #[test]
    fn persona_settings_update_keeps_unknown_keys_and_sets_switches() {
        let tree = TempTree::new("settings");
        let paths = tree.paths();
        fs::create_dir_all(paths.persona_settings.parent().expect("persona dir"))
            .expect("create persona dir");
        fs::write(
            &paths.persona_settings,
            "# 用户手写的注释\npersona_enabled = false\nmemory_enabled = false\nfuture_key = \"keep me\"\n",
        )
        .expect("write settings");

        let settings =
            apply_persona_settings(&paths, DEFAULT_PROFILE_ID, true).expect("apply settings");

        assert_eq!(settings.active_profile, DEFAULT_PROFILE_ID);
        assert!(settings.memory_enabled);
        assert!(settings.persona_enabled);
        let content = fs::read_to_string(&paths.persona_settings).expect("read settings");
        assert!(
            content.contains("future_key = \"keep me\""),
            "未知键丢了:\n{content}"
        );
        assert!(content.contains("# 用户手写的注释"));
        assert!(content.contains("memory_enabled = true"));

        // 非法 profile id 必须挡住，而不是写进去
        assert!(apply_persona_settings(&paths, "../etc/passwd", true).is_err());
    }

    #[test]
    fn skip_never_writes_the_marker_and_explains_the_consequences() {
        let tree = TempTree::new("skip");
        let paths = tree.paths();

        let report = skip_report(
            &paths,
            PHASE_PROVIDER,
            &OnboardingStepId::ApiKey,
            &PhaseProgress {
                credentials_verified: false,
                persona_collected: false,
            },
        );

        // 没有写任何东西 —— 包括标记
        assert!(!paths.marker.exists());
        assert!(!paths.environment_file.exists());
        assert!(!paths.persona_settings.exists());
        // 提示必须点名「哪些没配、后果、怎么补」
        assert!(report.notice.contains("API key"), "{}", report.notice);
        assert!(
            report.notice.contains("只会走本地静态 Runtime"),
            "{}",
            report.notice
        );
        assert!(
            report.notice.contains("重新运行 yunxi-linux"),
            "{}",
            report.notice
        );
        assert!(report.notice.contains("调用模型"), "{}", report.notice);
        assert_eq!(report.at_step, "API key");
        assert_eq!(report.at_phase, PHASE_PROVIDER);
    }

    #[test]
    fn skip_after_a_verified_key_says_so_instead_of_claiming_no_credentials() {
        let tree = TempTree::new("skip-verified");
        let paths = tree.paths();

        let report = skip_report(
            &paths,
            PHASE_PROFILE,
            &OnboardingStepId::Profile,
            &PhaseProgress {
                credentials_verified: true,
                persona_collected: false,
            },
        );

        assert!(!paths.marker.exists());
        assert!(
            report.notice.contains("验证通过了，但没有落盘"),
            "{}",
            report.notice
        );
        assert!(report.notice.contains("人格未选择"), "{}", report.notice);
    }

    // ─────────── 记忆写入与读回 ───────────

    #[test]
    fn user_card_is_written_to_the_existing_memory_system_as_active() {
        let tree = TempTree::new("memory");
        let workspace = tree.workspace();
        let home = tree.root.join("home");

        with_isolated_home(&home, || {
            let store = FilePersonaMemoryStore::for_workspace(&workspace);
            let answers = Answers {
                user_name: "小明".to_string(),
                user_identity: "后端工程师".to_string(),
                user_preference: "回答尽量短".to_string(),
                ..Answers::default()
            };

            let written =
                write_user_memories(&store, &answers, now_millis()).expect("write memories");
            assert_eq!(written.len(), 3);
            for record in &written {
                assert_eq!(record.scope, MemoryScope::GlobalUser);
                assert_eq!(
                    record.status,
                    MemoryStatus::Active,
                    "用户亲自填的记录必须直接 Active，不能等确认"
                );
                assert_ne!(record.status, MemoryStatus::Pending);
            }
            assert_eq!(written[0].kind, MemoryKind::PersonalFact);
            assert_eq!(written[2].kind, MemoryKind::Preference);
            assert_eq!(written[1].layer, yunxi_agent_persona::MemoryLayer::Profile);

            // 必须能用 list() 读回来
            let loaded = load_user_memories(&store);
            assert_eq!(loaded.len(), 3, "读回失败: {loaded:#?}");
            let contents = loaded
                .iter()
                .map(|record| record.content.clone())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(contents.contains("小明"), "{contents}");
            assert!(contents.contains("后端工程师"), "{contents}");
            assert!(contents.contains("回答尽量短"), "{contents}");
            // 全局记忆落在 YUNXI_HOME 下，工作区 .yunxi 里不该有这些记录
            assert!(home.join("memory").is_dir());
        });
    }

    #[test]
    fn empty_user_card_answers_produce_no_records() {
        let answers = Answers::default();
        assert!(user_memory_contents(&answers).is_empty());
    }

    // ─────────── 完整流程（假向导 + 假验证器） ───────────

    struct FakeWizard {
        /// 每次 `run_onboarding` 依次弹出的结果
        outcomes: Vec<OnboardingOutcome>,
        cursor: usize,
        /// 记录宿主报告过的重试原因
        retries: Vec<String>,
        /// 每次调用收到的步骤 id
        seen: Vec<Vec<OnboardingStepId>>,
    }

    impl FakeWizard {
        fn new(outcomes: Vec<OnboardingOutcome>) -> Self {
            Self {
                outcomes,
                cursor: 0,
                retries: Vec::new(),
                seen: Vec::new(),
            }
        }
    }

    impl WizardDriver for FakeWizard {
        fn run_onboarding(&mut self, steps: Vec<OnboardingStep>) -> Result<OnboardingOutcome> {
            self.seen
                .push(steps.iter().map(|step| step.id.clone()).collect());
            let outcome = self
                .outcomes
                .get(self.cursor)
                .cloned()
                .expect("FakeWizard 的结果用完了");
            self.cursor += 1;
            Ok(outcome)
        }

        fn report_retry(&mut self, message: &str) -> Result<()> {
            self.retries.push(message.to_string());
            Ok(())
        }
    }

    fn api_answers() -> OnboardingAnswers {
        OnboardingAnswers {
            api_key: "sk-test".to_string(),
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "mock-model".to_string(),
            ..OnboardingAnswers::default()
        }
    }

    fn profile_answers() -> OnboardingAnswers {
        OnboardingAnswers {
            profile: DEFAULT_PROFILE_ID.to_string(),
            user_name: "小明".to_string(),
            user_identity: "后端工程师".to_string(),
            user_preference: "回答尽量短".to_string(),
            memory_enabled: true,
            knowledge_enabled: false,
            ..OnboardingAnswers::default()
        }
    }

    fn ok_verify(_probe: ProviderProbe) -> std::future::Ready<Result<VerifyReport>> {
        std::future::ready(Ok(VerifyReport {
            model: "mock-model".to_string(),
            reply: "你好".to_string(),
        }))
    }

    // 这个测试自建 runtime（下面的 `Builder::new_current_thread`），所以不能再挂
    // `#[tokio::test]` —— 那会让 `block_on` 在已有 runtime 里执行并 panic。
    #[test]
    fn flow_retries_until_the_provider_verifies_then_persists_everything() {
        let tree = TempTree::new("flow");
        let paths = tree.paths();
        let workspace = tree.workspace();
        let home = tree.root.join("home");

        let attempts = AtomicUsize::new(0);
        let wizard_outcomes = vec![
            OnboardingOutcome::Completed(api_answers()),
            OnboardingOutcome::Completed(api_answers()),
            OnboardingOutcome::Completed(profile_answers()),
        ];
        let mut wizard = FakeWizard::new(wizard_outcomes);
        let outcome = with_isolated_home(&home, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let outcome = runtime.block_on(run_first_time_flow(
                &mut wizard,
                &paths,
                &workspace,
                |probe: ProviderProbe| {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if attempt == 0 {
                            anyhow::bail!("验证失败：API key 被服务端拒绝（HTTP 401/403）。");
                        }
                        Ok(VerifyReport {
                            model: probe.model,
                            reply: "你好".to_string(),
                        })
                    }
                },
            ));
            // 记忆必须在**隔离 HOME 之内**读：store 的根路径由 HOME /
            // XDG_DATA_HOME 决定，出了闭包再读就落到用户真实记忆目录上，
            // 于是永远读到 0 条 —— 这个测试原来就是这么挂的。
            let loaded = load_user_memories(&FilePersonaMemoryStore::for_workspace(&workspace));
            (outcome, loaded)
        });
        let (outcome, loaded) = outcome;
        let outcome = outcome.expect("flow runs");

        let FlowOutcome::Completed(report) = outcome else {
            panic!("应当走完流程: {outcome:?}");
        };
        // 验证失败重开的是「连接段」三步，然后才是「人格段」四步
        assert_eq!(wizard.seen[0].len(), 3);
        assert_eq!(wizard.seen[1].len(), 3);
        assert_eq!(wizard.seen[2].len(), 4);
        assert_eq!(wizard.retries.len(), 2, "{:#?}", wizard.retries);
        assert!(
            wizard.retries[0].contains("被服务端拒绝"),
            "{:#?}",
            wizard.retries
        );

        // 落盘结果
        assert!(paths.marker.is_file(), "完成标记没写");
        assert_eq!(file_mode(&paths.environment_file), Some(0o600));
        let environment = fs::read_to_string(&paths.environment_file).expect("read environment");
        assert!(environment.contains("YUNXI_PROVIDER_API_KEY=sk-test"));
        assert!(environment.contains("YUNXI_AGENT_MODEL=mock-model"));
        assert_eq!(report.environment_keys.len(), 3);
        assert!(!report.settings.memory_enabled == false);
        assert!(paths.persona_settings.is_file());
        assert!(paths.persona_onboarding.is_file());
        assert_eq!(report.memories.len(), 3);

        // 记忆真的能读回来（已在上面的隔离 HOME 内读出）
        assert_eq!(loaded.len(), 3);
        assert!(
            loaded
                .iter()
                .all(|record| record.status == MemoryStatus::Active)
        );

        // 判据三连：现在应当判定为「已配置」
        let status = detect(&paths, None);
        assert!(status.is_configured(), "{status:?}");

        // 完成提示里绝不能出现凭证
        let notice = completion_notice(&report);
        assert!(!notice.contains("sk-test"), "{notice}");
        assert!(notice.contains("首次配置完成"), "{notice}");
    }

    // 这个测试自建 runtime（下面的 `Builder::new_current_thread`），所以不能再挂
    // `#[tokio::test]` —— 那会让 `block_on` 在已有 runtime 里执行并 panic。
    #[test]
    fn flow_skip_in_the_first_phase_writes_nothing() {
        let tree = TempTree::new("flow-skip");
        let paths = tree.paths();
        let workspace = tree.workspace();
        let home = tree.root.join("home");

        let mut wizard = FakeWizard::new(vec![OnboardingOutcome::Skipped {
            at_step: OnboardingStepId::ApiBaseUrl,
        }]);
        let outcome = with_isolated_home(&home, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            runtime.block_on(run_first_time_flow(
                &mut wizard,
                &paths,
                &workspace,
                ok_verify,
            ))
        })
        .expect("flow runs");

        let FlowOutcome::Skipped(report) = outcome else {
            panic!("应当跳过: {outcome:?}");
        };
        assert_eq!(report.at_step, "接口地址");
        assert!(!paths.marker.exists(), "跳过时绝不能写完成标记");
        assert!(!paths.environment_file.exists());
        assert!(!paths.persona_settings.exists());
        assert!(!home.join("memory").exists(), "跳过时不该写记忆");
        // 跳过时也不该有人调验证器
        assert_eq!(wizard.seen.len(), 1);
    }

    // 这个测试自建 runtime（下面的 `Builder::new_current_thread`），所以不能再挂
    // `#[tokio::test]` —— 那会让 `block_on` 在已有 runtime 里执行并 panic。
    #[test]
    fn flow_skip_in_the_second_phase_keeps_the_marker_unwritten() {
        let tree = TempTree::new("flow-skip-2");
        let paths = tree.paths();
        let workspace = tree.workspace();
        let home = tree.root.join("home");

        let mut wizard = FakeWizard::new(vec![
            OnboardingOutcome::Completed(api_answers()),
            OnboardingOutcome::Skipped {
                at_step: OnboardingStepId::UserCard,
            },
        ]);
        let outcome = with_isolated_home(&home, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            runtime.block_on(run_first_time_flow(
                &mut wizard,
                &paths,
                &workspace,
                ok_verify,
            ))
        })
        .expect("flow runs");

        let FlowOutcome::Skipped(report) = outcome else {
            panic!("应当跳过: {outcome:?}");
        };
        assert_eq!(report.at_step, "关于你（称呼 / 身份 / 偏好）");
        assert!(!paths.marker.exists());
        assert!(
            !paths.environment_file.exists(),
            "跳过就不该落盘（下次重来）"
        );
    }

    // ─────────── 验证器本身（本地 mock server，绝不打真 API） ───────────

    /// 起一个最小 HTTP server：只处理一次 POST /v1/chat/completions。
    fn spawn_mock_provider(
        status_line: &'static str,
        body: &'static str,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock provider");
        let address = listener.local_addr().expect("mock address");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut request_line = String::new();
            let _ = reader.read_line(&mut request_line);
            // 把请求行与**所有请求头**都累积下来。原来只从句柄里抠出
            // content-length、把头全丢掉，于是调用方那条「必须带 bearer 凭证」
            // 的断言在结构上就不可能通过 —— 不是 provider 没发头，是 mock 没记。
            let mut captured = request_line;
            let mut content_length = 0usize;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                    break;
                }
                captured.push_str(&header);
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut payload = vec![0u8; content_length];
            let _ = reader.read_exact(&mut payload);
            let response = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            let _ = stream.flush();
            captured.push_str(&String::from_utf8(payload).expect("request body is utf8"));
            captured
        });
        (format!("http://{address}/v1"), handle)
    }

    #[tokio::test]
    async fn provider_verification_succeeds_against_a_local_mock() {
        let (base_url, server) = spawn_mock_provider(
            "200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":"你好呀"}}]}"#,
        );
        let probe = ProviderProbe {
            api_key: "sk-mock".to_string(),
            base_url: base_url.clone(),
            model: "mock-model".to_string(),
        };

        let report = verify_provider(&probe, Path::new("."))
            .await
            .expect("mock provider verifies");
        assert_eq!(report.model, "mock-model");
        assert_eq!(report.reply, "你好呀");

        let request = server.join().expect("mock server joins");
        // 请求里必须带上模型名、那句最短的问候、以及 bearer 凭证
        assert!(request.contains("\"model\":\"mock-model\""), "{request}");
        assert!(request.contains("你好"), "{request}");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-mock"),
            "{request}"
        );
        // 验证请求不该带工具 schema（最小请求）
        assert!(!request.contains("\"tools\""), "{request}");
    }

    #[tokio::test]
    async fn provider_verification_reports_a_rejected_key_in_plain_chinese() {
        let (base_url, server) = spawn_mock_provider(
            "401 Unauthorized",
            r#"{"error":{"type":"authentication_error","code":"invalid_api_key"}}"#,
        );
        let probe = ProviderProbe {
            api_key: "sk-bad".to_string(),
            base_url,
            model: "mock-model".to_string(),
        };

        let error = verify_provider(&probe, Path::new("."))
            .await
            .expect_err("401 必须失败");
        let message = error.to_string();
        assert!(message.contains("API key 被服务端拒绝"), "{message}");
        assert!(message.contains("401"), "{message}");
        let _ = server.join();
    }

    #[test]
    fn base_url_normalization_matches_the_openai_convention() {
        assert_eq!(
            normalize_base_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_base_url("  https://api.deepseek.com/v1/  "),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8080/v1"),
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8080"),
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(normalize_base_url("   "), "");
    }

    #[test]
    fn persona_options_always_offer_the_builtin_profile() {
        let options = persona_options();
        assert!(!options.is_empty());
        assert_eq!(options[0].id, DEFAULT_PROFILE_ID);
        assert!(!options[0].detail.is_empty());
    }
}
