use crate::approval_layout::{APPROVAL_HINT_PRIMARY, APPROVAL_HINT_SECONDARY};
use crate::bottom_pane::{ApprovalRequestView, BottomPane, BottomPaneMode, UserInputRequestView};
use crate::chat::Transcript;
use crate::input_map::FocusTarget;
use crate::presentation::{TuiEvent, TuiPresentation};
use crate::text_layout::{ClipPriority, PrioritySegment, TextLayout};
use crate::timeline_store::TimelineStore;
use crate::transcript_layout::WrappedTranscript;
use crate::viewport::TranscriptViewport;
use crate::welcome::WelcomeScene;
use std::time::Instant;
use yunxi_agent_core::{AgentEvent, ControlSnapshot, TokenUsage};

const SPINNER_FRAMES: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// 欢迎界面的淡入帧数，与 `WelcomeScene::tick()` 内部那个 24 帧淡入保持一致。
const WELCOME_FADE_IN_FRAMES: usize = 24;

/// 星空一次完整闪烁的帧数。
///
/// `star_at()` 里每颗星的相位周期是 `8 * speed`，`speed ∈ 2..=4`，也就是
/// 16 / 24 / 32 帧；三者的最小公倍数是 96 —— 走满 96 帧后整片星域精确回到同一图案。
const STARFIELD_CYCLE_FRAMES: usize = 96;

/// 欢迎动画的总帧数预算：淡入 24 帧 + 3 轮星移（3 × 96）≈ 10.4s（33ms/帧）。
///
/// 动画被刻意定义为**有限时长**的开场动效（对应 issue #38 的方案 C 思路）：
/// 播完就停在最后一帧，空闲等待输入时不再每 33ms 重绘一次全屏，
/// 这样用户盯着欢迎界面思考时 CPU 和终端写入都归零。
/// 想让它像壁纸一样一直闪，把这个预算调大即可 —— 代价就是空闲态持续 30fps 重绘。
const WELCOME_ANIMATION_FRAMES: usize = WELCOME_FADE_IN_FRAMES + 3 * STARFIELD_CYCLE_FRAMES;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YunxiTuiBanner {
    pub cwd: String,
    pub backend: String,
    pub provider_live: bool,
    pub provider_source: String,
    pub model: String,
    pub provider: String,
}

#[derive(Clone, Debug)]
pub(crate) struct YunxiTuiApp {
    version: String,
    banner: Option<YunxiTuiBanner>,
    welcome_enabled: bool,
    welcome_checklist: Vec<String>,
    welcome_scene: Option<WelcomeScene>,
    /// 欢迎动画已经推进过的帧数，用于给空闲态的开场动效封顶（见 `WELCOME_ANIMATION_FRAMES`）。
    welcome_animation_frames: usize,
    presentation: TuiPresentation,
    timeline: TimelineStore,
    transcript: Transcript,
    viewport: TranscriptViewport,
    bottom_pane: BottomPane,
    control_snapshot: Option<ControlSnapshot>,
    realtime_voice_enabled: bool,
    details: Option<String>,
    details_scroll: u16,
    focus: FocusTarget,
    previous_focus: FocusTarget,
    turn_started_at: Option<Instant>,
    spinner_frame: usize,
    last_usage: Option<TokenUsage>,
    active_context_tokens: Option<i64>,
    context_token_limit_reached: bool,
    context_compacted: bool,
    context_dropped_messages: usize,
    context_pressure: bool,
}

impl Default for YunxiTuiApp {
    fn default() -> Self {
        Self {
            version: format!("v{}", env!("CARGO_PKG_VERSION")),
            banner: None,
            welcome_enabled: true,
            welcome_checklist: Vec::new(),
            welcome_scene: Some(WelcomeScene::new()),
            welcome_animation_frames: 0,
            presentation: TuiPresentation::default(),
            timeline: TimelineStore::default(),
            transcript: Transcript::default(),
            viewport: TranscriptViewport::default(),
            bottom_pane: BottomPane::default(),
            control_snapshot: None,
            realtime_voice_enabled: false,
            details: None,
            details_scroll: 0,
            focus: FocusTarget::Composer,
            previous_focus: FocusTarget::Composer,
            turn_started_at: None,
            spinner_frame: 0,
            last_usage: None,
            active_context_tokens: None,
            context_token_limit_reached: false,
            context_compacted: false,
            context_dropped_messages: 0,
            context_pressure: false,
        }
    }
}

impl YunxiTuiApp {
    #[cfg(test)]
    pub(crate) fn set_version_for_snapshot(&mut self, version: &str) {
        self.version = version.to_string();
    }

    pub(crate) fn set_banner(&mut self, banner: YunxiTuiBanner) {
        self.presentation.set_offline_label(!banner.provider_live);
        self.banner = Some(banner);
    }

    pub(crate) fn set_welcome_enabled(&mut self, enabled: bool) {
        self.welcome_enabled = enabled;
    }

    pub(crate) fn set_welcome_checklist(&mut self, checklist: Vec<String>) {
        self.welcome_checklist = checklist;
    }

    pub(crate) fn welcome_enabled(&self) -> bool {
        self.welcome_enabled
    }

    pub(crate) fn welcome_checklist(&self) -> &[String] {
        &self.welcome_checklist
    }

    pub(crate) fn set_realtime_voice_enabled(&mut self, enabled: bool) {
        self.realtime_voice_enabled = enabled;
    }

    pub(crate) fn begin_turn(&mut self) {
        self.turn_started_at = Some(Instant::now());
        self.spinner_frame = 0;
    }

    pub(crate) fn end_turn(&mut self) {
        self.turn_started_at = None;
    }

    pub(crate) fn advance_spinner(&mut self) -> bool {
        if self.turn_started_at.is_none() {
            return false;
        }
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
        true
    }

    /// 欢迎动画此刻是否还需要供帧。
    ///
    /// issue #38 的根因是"欢迎动画没有 tick 供给"：`tick()` 只在回合里被调用，
    /// 而欢迎界面属于空闲态。这个判据就是给空闲态用的闸门 —— 只有返回 `true`
    /// 时 `host.rs::read_prompt()` 才会带超时轮询并重绘，返回 `false` 就退回纯阻塞读。
    ///
    /// 四个条件缺一不可：
    /// - `welcome_enabled`：`YUNXI_TUI_BANNER=0` 时压根没有欢迎卡；
    /// - `!has_user_round()`：首轮真实对话之后 `render.rs` 用 transcript 替掉欢迎卡；
    /// - `welcome_checklist` 为空：首次启动渲染的是**静态**检查卡
    ///   （`render.rs::render_transcript` 只在 checklist 为空时才用动画卡），
    ///   此时推进动画没有任何像素会变，属于纯浪费；
    /// - 帧数预算没花完：动画是有限时长的开场动效，播完就停，不再空转。
    pub(crate) fn welcome_animation_pending(&self) -> bool {
        self.welcome_enabled
            && self.welcome_scene.is_some()
            && !self.has_user_round()
            && self.welcome_animation_frames < WELCOME_ANIMATION_FRAMES
    }

    /// 推进一帧欢迎动画。
    ///
    /// 返回值从"有没有欢迎场景"改成了"**还要不要重绘**"：
    /// `true` = 动画还有后续帧，调用方应请求重绘；`false` = 不必重绘。
    /// 注释见 `welcome_animation_pending()`。
    ///
    /// 调用点只有 `host.rs` 两处，两处都按这个返回值决定要不要 `frame.request(...)`：
    /// - `tick()`：回合中的 33ms tick（回合里欢迎卡通常已经撤下，于是不再产生重绘）；
    /// - `read_prompt()`：空闲等待输入时 poll 超时的分支（issue #38 的修复点）。
    pub(crate) fn tick_welcome(&mut self) -> bool {
        if !self.welcome_animation_pending() {
            return false;
        }
        let Some(scene) = &mut self.welcome_scene else {
            return false;
        };
        scene.tick();
        self.welcome_animation_frames = self.welcome_animation_frames.saturating_add(1);
        true
    }

    pub(crate) fn welcome_scene(&self) -> Option<&WelcomeScene> {
        self.welcome_scene.as_ref()
    }

    pub(crate) fn record_agent_status(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Completed { usage, .. } => {
                self.last_usage = *usage;
            }
            AgentEvent::ContextStatus {
                active_context_tokens,
                token_limit_reached,
                compacted,
                dropped_messages,
                pressure,
            } => {
                self.active_context_tokens = Some((*active_context_tokens).max(0));
                self.context_token_limit_reached = *token_limit_reached;
                self.context_compacted = *compacted;
                self.context_dropped_messages = *dropped_messages;
                self.context_pressure = *pressure;
            }
            _ => {}
        }
    }

    fn context_status_label(&self) -> Option<String> {
        let active_context_tokens = self.active_context_tokens?;
        if self.context_compacted {
            if self.context_dropped_messages > 0 {
                return Some(format!(
                    "上下文已压缩 · 保留 {active_context_tokens} · 丢弃 {}",
                    self.context_dropped_messages
                ));
            }
            return Some(format!("上下文已压缩 · 当前 {active_context_tokens}"));
        }
        if self.context_pressure {
            return Some(format!("上下文接近压缩 · {active_context_tokens}"));
        }
        if self.context_token_limit_reached {
            return Some(format!("上下文接近上限 · {active_context_tokens}"));
        }
        Some(format!("上下文 {active_context_tokens}"))
    }

    pub(crate) fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub(crate) fn has_user_round(&self) -> bool {
        self.transcript.cells().iter().any(|cell| {
            matches!(
                cell.kind(),
                crate::chat::HistoryCellKind::User(_)
                    | crate::chat::HistoryCellKind::Assistant { .. }
                    | crate::chat::HistoryCellKind::Tool(_)
            )
        })
    }

    pub(crate) fn provider_live(&self) -> Option<bool> {
        self.banner.as_ref().map(|banner| banner.provider_live)
    }

    pub(crate) fn viewport(&self) -> &TranscriptViewport {
        &self.viewport
    }

    pub(crate) fn bottom_pane(&self) -> &BottomPane {
        &self.bottom_pane
    }

    pub(crate) fn bottom_pane_mut(&mut self) -> &mut BottomPane {
        &mut self.bottom_pane
    }

    pub(crate) fn control_snapshot(&self) -> Option<&ControlSnapshot> {
        self.control_snapshot.as_ref()
    }

    pub(crate) fn details(&self) -> Option<&str> {
        self.details.as_deref()
    }

    pub(crate) fn focus_target(&self) -> FocusTarget {
        self.focus
    }

    pub(crate) fn focus_next(&mut self) {
        self.focus = match self.focus {
            FocusTarget::Composer => FocusTarget::History,
            FocusTarget::History => FocusTarget::Composer,
            other => other,
        };
    }

    pub(crate) fn focus_previous(&mut self) {
        self.focus_next();
    }

    pub(crate) fn detail_scroll_up(&mut self, lines: u16) {
        self.details_scroll = self.details_scroll.saturating_sub(lines.max(1));
    }

    pub(crate) fn detail_scroll_down(&mut self, lines: u16) {
        self.details_scroll = self.details_scroll.saturating_add(lines.max(1));
    }

    pub(crate) fn details_scroll(&self) -> u16 {
        self.details_scroll
    }

    pub(crate) fn show_control_snapshot(&mut self, snapshot: ControlSnapshot) {
        self.previous_focus = self.focus;
        self.details = None;
        self.details_scroll = 0;
        self.control_snapshot = Some(snapshot);
        self.focus = FocusTarget::Details;
    }

    pub(crate) fn show_transcript(&mut self) {
        self.control_snapshot = None;
        self.details = None;
        self.focus = match self.bottom_pane.mode() {
            BottomPaneMode::Approval { .. } => FocusTarget::Approval,
            BottomPaneMode::UserInput { .. } | BottomPaneMode::Composer => FocusTarget::Composer,
        };
    }

    pub(crate) fn close_details(&mut self) {
        self.details = None;
        self.control_snapshot = None;
        self.details_scroll = 0;
        self.focus = self.previous_focus;
    }

    pub(crate) fn footer_for_width(&self, width: usize) -> String {
        if self.focus == FocusTarget::Details {
            return TextLayout::priority_line(
                &[
                    PrioritySegment::new("Esc close details", ClipPriority::MustKeep),
                    PrioritySegment::new("wheel/PgUp/PgDown scroll", ClipPriority::Important),
                ],
                width,
            );
        }
        if self.focus == FocusTarget::History {
            return TextLayout::priority_line(
                &[
                    PrioritySegment::new("Tab composer", ClipPriority::MustKeep),
                    PrioritySegment::new("wheel/drag/PgUp/PgDown scroll", ClipPriority::Important),
                ],
                width,
            );
        }
        if self.focus == FocusTarget::Approval {
            return TextLayout::priority_line(
                &[
                    PrioritySegment::new(APPROVAL_HINT_PRIMARY, ClipPriority::MustKeep),
                    PrioritySegment::new(APPROVAL_HINT_SECONDARY, ClipPriority::Important),
                ],
                width,
            );
        }
        if self.realtime_voice_enabled {
            return TextLayout::priority_line(
                &[
                    PrioritySegment::new("Speak naturally", ClipPriority::MustKeep),
                    PrioritySegment::new("q + Enter stop", ClipPriority::Important),
                    PrioritySegment::new("Ctrl+C stop", ClipPriority::Important),
                ],
                width,
            );
        }
        match self.viewport.scroll_status() {
            "new output below" => TextLayout::priority_line(
                &[
                    PrioritySegment::new("End 回到最新", ClipPriority::MustKeep),
                    PrioritySegment::new("有新输出", ClipPriority::Important),
                    PrioritySegment::new("wheel/drag history", ClipPriority::Optional),
                ],
                width,
            ),
            "history" => TextLayout::priority_line(
                &[
                    PrioritySegment::new("End 回到最新", ClipPriority::MustKeep),
                    PrioritySegment::new("已上滚", ClipPriority::Important),
                    PrioritySegment::new("wheel/drag PgUp/PgDown", ClipPriority::Optional),
                ],
                width,
            ),
            _ if self.timeline.has_active_sessions() || self.turn_started_at.is_some() => {
                let spinner = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];
                let elapsed = self
                    .turn_started_at
                    .map(|started| started.elapsed().as_secs())
                    .unwrap_or_default();
                let running_label = format!("{spinner} 运行中 · {elapsed}s");
                let usage_label = self.last_usage.map(|usage| {
                    format!(
                        "↑{} ↓{}",
                        usage.input_tokens.max(0),
                        usage.output_tokens.max(0)
                    )
                });
                let context_label = self.context_status_label();
                let mut segments =
                    vec![PrioritySegment::new(&running_label, ClipPriority::MustKeep)];
                if let Some(model) = self.banner.as_ref().map(|banner| banner.model.as_str()) {
                    segments.push(PrioritySegment::new(model, ClipPriority::Important));
                }
                if let Some(usage_label) = usage_label.as_deref() {
                    segments.push(PrioritySegment::new(usage_label, ClipPriority::Optional));
                }
                if let Some(context_label) = context_label.as_deref() {
                    segments.push(PrioritySegment::new(context_label, ClipPriority::Optional));
                }
                segments.push(PrioritySegment::new("Ctrl+C 取消", ClipPriority::Important));
                segments.push(PrioritySegment::new(
                    "输入会保留草稿",
                    ClipPriority::DebugOnly,
                ));
                TextLayout::priority_line(&segments, width)
            }
            _ => {
                let context_label = self.context_status_label();
                let mut segments = vec![
                    PrioritySegment::new("Enter 发送", ClipPriority::MustKeep),
                    PrioritySegment::new("Ctrl+C 退出", ClipPriority::Important),
                    PrioritySegment::new("/help 命令", ClipPriority::Optional),
                    PrioritySegment::new("Alt+Enter 换行", ClipPriority::Optional),
                    PrioritySegment::new("滚轮/拖拽 滚动", ClipPriority::DebugOnly),
                ];
                if self.banner.is_some() && !self.has_user_round() {
                    segments.insert(
                        2,
                        PrioritySegment::new("试着说说你想做什么", ClipPriority::Optional),
                    );
                }
                if let Some(context_label) = context_label.as_deref() {
                    segments.insert(
                        segments.len().saturating_sub(1),
                        PrioritySegment::new(context_label, ClipPriority::Optional),
                    );
                }
                TextLayout::priority_line(&segments, width)
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn footer(&self) -> String {
        self.footer_for_width(usize::MAX)
    }

    pub(crate) fn header_for_width(&self, width: usize) -> String {
        match &self.banner {
            Some(banner) => {
                let model = TextLayout::truncate(&banner.model, width.saturating_sub(20).max(8));
                let state = if self.timeline.has_active_sessions() {
                    "运行中"
                } else if banner.provider_live {
                    "就绪"
                } else {
                    "离线"
                };
                TextLayout::priority_line(
                    &[
                        PrioritySegment::new("云熙", ClipPriority::MustKeep),
                        PrioritySegment::new(&model, ClipPriority::Important),
                        PrioritySegment::new(state, ClipPriority::Optional),
                    ],
                    width,
                )
            }
            None => TextLayout::truncate("云熙 · 初始化中", width),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn header(&self) -> String {
        self.header_for_width(usize::MAX)
    }

    pub(crate) fn subheader_for_width(&self, width: usize) -> String {
        let view = match self.viewport.scroll_status() {
            "history" => "↑ 已上滚 · End 回到最新",
            "new output below" => "↓ 有新输出 · End 回到最新",
            _ => "",
        };
        if !view.is_empty() {
            return TextLayout::truncate(view, width);
        }
        if self.timeline.has_active_sessions() {
            return TextLayout::truncate("处理中 · Ctrl+C 取消", width);
        }
        if self.realtime_voice_enabled {
            return TextLayout::truncate("语音已开启", width);
        }
        String::new()
    }

    #[allow(dead_code)]
    pub(crate) fn subheader(&self) -> String {
        self.subheader_for_width(usize::MAX)
    }

    pub(crate) fn start_prompt(&mut self, prompt: &str) {
        self.control_snapshot = None;
        self.details = None;
        self.focus = FocusTarget::Composer;
        self.bottom_pane.start_composer(prompt);
    }

    pub(crate) fn prepare_prompt(&mut self, prompt: &str) {
        if self.focus != FocusTarget::Details {
            self.start_prompt(prompt);
        }
    }

    pub(crate) fn start_approval(&mut self, request: ApprovalRequestView) {
        self.bottom_pane.start_approval(request);
        self.focus = FocusTarget::Approval;
    }

    pub(crate) fn start_user_input(&mut self, request: UserInputRequestView) {
        self.bottom_pane.start_user_input(request);
        self.focus = FocusTarget::Composer;
    }

    pub(crate) fn push_user(&mut self, value: impl Into<String>) {
        self.show_transcript();
        let mut changed = false;
        for update in self.timeline.finish_active() {
            changed |= self.transcript.apply_assistant_update(update);
        }
        let event = self.presentation.present_user(value);
        self.push_tui_event(event);
        if changed {
            self.on_transcript_changed();
        }
    }

    pub(crate) fn push_agent_event(&mut self, event: &AgentEvent) {
        self.record_agent_status(event);
        let cancelled = matches!(event, AgentEvent::Cancelled { .. });
        let terminal = matches!(
            event,
            AgentEvent::Completed { .. }
                | AgentEvent::Cancelled { .. }
                | AgentEvent::ProviderError { .. }
                | AgentEvent::Error { .. }
        );
        let event = self.presentation.present_agent_event(event);
        self.push_tui_event(event);
        if terminal {
            let mut changed = false;
            let updates = if cancelled {
                self.timeline.cancel_active()
            } else {
                self.timeline.finish_active()
            };
            for update in updates {
                changed |= self.transcript.apply_assistant_update(update);
            }
            if changed {
                self.on_transcript_changed();
            }
        }
    }

    pub(crate) fn push_tui_event(&mut self, mut event: TuiEvent) {
        let has_stream = event.stream.is_some();
        let mut changed = self
            .timeline
            .apply(&mut event)
            .is_some_and(|update| self.transcript.apply_assistant_update(update));
        if event.kind != crate::presentation::TuiCellKind::AssistantMessage || !has_stream {
            let before = self.transcript.cells().len();
            self.transcript.push_tui_event(event);
            changed |= self.transcript.cells().len() != before;
        }
        if changed {
            self.on_transcript_changed();
        }
    }

    pub(crate) fn set_debug_events(&mut self, enabled: bool) {
        self.transcript.set_debug_events(enabled);
        let event = self.presentation.present_notice(
            "debug",
            if enabled {
                "event debug enabled"
            } else {
                "event debug disabled"
            },
        );
        self.push_tui_event(event);
    }

    pub(crate) fn show_details(&mut self, id: Option<usize>) {
        let detail = self.transcript.detail_text(id);
        self.previous_focus = self.focus;
        self.details = Some(detail);
        self.details_scroll = 0;
        self.control_snapshot = None;
        self.focus = FocusTarget::Details;
    }

    pub(crate) fn push_notice(&mut self, kind: &str, message: &str) {
        let event = self.presentation.present_notice(kind, message);
        self.push_tui_event(event);
    }

    pub(crate) fn push_warning(&mut self, message: &str) {
        let event = self.presentation.present_warning(message);
        self.push_tui_event(event);
    }

    pub(crate) fn push_error(&mut self, message: &str) {
        let event = self.presentation.present_error(message);
        self.push_tui_event(event);
    }

    pub(crate) fn clear_transcript(&mut self) {
        self.timeline.clear();
        self.transcript.clear();
        self.viewport.reset();
        // `/clear` 会把空会话的欢迎卡请回来，所以动画预算也要还回去，
        // 否则重开的欢迎界面会是一张静止的星空图。
        self.welcome_animation_frames = 0;
    }

    pub(crate) fn scroll_up(
        &mut self,
        lines: usize,
        wrapped: &WrappedTranscript,
        visible_height: usize,
    ) {
        self.viewport.scroll_up(lines, wrapped, visible_height);
    }

    pub(crate) fn scroll_down(
        &mut self,
        lines: usize,
        wrapped: &WrappedTranscript,
        visible_height: usize,
    ) {
        self.viewport.scroll_down(
            lines.max(1).min(visible_height.max(1)),
            wrapped,
            visible_height,
        );
    }

    pub(crate) fn page_up(&mut self, wrapped: &WrappedTranscript, visible_height: usize) {
        self.viewport.page_up(wrapped, visible_height);
    }

    pub(crate) fn page_down(&mut self, wrapped: &WrappedTranscript, visible_height: usize) {
        self.viewport.page_down(wrapped, visible_height);
    }

    pub(crate) fn jump_top(&mut self, wrapped: &WrappedTranscript, visible_height: usize) {
        self.viewport.jump_top(wrapped, visible_height);
    }

    pub(crate) fn follow_tail(&mut self) {
        self.viewport.follow_tail();
    }

    pub(crate) fn set_scroll_fraction(
        &mut self,
        numerator: usize,
        denominator: usize,
        wrapped: &WrappedTranscript,
        visible_height: usize,
    ) {
        self.viewport
            .set_scroll_fraction(numerator, denominator, wrapped, visible_height);
    }

    pub(crate) fn reanchor_viewport(&mut self, wrapped: &WrappedTranscript, visible_height: usize) {
        self.viewport.reanchor(wrapped, visible_height);
    }

    fn on_transcript_changed(&mut self) {
        self.viewport.on_content_changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::HistoryCellKind;
    use yunxi_agent_core::{
        AgentMessageSequence, AgentMessageStream, AgentMessageStreamPhase, AgentRunStatus,
        TokenUsage,
    };

    fn banner() -> YunxiTuiBanner {
        YunxiTuiBanner {
            cwd: "D:/YunXi Agent/crates/yunxi-agent-cli".to_string(),
            backend: "yunxi".to_string(),
            provider_live: false,
            provider_source: "offline_static".to_string(),
            model: "deepseek-chat".to_string(),
            provider: "static".to_string(),
        }
    }

    fn assistant_event(content: &str, sequence: u64, phase: AgentMessageStreamPhase) -> AgentEvent {
        assistant_event_for("turn-live", "message-live", content, sequence, phase)
    }

    fn assistant_event_for(
        turn_id: &str,
        stream_id: &str,
        content: &str,
        sequence: u64,
        phase: AgentMessageStreamPhase,
    ) -> AgentEvent {
        AgentEvent::Message {
            content: content.to_string(),
            stream: Some(AgentMessageStream {
                thread_id: "thread-live".to_string(),
                turn_id: turn_id.to_string(),
                stream_id: stream_id.to_string(),
                event_id: format!("fallback:app-test:{turn_id}:{stream_id}:{sequence}"),
                source_sequence: AgentMessageSequence::LocalFallback(sequence),
                phase,
            }),
        }
    }

    #[test]
    fn narrow_header_keeps_human_status_summary() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());

        let header = app.header_for_width(80);
        let subheader = app.subheader_for_width(80);
        let footer = app.footer_for_width(80);

        assert!(TextLayout::measure(&header) <= 80);
        assert!(TextLayout::measure(&subheader) <= 80);
        assert!(TextLayout::measure(&footer) <= 80);
        assert!(header.contains("云熙"));
        assert!(header.contains("deepseek-chat"));
        assert!(header.contains("离线"));
        assert!(!header.contains("v2."));
        assert!(!header.contains("model="));
        assert!(!header.contains("D:/"));
        assert!(!header.contains("yunxi-agent-cli"));
        assert!(!header.contains(".../"));
        assert!(subheader.is_empty());
        assert!(footer.contains("Enter 发送"));
        assert!(footer.contains("Ctrl+C 退出"));
    }

    #[test]
    fn welcome_surface_can_be_disabled_without_touching_transcript() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        assert!(app.welcome_enabled());
        app.set_welcome_enabled(false);
        assert!(!app.welcome_enabled());
        assert!(!app.has_user_round());
    }

    #[test]
    fn idle_welcome_animation_advances_frames_and_then_stops_for_good() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        assert!(app.welcome_animation_pending());

        // 空闲路径上推进的第一帧必须真的改变画面：
        // issue #38 里测试全绿而实机静止，就是因为单测只验证了 `tick()` 本身，
        // 没人验证"谁在什么时机调用它"。
        let before = app.welcome_scene().expect("welcome scene").render(80, 24);
        assert!(app.tick_welcome());
        let after = app.welcome_scene().expect("welcome scene").render(80, 24);
        assert_ne!(before, after, "空闲态推进一帧后欢迎界面必须改变");

        // 预算是有限的：动画播完就不再供帧，空闲态不会永远 30fps 重绘。
        let mut frames = 1;
        while app.tick_welcome() {
            frames += 1;
            assert!(frames <= WELCOME_ANIMATION_FRAMES, "欢迎动画帧数必须封顶");
        }
        assert_eq!(frames, WELCOME_ANIMATION_FRAMES);
        assert!(!app.welcome_animation_pending());
        assert!(!app.tick_welcome(), "预算花完后不应再要求重绘");
    }

    #[test]
    fn welcome_animation_only_runs_while_its_card_is_on_screen() {
        // 首启**也**走动画界面：清单现在接在动画下方渲染（render.rs 不再按
        // checklist 是否为空二选一），所以首启同样需要供帧，否则用户看到的是
        // 一张静止的第一帧。
        let mut app = YunxiTuiApp::default();
        app.set_welcome_checklist(vec!["  工作区                 ✓ /tmp".to_string()]);
        assert!(app.welcome_animation_pending());
        assert!(app.tick_welcome());

        // YUNXI_TUI_BANNER=0：没有欢迎卡。
        let mut app = YunxiTuiApp::default();
        app.set_welcome_enabled(false);
        assert!(!app.welcome_animation_pending());
        assert!(!app.tick_welcome());

        // 首轮真实对话之后，欢迎卡被 transcript 取代。
        let mut app = YunxiTuiApp::default();
        app.push_user("你好");
        assert!(!app.welcome_animation_pending());
        assert!(!app.tick_welcome());
    }

    #[test]
    fn clear_transcript_gives_the_welcome_animation_its_budget_back() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        while app.tick_welcome() {}
        assert!(!app.welcome_animation_pending());

        app.push_user("你好");
        app.clear_transcript();

        assert!(!app.has_user_round());
        assert!(app.welcome_animation_pending());
        assert!(app.tick_welcome());
    }

    #[test]
    fn empty_composer_footer_offers_a_natural_language_prompt() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        assert!(app.footer_for_width(120).contains("试着说说你想做什么"));

        app.push_user("列出当前目录");
        assert!(!app.footer_for_width(120).contains("试着说说你想做什么"));
    }

    #[test]
    fn active_turn_footer_shows_spinner_elapsed_model_and_usage() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(YunxiTuiBanner {
            provider_live: true,
            model: "agnes-2.5-flash".to_string(),
            ..banner()
        });
        app.begin_turn();
        app.record_agent_status(&AgentEvent::ContextStatus {
            active_context_tokens: 180,
            token_limit_reached: false,
            compacted: false,
            dropped_messages: 0,
            pressure: false,
        });
        app.record_agent_status(&AgentEvent::Completed {
            status: AgentRunStatus::Completed,
            usage: Some(TokenUsage {
                input_tokens: 1200,
                cached_input_tokens: 100,
                output_tokens: 340,
                reasoning_output_tokens: 0,
            }),
        });

        let footer = app.footer_for_width(120);

        assert!(footer.contains("运行中"));
        assert!(footer.contains("agnes-2.5-flash"));
        assert!(footer.contains("↑1200 ↓340"));
        assert!(footer.contains("上下文 180"));
        assert!(footer.contains("Ctrl+C 取消"));
    }

    #[test]
    fn idle_footer_explains_context_compaction() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.record_agent_status(&AgentEvent::ContextStatus {
            active_context_tokens: 4096,
            token_limit_reached: true,
            compacted: true,
            dropped_messages: 3,
            pressure: false,
        });

        let footer = app.footer_for_width(160);

        assert!(footer.contains("上下文已压缩"));
        assert!(footer.contains("丢弃 3"));
    }

    #[test]
    fn idle_footer_warns_before_context_compaction() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.record_agent_status(&AgentEvent::ContextStatus {
            active_context_tokens: 19_000,
            token_limit_reached: false,
            compacted: false,
            dropped_messages: 0,
            pressure: true,
        });

        let footer = app.footer_for_width(160);

        assert!(footer.contains("上下文接近压缩"));
    }

    #[test]
    fn spinner_advances_only_while_a_turn_is_active() {
        let mut app = YunxiTuiApp::default();
        assert!(!app.advance_spinner());
        app.begin_turn();
        assert!(app.advance_spinner());
        app.end_turn();
        assert!(!app.advance_spinner());
    }

    #[test]
    fn wide_header_avoids_repeating_diagnostic_details() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());

        let header = app.header_for_width(120);
        let subheader = app.subheader_for_width(120);

        assert_eq!(header, "云熙 | deepseek-chat | 离线");
        assert!(!header.contains("model="));
        assert!(!header.contains("D:/"));
        assert!(!header.contains("yunxi-agent-cli"));
        assert!(subheader.is_empty());
        assert!(!subheader.contains("backend="));
        assert!(!subheader.contains("source="));
    }

    #[test]
    fn realtime_voice_state_is_visible_without_changing_the_banner_contract() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        assert!(!app.subheader_for_width(120).contains("语音"));

        app.set_realtime_voice_enabled(true);

        assert!(app.subheader_for_width(120).contains("语音"));
        assert!(app.subheader_for_width(70).contains("语音"));
        assert!(app.footer_for_width(120).contains("q + Enter stop"));
    }

    #[test]
    fn medium_header_truncates_model_without_cwd_or_provider() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(YunxiTuiBanner {
            model: "deepseek-chat-ultra-long-model-name".to_string(),
            provider_live: true,
            provider: "deepseek".to_string(),
            ..banner()
        });

        let header = app.header_for_width(100);

        assert!(TextLayout::measure(&header) <= 100);
        assert!(header.contains("deepseek-chat"));
        assert!(!header.contains("deepseek live"));
        assert!(!header.contains("model="));
        assert!(!header.contains("D:/"));
        assert!(!header.contains("yunxi-agent-cli"));
        assert!(!header.contains(".../"));
    }

    #[test]
    fn responsive_status_priority_is_stable_across_width_matrix() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(YunxiTuiBanner {
            cwd: "C:\\Users\\24763\\YunXi Agent\\包含空格\\very-long-workspace".to_string(),
            model: "deepseek-chat-ultra-long-model-name".to_string(),
            provider_live: true,
            provider: "deepseek".to_string(),
            ..banner()
        });

        for width in [58, 80, 100, 120, 200] {
            let header = app.header_for_width(width);
            let subheader = app.subheader_for_width(width);
            let footer = app.footer_for_width(width);
            assert!(TextLayout::measure(&header) <= width, "width={width}");
            assert!(TextLayout::measure(&subheader) <= width, "width={width}");
            assert!(TextLayout::measure(&footer) <= width, "width={width}");
            assert!(header.contains("云熙"), "width={width}");
            assert!(header.contains("deepseek-chat"), "width={width}");
            assert!(!header.contains("model="), "width={width}");
            assert!(!header.contains("deepseek live"), "width={width}");
            assert!(subheader.is_empty(), "width={width}");
            assert!(footer.contains("Enter 发送"), "width={width}");
        }
        let narrow = app.header_for_width(80);
        assert!(!narrow.contains("model="));
        assert!(!narrow.contains("C:"));
        assert!(!narrow.contains("very-long-workspace"));
        assert!(!narrow.contains(".../"));

        let tight_subheader = app.subheader_for_width(58);
        assert!(tight_subheader.is_empty());
        assert!(!tight_subheader.contains("cells="));
        assert!(!tight_subheader.contains("backend="));
        assert!(!tight_subheader.contains("source="));
        assert!(!tight_subheader.contains("debug"));
        assert!(!tight_subheader.contains("live"));

        let medium = app.header_for_width(100);
        assert!(medium.contains("deepseek-chat"));
        assert!(!medium.contains("C:"));
        assert!(!medium.contains("very-long-workspace"));
        assert!(!medium.contains(".../"));

        for width in [120, 200] {
            let wide = app.header_for_width(width);
            assert!(wide.contains("deepseek-chat"), "width={width}");
            assert!(!wide.contains("very-long-workspace"), "width={width}");
        }
    }

    #[test]
    fn provider_shaped_delta_and_final_leave_one_canonical_assistant_cell() {
        let mut app = YunxiTuiApp::default();
        app.push_user("你好");
        app.push_agent_event(&assistant_event("你", 1, AgentMessageStreamPhase::Delta));
        app.push_agent_event(&assistant_event(
            "你好，我在。",
            2,
            AgentMessageStreamPhase::Final,
        ));
        app.push_agent_event(&AgentEvent::Completed {
            status: AgentRunStatus::Completed,
            usage: Some(TokenUsage {
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 4,
                reasoning_output_tokens: 0,
            }),
        });

        let assistant_cells = app
            .transcript()
            .cells()
            .iter()
            .filter_map(|cell| match cell.kind() {
                HistoryCellKind::Assistant { content, active } => Some((content, active)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(assistant_cells, vec![(&"你好，我在。".to_string(), &false)]);
        assert_eq!(app.transcript().cells().len(), 2);
    }

    #[test]
    fn final_update_preserves_history_scroll_position() {
        let mut app = YunxiTuiApp::default();
        for index in 0..30 {
            app.push_notice("history", &format!("history line {index}"));
        }
        app.push_user("history test");
        app.push_agent_event(&assistant_event(
            "partial",
            1,
            AgentMessageStreamPhase::Delta,
        ));
        let wrapped_before =
            crate::transcript_layout::build_wrapped_transcript(app.transcript().cells(), 80);
        app.scroll_up(4, &wrapped_before, 10);
        let anchor_before = wrapped_before
            .anchor_at(app.viewport().view_start(&wrapped_before, 10))
            .cloned();

        app.push_agent_event(&assistant_event(
            "final answer",
            2,
            AgentMessageStreamPhase::Final,
        ));

        let wrapped_after =
            crate::transcript_layout::build_wrapped_transcript(app.transcript().cells(), 80);
        let anchor_after = wrapped_after
            .anchor_at(app.viewport().view_start(&wrapped_after, 10))
            .cloned();
        assert_eq!(anchor_after, anchor_before);
        assert_eq!(app.viewport().scroll_status(), "new output below");
    }

    #[test]
    fn active_stream_footer_reports_history_instead_of_claiming_tail() {
        let mut app = YunxiTuiApp::default();
        for index in 0..30 {
            app.push_notice("history", &format!("history line {index}"));
        }
        app.push_agent_event(&assistant_event(
            "partial",
            1,
            AgentMessageStreamPhase::Delta,
        ));
        let wrapped =
            crate::transcript_layout::build_wrapped_transcript(app.transcript().cells(), 80);
        app.scroll_up(5, &wrapped, 10);

        let footer = app.footer_for_width(100);

        assert!(footer.contains("已上滚"));
        assert!(!footer.starts_with("streaming current turn"));
    }

    #[test]
    fn focus_footers_only_advertise_available_actions() {
        let mut app = YunxiTuiApp::default();
        let composer = app.footer_for_width(100);
        assert!(composer.contains("Enter 发送"));
        assert!(composer.contains("滚轮/拖拽 滚动"));

        app.focus_next();
        let history = app.footer_for_width(100);
        assert!(history.contains("wheel/drag/PgUp/PgDown scroll"));

        app.start_approval(ApprovalRequestView {
            id: None,
            tool_name: "shell".to_string(),
            cwd: ".".to_string(),
            command: Some("echo safe".to_string()),
            reason: "test".to_string(),
            risk_label: None,
        });
        let approval = app.footer_for_width(100);
        assert!(approval.contains("Enter 确认选中项"));
        assert!(approval.contains("Tab/Shift+Tab 选择"));
        assert!(approval.contains("Esc 拒绝"));
        assert!(approval.contains("Y 批准"));
        assert!(!approval.contains("wheel"));

        app.show_details(None);
        let details = app.footer_for_width(100);
        assert!(details.contains("Esc close details"));
        assert!(details.contains("wheel/PgUp/PgDown scroll"));
    }

    #[test]
    fn active_stream_resize_then_cancel_preserves_pinned_cell() {
        let mut app = YunxiTuiApp::default();
        for index in 0..24 {
            app.push_notice(
                "history",
                &format!("history line {index} with wrapped text"),
            );
        }
        app.push_agent_event(&assistant_event(
            "partial 中文 👨‍👩‍👧‍👦",
            1,
            AgentMessageStreamPhase::Delta,
        ));
        let narrow =
            crate::transcript_layout::build_wrapped_transcript(app.transcript().cells(), 28);
        app.scroll_up(8, &narrow, 8);

        let wide = crate::transcript_layout::build_wrapped_transcript(app.transcript().cells(), 90);
        app.reanchor_viewport(&wide, 12);
        let expected_after_resize = wide
            .anchor_at(app.viewport().view_start(&wide, 12))
            .unwrap()
            .cell_id
            .clone();
        app.push_agent_event(&AgentEvent::Cancelled {
            reason: Some("current turn cancelled".to_string()),
        });
        let cancelled =
            crate::transcript_layout::build_wrapped_transcript(app.transcript().cells(), 90);
        let actual = cancelled
            .anchor_at(app.viewport().view_start(&cancelled, 12))
            .unwrap()
            .cell_id
            .clone();

        assert_eq!(actual, expected_after_resize);
        assert_eq!(app.viewport().scroll_status(), "new output below");
        assert!(!app.timeline.has_active_sessions());
    }

    #[test]
    fn cancellation_freezes_partial_assistant_cell_and_accepts_next_input() {
        let mut app = YunxiTuiApp::default();
        app.push_user("first prompt");
        app.push_agent_event(&assistant_event(
            "partial 中文 👨‍👩‍👧‍👦",
            1,
            AgentMessageStreamPhase::Delta,
        ));

        app.push_agent_event(&AgentEvent::Cancelled {
            reason: Some("current turn cancelled".to_string()),
        });
        app.push_user("next prompt");

        assert!(app.transcript().cells().iter().any(|cell| matches!(
            cell.kind(),
            HistoryCellKind::Assistant { content, active }
                if content == "partial 中文 👨‍👩‍👧‍👦" && !active
        )));
        assert!(app.transcript().cells().iter().any(|cell| matches!(
            cell.kind(),
            HistoryCellKind::User(content) if content == "next prompt"
        )));
        assert!(!app.timeline.has_active_sessions());
    }

    #[test]
    fn provider_disconnect_freezes_partial_cell_shows_summary_and_allows_next_turn() {
        let mut app = YunxiTuiApp::default();
        app.push_user("first prompt");
        app.push_agent_event(&assistant_event_for(
            "turn-disconnect",
            "message-disconnect",
            "partial response",
            1,
            AgentMessageStreamPhase::Delta,
        ));
        app.push_agent_event(&AgentEvent::ProviderError {
            provider: "fixture".to_string(),
            status: None,
            classification: "network".to_string(),
            message: "stream disconnected".to_string(),
        });
        app.push_user("next prompt");
        app.push_agent_event(&assistant_event_for(
            "turn-next",
            "message-next",
            "next answer",
            1,
            AgentMessageStreamPhase::Final,
        ));

        assert!(app.transcript().cells().iter().any(|cell| matches!(
            cell.kind(),
            HistoryCellKind::Assistant { content, active }
                if content == "partial response" && !active
        )));
        assert!(app.transcript().cells().iter().any(|cell| matches!(
            cell.kind(),
            HistoryCellKind::Error(content) if content.contains("YX-PROVIDER-001")
        )));
        assert!(app.transcript().cells().iter().any(|cell| matches!(
            cell.kind(),
            HistoryCellKind::Assistant { content, active }
                if content == "next answer" && !active
        )));
        assert!(!app.timeline.has_active_sessions());
    }

    #[test]
    fn history_eviction_keeps_viewport_anchor_within_retained_rows() {
        let mut app = YunxiTuiApp::default();
        for index in 0..900 {
            app.push_notice("history", &format!("retained history line {index}"));
        }
        let wrapped =
            crate::transcript_layout::build_wrapped_transcript(app.transcript().cells(), 58);
        app.scroll_up(200, &wrapped, 18);
        app.push_notice("history", "latest after eviction");
        let updated =
            crate::transcript_layout::build_wrapped_transcript(app.transcript().cells(), 58);
        let start = app.viewport().view_start(&updated, 18);

        assert!(start <= crate::viewport::max_start(updated.rows.len(), 18));
        assert!(updated.anchor_at(start).is_some());
        assert!(app.transcript().cells().len() <= crate::chat::MAX_HISTORY_CELLS);
    }

    #[test]
    fn overlays_and_control_snapshots_preserve_composer_text_and_cursor() {
        let mut app = YunxiTuiApp::default();
        app.bottom_pane_mut().paste("draft 中文👩‍💻");
        app.bottom_pane_mut()
            .handle_composer_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Left,
                crossterm::event::KeyModifiers::NONE,
            ));
        let expected = app.bottom_pane().composer_snapshot();

        app.start_approval(ApprovalRequestView {
            id: Some("approval-1".to_string()),
            tool_name: "shell".to_string(),
            cwd: ".".to_string(),
            command: Some("echo ok".to_string()),
            reason: "test".to_string(),
            risk_label: None,
        });
        app.start_prompt("yunxi> ");
        assert_eq!(app.bottom_pane().composer_snapshot(), expected);

        app.start_user_input(UserInputRequestView {
            id: Some("input-1".to_string()),
            prompt: "details".to_string(),
        });
        app.start_prompt("yunxi> ");
        app.show_control_snapshot(ControlSnapshot {
            companion_enabled: false,
            cloud_control_enabled: false,
            quiet_hours: None,
            persona_summary: String::new(),
            memory_summary: String::new(),
            relationship_summary: String::new(),
            scopes: Vec::new(),
            recent_change: None,
        });
        app.show_transcript();
        assert_eq!(app.bottom_pane().composer_snapshot(), expected);
    }

    #[test]
    fn details_restore_prior_focus_draft_and_approval_selection() {
        let mut app = YunxiTuiApp::default();
        app.bottom_pane_mut().paste("details draft 中文👩‍💻");
        let draft = app.bottom_pane().composer_snapshot();

        app.show_details(None);
        assert_eq!(app.focus_target(), FocusTarget::Details);
        app.detail_scroll_down(4);
        assert_eq!(app.details_scroll(), 4);
        app.close_details();
        assert_eq!(app.focus_target(), FocusTarget::Composer);
        assert_eq!(app.bottom_pane().composer_snapshot(), draft);

        app.start_approval(ApprovalRequestView {
            id: Some("approval-details".to_string()),
            tool_name: "shell".to_string(),
            cwd: ".".to_string(),
            command: Some("echo ok".to_string()),
            reason: "test".to_string(),
            risk_label: None,
        });
        app.bottom_pane_mut()
            .handle_approval_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Tab,
                crossterm::event::KeyModifiers::NONE,
            ));
        let approval = app.bottom_pane().mode().clone();

        app.show_details(None);
        app.close_details();
        assert_eq!(app.focus_target(), FocusTarget::Approval);
        assert_eq!(app.bottom_pane().mode(), &approval);
        app.start_prompt("yunxi> ");
        assert_eq!(app.bottom_pane().composer_snapshot(), draft);
    }

    #[test]
    fn prompt_preparation_keeps_details_open_until_escape_closes_it() {
        let mut app = YunxiTuiApp::default();
        app.bottom_pane_mut().paste("details draft");
        app.show_details(None);
        app.prepare_prompt("yunxi> ");

        assert_eq!(app.focus_target(), FocusTarget::Details);
        assert!(app.details().is_some());
        app.close_details();
        app.prepare_prompt("yunxi> ");
        assert_eq!(app.focus_target(), FocusTarget::Composer);
        assert!(app.details().is_none());
    }

    #[test]
    fn streaming_final_and_completion_do_not_touch_pretyped_draft_or_duplicate_answer() {
        let mut app = YunxiTuiApp::default();
        app.push_user("first prompt");
        app.push_agent_event(&assistant_event(
            "partial",
            1,
            AgentMessageStreamPhase::Delta,
        ));
        app.bottom_pane_mut().paste("next 草稿👩‍💻");
        let expected = app.bottom_pane().composer_snapshot();

        app.push_agent_event(&assistant_event(
            "final answer",
            2,
            AgentMessageStreamPhase::Final,
        ));
        app.push_agent_event(&AgentEvent::Completed {
            status: AgentRunStatus::Completed,
            usage: None,
        });

        let assistant_cells = app
            .transcript()
            .cells()
            .iter()
            .filter(|cell| matches!(cell.kind(), HistoryCellKind::Assistant { .. }))
            .count();
        assert_eq!(assistant_cells, 1);
        assert_eq!(app.bottom_pane().composer_snapshot(), expected);
    }
}
