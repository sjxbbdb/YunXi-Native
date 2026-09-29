use crate::TuiEvent;
use crate::app::{YunxiTuiApp, YunxiTuiBanner};
use crate::bottom_pane::{
    ApprovalAction, ApprovalDecision, ApprovalRequestView, ComposerAction, UserInputAction,
    UserInputRequestView, UserInputResponse,
};
use crate::frame::{DEFAULT_MIN_FRAME_INTERVAL, RedrawPriority, RedrawReason, RedrawScheduler};
use crate::input_map::{FocusTarget, TuiAction, resolve_event, resolve_key};
use crate::layout::{compute_layout, rect_contains};
use crate::render::render_tui_frame;
use crate::scrollbar::{ScrollbarHit, TranscriptScrollbarGeometry};
use crate::transcript_layout::{WrappedTranscript, build_wrapped_transcript};
use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind, poll, read,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Rect, Size};
use std::io::{self, Stdout, Write};
use std::time::{Duration, Instant};
use yunxi_agent_core::{AgentEvent, AgentMessageStreamPhase, ControlSnapshot};

pub struct YunxiTui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    app: YunxiTuiApp,
    frame: RedrawScheduler,
    scroll_drag: Option<TranscriptScrollDrag>,
    windows_input_burst: WindowsInputBurst,
    _guard: TerminalGuard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiTickAction {
    None,
    CancelCurrentTurn,
}

/// 空闲等待输入时推进欢迎动画的节拍。
///
/// 直接复用 `RedrawScheduler` 的最小帧间隔：这样每次超时都刚好越过合并阈值，
/// 不会出现"动画推进了一帧、重绘却被节流丢掉"的空转。
const IDLE_ANIMATION_TICK: Duration = DEFAULT_MIN_FRAME_INTERVAL;

impl YunxiTui {
    pub fn enter() -> Result<Self> {
        let guard = TerminalGuard::enter()?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        Ok(Self {
            terminal,
            app: YunxiTuiApp::default(),
            frame: RedrawScheduler::default(),
            scroll_drag: None,
            windows_input_burst: WindowsInputBurst::default(),
            _guard: guard,
        })
    }

    pub fn set_banner(&mut self, banner: YunxiTuiBanner) -> Result<()> {
        self.app.set_banner(banner);
        self.request_redraw(RedrawReason::StatusChanged)
    }

    pub fn set_welcome_enabled(&mut self, enabled: bool) -> Result<()> {
        self.app.set_welcome_enabled(enabled);
        self.request_redraw(RedrawReason::StatusChanged)
    }

    pub fn set_welcome_checklist(&mut self, checklist: Vec<String>) -> Result<()> {
        self.app.set_welcome_checklist(checklist);
        self.request_redraw(RedrawReason::StatusChanged)
    }

    pub fn set_realtime_voice_enabled(&mut self, enabled: bool) -> Result<()> {
        self.app.set_realtime_voice_enabled(enabled);
        self.request_redraw(RedrawReason::StatusChanged)
    }

    pub fn begin_turn(&mut self) -> Result<()> {
        self.app.begin_turn();
        self.request_redraw(RedrawReason::StatusChanged)
    }

    pub fn end_turn(&mut self) -> Result<()> {
        self.app.end_turn();
        self.request_redraw(RedrawReason::StatusChanged)
    }

    pub fn clear_transcript(&mut self) -> Result<()> {
        self.app.clear_transcript();
        self.request_redraw(RedrawReason::InputChanged)
    }

    pub fn push_agent_event(&mut self, event: &AgentEvent) -> Result<()> {
        let reason = redraw_reason_for_agent_event(event);
        self.app.push_agent_event(event);
        self.request_redraw(reason)
    }

    pub fn push_tui_event(&mut self, event: TuiEvent) -> Result<()> {
        let reason = redraw_reason_for_tui_event(&event);
        self.app.push_tui_event(event);
        self.request_redraw(reason)
    }

    pub fn push_notice(&mut self, kind: &str, message: &str) -> Result<()> {
        self.app.push_notice(kind, message);
        self.request_redraw(RedrawReason::StatusChanged)
    }

    pub fn push_user_message(&mut self, message: impl Into<String>) -> Result<()> {
        self.app.push_user(message);
        self.request_redraw(RedrawReason::InputChanged)
    }

    pub fn push_warning(&mut self, message: &str) -> Result<()> {
        self.app.push_warning(message);
        self.request_redraw(RedrawReason::Error)
    }

    pub fn push_error(&mut self, message: &str) -> Result<()> {
        self.app.push_error(message);
        self.request_redraw(RedrawReason::Error)
    }

    pub fn set_debug_events(&mut self, enabled: bool) -> Result<()> {
        self.app.set_debug_events(enabled);
        self.request_redraw(RedrawReason::ControlChanged)
    }

    pub fn show_details(&mut self, id: Option<usize>) -> Result<()> {
        self.app.show_details(id);
        self.request_redraw(RedrawReason::ControlChanged)
    }

    pub fn show_control_snapshot(&mut self, snapshot: ControlSnapshot) -> Result<()> {
        self.app.show_control_snapshot(snapshot);
        self.request_redraw(RedrawReason::ControlChanged)
    }

    pub fn tick(&mut self) -> Result<TuiTickAction> {
        let action = self.drain_turn_events()?;
        if self.app.advance_spinner() {
            self.frame.request(RedrawReason::StatusChanged);
        }
        // `tick_welcome()` 的返回值是"动画还要不要重绘"：回合里欢迎卡通常已经撤下，
        // 于是这里不会产生多余的重绘请求；空闲态的供帧在 `read_prompt()` 里。
        if self.app.tick_welcome() {
            self.frame.request(RedrawReason::StatusChanged);
        }
        self.flush_frame(Instant::now())?;
        Ok(action)
    }

    pub fn poll_realtime_voice_stop(&mut self) -> Result<bool> {
        let mut stop = false;
        let mut redraw_reason = None;
        let mut poll_timeout = Duration::ZERO;
        while poll(poll_timeout)? {
            poll_timeout = if cfg!(windows) {
                Duration::from_millis(5)
            } else {
                Duration::ZERO
            };
            let event = read()?;
            if is_ctrl_c_event(&event) {
                stop = true;
                redraw_reason = Some(RedrawReason::CancelCurrentTurn);
                continue;
            }
            if is_realtime_voice_submit_event(&event)
                && is_realtime_voice_stop_draft(self.app.bottom_pane().composer_buffer().text())
            {
                self.app.bottom_pane_mut().reset_composer("yunxi> ");
                stop = true;
                redraw_reason = Some(RedrawReason::InputChanged);
                continue;
            }
            if self.handle_navigation_event(&event)? {
                redraw_reason = Some(if matches!(event, Event::Resize(_, _)) {
                    RedrawReason::Resize
                } else {
                    RedrawReason::ScrollChanged
                });
                continue;
            }
            if apply_turn_draft_event(&mut self.app, &event) {
                redraw_reason = Some(RedrawReason::InputChanged);
            }
        }
        if let Some(reason) = redraw_reason {
            self.frame.request(reason);
        }
        self.flush_frame(Instant::now())?;
        Ok(stop)
    }

    pub fn flush(&mut self) -> Result<()> {
        self.request_draw_now()
    }

    /// 等待用户输入。
    ///
    /// 除了"读一行输入"，这里还负责**空闲态的动画供帧** —— issue #38 的根因就是
    /// `tick()` 只挂在 `run_turn()` 的 33ms 循环上，而欢迎界面属于空闲态，
    /// 于是星空永远停在第一帧。
    ///
    /// 做法（issue 里的方案 A）：把唯一的阻塞 `read()` 换成 `poll(节拍)` + `read()`；
    /// 超时说明用户还在发呆，就推进一帧欢迎动画并重绘。其余语义一律不动：
    /// - 仍然只有用户输入（或 Ctrl+C / Ctrl+D）才会让这个函数返回，
    ///   Linux/Windows 前端和测试看到的还是原来那个"一直等到有输入"的阻塞接口；
    /// - 只有 `welcome_animation_pending()`（真的画在屏幕上、而且还没播完）时才轮询，
    ///   会话回合中、动画播完、欢迎卡被关掉之后直接走阻塞 `read()`，
    ///   一次多余的唤醒都不产生 —— 空闲 CPU 与修复前一致（修复前是 0%）；
    /// - 动画本身是有限时长的开场动效，见 `YunxiTuiApp::WELCOME_ANIMATION_FRAMES`。
    ///
    /// 没选方案 B（把 tick 从 `run_turn` 提到主循环）：`read_prompt` 是公共入口，
    /// 让它"每次只处理一个事件就返回"会把非阻塞状态机推给每个调用点（含 Windows 前端
    /// 与现有测试），而且主循环里那条 tick 通道最终仍旧要在阻塞读上超时，绕一圈回到同一处。
    pub fn read_prompt(&mut self, prompt: &str) -> Result<Option<String>> {
        self.app.prepare_prompt(prompt);
        self.windows_input_burst.reset();
        self.request_draw_now()?;
        loop {
            if let Some(tick) = idle_animation_poll_timeout(&self.app)
                && !poll(tick)?
            {
                // 超时 = 用户在空闲等待，趁机推进欢迎动画。
                self.tick_idle_welcome()?;
                continue;
            }
            let event = read()?;
            let paste_newline = self.windows_input_burst.observe(&event, Instant::now());
            if self.handle_navigation_event(&event)? {
                self.request_draw_now()?;
                continue;
            }
            if paste_newline {
                self.app
                    .bottom_pane_mut()
                    .handle_composer_key(paste_newline_key());
                self.request_draw_now()?;
                continue;
            }
            match event {
                Event::Key(key) if is_ctrl_d(key) => return Ok(None),
                Event::Key(key) => match self.app.bottom_pane_mut().handle_composer_key(key) {
                    ComposerAction::None => {}
                    ComposerAction::Cancel => return Ok(None),
                    ComposerAction::Submit(value) => {
                        if should_render_submitted_user_prompt(&value) {
                            self.app.push_user(value.clone());
                        }
                        self.request_draw_now()?;
                        return Ok(Some(value));
                    }
                },
                Event::Paste(value) => {
                    self.app.bottom_pane_mut().paste(&value);
                }
                _ => {}
            }
            self.request_draw_now()?;
        }
    }

    /// 空闲等待输入时推进一帧欢迎动画，动画还想继续时立刻重绘。
    ///
    /// 重绘走的是回合内 `tick()` 的同一条 `RedrawReason::StatusChanged` 通道
    /// （合并到调度器的最小帧间隔），动画播完 `tick_welcome()` 返回 `false`，
    /// 就不会再产生任何重绘请求。
    fn tick_idle_welcome(&mut self) -> Result<()> {
        if self.app.tick_welcome() {
            self.frame.request(RedrawReason::StatusChanged);
            self.flush_frame(Instant::now())?;
        }
        Ok(())
    }

    pub fn request_approval(&mut self, request: ApprovalRequestView) -> Result<ApprovalDecision> {
        self.app.start_approval(request);
        self.windows_input_burst.reset();
        self.request_draw_now()?;
        loop {
            let event = read()?;
            if self.handle_navigation_event(&event)? {
                self.request_draw_now()?;
                continue;
            }
            match event {
                Event::Key(key) => match self.app.bottom_pane_mut().handle_approval_key(key) {
                    ApprovalAction::None => {}
                    ApprovalAction::Decide(decision) => {
                        self.app.start_prompt("yunxi> ");
                        self.request_draw_now()?;
                        return Ok(decision);
                    }
                },
                _ => {}
            }
            self.request_draw_now()?;
        }
    }

    pub fn request_user_input(
        &mut self,
        request: UserInputRequestView,
    ) -> Result<UserInputResponse> {
        self.app.start_user_input(request);
        self.windows_input_burst.reset();
        self.request_draw_now()?;
        loop {
            let event = read()?;
            let paste_newline = self.windows_input_burst.observe(&event, Instant::now());
            if self.handle_navigation_event(&event)? {
                self.request_draw_now()?;
                continue;
            }
            if paste_newline {
                self.app
                    .bottom_pane_mut()
                    .handle_user_input_key(paste_newline_key());
                self.request_draw_now()?;
                continue;
            }
            match event {
                Event::Key(key) => match self.app.bottom_pane_mut().handle_user_input_key(key) {
                    UserInputAction::None => {}
                    UserInputAction::Cancel => {
                        self.app.start_prompt("yunxi> ");
                        self.request_draw_now()?;
                        return Ok(UserInputResponse { value: None });
                    }
                    UserInputAction::Submit(response) => {
                        self.app.start_prompt("yunxi> ");
                        self.request_draw_now()?;
                        return Ok(response);
                    }
                },
                Event::Paste(value) => {
                    self.app.bottom_pane_mut().paste(&value);
                }
                _ => {}
            }
            self.request_draw_now()?;
        }
    }

    fn request_draw_now(&mut self) -> Result<()> {
        self.request_redraw(RedrawReason::InputChanged)
    }

    fn request_redraw(&mut self, reason: RedrawReason) -> Result<()> {
        let priority = self.frame.request(reason);
        if priority == RedrawPriority::Immediate {
            self.flush_frame(Instant::now())?;
        }
        Ok(())
    }

    fn flush_frame(&mut self, now: Instant) -> Result<()> {
        if self.frame.should_draw(now) {
            self.draw(now)?;
        }
        Ok(())
    }

    fn draw(&mut self, now: Instant) -> Result<()> {
        self.terminal
            .draw(|frame| render_tui_frame(frame, &self.app))
            .map(|_| ())?;
        self.frame.record_draw(now);
        Ok(())
    }

    fn drain_turn_events(&mut self) -> Result<TuiTickAction> {
        let mut redraw_reason = None;
        let mut action = TuiTickAction::None;
        let mut poll_timeout = Duration::ZERO;
        while poll(poll_timeout)? {
            poll_timeout = if cfg!(windows) {
                Duration::from_millis(5)
            } else {
                Duration::ZERO
            };
            let event = read()?;
            let paste_newline = self.windows_input_burst.observe(&event, Instant::now());
            if is_ctrl_c_event(&event) {
                action = TuiTickAction::CancelCurrentTurn;
                redraw_reason = Some(RedrawReason::CancelCurrentTurn);
                continue;
            }
            if self.handle_navigation_event(&event)? {
                redraw_reason = Some(if matches!(event, Event::Resize(_, _)) {
                    RedrawReason::Resize
                } else {
                    RedrawReason::ScrollChanged
                });
                continue;
            }
            if paste_newline {
                if self
                    .app
                    .bottom_pane_mut()
                    .handle_composer_draft_key(paste_newline_key())
                {
                    redraw_reason = Some(RedrawReason::InputChanged);
                }
                continue;
            }
            if apply_turn_draft_event(&mut self.app, &event) {
                redraw_reason = Some(RedrawReason::InputChanged);
            }
        }
        if let Some(reason) = redraw_reason {
            self.frame.request(reason);
        }
        Ok(action)
    }

    fn handle_navigation_event(&mut self, event: &Event) -> Result<bool> {
        let metrics = self.transcript_metrics()?;
        Ok(handle_navigation_event_with_metrics(
            &mut self.app,
            &mut self.scroll_drag,
            event,
            &metrics,
        ))
    }

    fn transcript_metrics(&self) -> Result<TranscriptMetrics> {
        let size = self.terminal.size()?;
        Ok(transcript_metrics_for_size(
            size,
            self.app.bottom_pane().desired_height(),
            &self.app,
        ))
    }
}

/// 空闲等待输入时该不该带超时轮询。
///
/// `Some(节拍)` = 欢迎动画还需要供帧，`read_prompt()` 用 `poll(节拍)` 等输入，
/// 超时就推进一帧；`None` = 没有动画要播，`read_prompt()` 直接阻塞 `read()`，
/// 保持"等到用户输入才返回、期间零唤醒"的原始语义。
fn idle_animation_poll_timeout(app: &YunxiTuiApp) -> Option<Duration> {
    app.welcome_animation_pending()
        .then_some(IDLE_ANIMATION_TICK)
}

fn should_render_submitted_user_prompt(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && !value.starts_with('/')
}

fn is_realtime_voice_submit_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            kind: KeyEventKind::Press,
            ..
        })
    )
}

fn is_realtime_voice_stop_draft(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "q" | "quit" | "exit" | "/voice off" | "/voice realtime off"
    )
}

fn handle_navigation_event_with_metrics(
    app: &mut YunxiTuiApp,
    scroll_drag: &mut Option<TranscriptScrollDrag>,
    event: &Event,
    metrics: &TranscriptMetrics,
) -> bool {
    match event {
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => handle_mouse_scroll_by_focus(
                app,
                resolve_event(app.focus_target(), event),
                metrics,
                mouse.column,
                mouse.row,
            ),
            MouseEventKind::Down(MouseButton::Left) => {
                handle_scrollbar_down(app, scroll_drag, metrics, mouse.column, mouse.row)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                handle_scrollbar_drag(app, scroll_drag, metrics, mouse.row)
            }
            MouseEventKind::Up(MouseButton::Left) => scroll_drag.take().is_some(),
            _ => false,
        },
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            match resolve_key(app.focus_target(), *key) {
                TuiAction::FocusNext => {
                    app.focus_next();
                    true
                }
                TuiAction::FocusPrevious => {
                    app.focus_previous();
                    true
                }
                TuiAction::CloseDetails => {
                    app.close_details();
                    true
                }
                TuiAction::PageUp => {
                    if app.focus_target() == FocusTarget::Details {
                        app.detail_scroll_up(metrics.visible_height as u16);
                    } else {
                        app.page_up(&metrics.wrapped, metrics.visible_height);
                    }
                    true
                }
                TuiAction::PageDown => {
                    if app.focus_target() == FocusTarget::Details {
                        app.detail_scroll_down(metrics.visible_height as u16);
                    } else {
                        app.page_down(&metrics.wrapped, metrics.visible_height);
                    }
                    true
                }
                _ if matches!(key.code, KeyCode::Home)
                    && !app.bottom_pane().text_input_active()
                    && app.focus_target() != FocusTarget::Details =>
                {
                    app.jump_top(&metrics.wrapped, metrics.visible_height);
                    true
                }
                _ if matches!(key.code, KeyCode::End)
                    && !app.bottom_pane().text_input_active()
                    && app.focus_target() != FocusTarget::Details =>
                {
                    app.follow_tail();
                    true
                }
                _ if matches!(
                    app.focus_target(),
                    FocusTarget::History | FocusTarget::Details
                ) =>
                {
                    true
                }
                _ => false,
            }
        }
        Event::Resize(_, _) => {
            *scroll_drag = None;
            app.reanchor_viewport(&metrics.wrapped, metrics.visible_height);
            true
        }
        _ => false,
    }
}

fn handle_mouse_scroll_by_focus(
    app: &mut YunxiTuiApp,
    action: TuiAction,
    metrics: &TranscriptMetrics,
    x: u16,
    y: u16,
) -> bool {
    if !rect_contains(metrics.layout.transcript, x, y) {
        return false;
    }
    match action {
        TuiAction::ScrollUp => {
            app.scroll_up(3, &metrics.wrapped, metrics.visible_height);
            true
        }
        TuiAction::ScrollDown => {
            app.scroll_down(3, &metrics.wrapped, metrics.visible_height);
            true
        }
        TuiAction::DetailScrollUp => {
            app.detail_scroll_up(3);
            true
        }
        TuiAction::DetailScrollDown => {
            app.detail_scroll_down(3);
            true
        }
        TuiAction::None if app.focus_target() == FocusTarget::Approval => true,
        _ => false,
    }
}

fn allows_transcript_scrollbar(focus: FocusTarget) -> bool {
    matches!(focus, FocusTarget::Composer | FocusTarget::History)
}

fn handle_scrollbar_down(
    app: &mut YunxiTuiApp,
    scroll_drag: &mut Option<TranscriptScrollDrag>,
    metrics: &TranscriptMetrics,
    x: u16,
    y: u16,
) -> bool {
    let Some(scrollbar) = metrics.scrollbar else {
        return false;
    };
    let hit = scrollbar.hit_test(x, y);
    if !allows_transcript_scrollbar(app.focus_target()) {
        *scroll_drag = None;
        return hit != ScrollbarHit::Outside;
    }
    match hit {
        ScrollbarHit::Thumb { grab_offset } => {
            *scroll_drag = Some(TranscriptScrollDrag { grab_offset });
            true
        }
        ScrollbarHit::PageUp => {
            app.page_up(&metrics.wrapped, metrics.visible_height);
            true
        }
        ScrollbarHit::PageDown => {
            app.page_down(&metrics.wrapped, metrics.visible_height);
            true
        }
        ScrollbarHit::Outside => false,
    }
}

fn handle_scrollbar_drag(
    app: &mut YunxiTuiApp,
    scroll_drag: &mut Option<TranscriptScrollDrag>,
    metrics: &TranscriptMetrics,
    y: u16,
) -> bool {
    if !allows_transcript_scrollbar(app.focus_target()) {
        *scroll_drag = None;
        return false;
    }
    let Some(drag) = *scroll_drag else {
        return false;
    };
    let Some(scrollbar) = metrics.scrollbar else {
        *scroll_drag = None;
        return false;
    };
    let start = scrollbar.start_for_drag_y(y, drag.grab_offset);
    let max_start = crate::viewport::max_start(metrics.content_height, metrics.visible_height);
    app.set_scroll_fraction(start, max_start, &metrics.wrapped, metrics.visible_height);
    true
}

fn apply_turn_draft_event(app: &mut YunxiTuiApp, event: &Event) -> bool {
    match event {
        Event::Paste(value) => app.bottom_pane_mut().paste(value),
        Event::Key(key) => app.bottom_pane_mut().handle_composer_draft_key(*key),
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TranscriptScrollDrag {
    grab_offset: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TranscriptMetrics {
    layout: crate::layout::TuiLayout,
    visible_height: usize,
    content_height: usize,
    start: usize,
    scrollbar: Option<TranscriptScrollbarGeometry>,
    wrapped: WrappedTranscript,
}

fn transcript_metrics_for_size(
    size: Size,
    bottom_pane_height: u16,
    app: &YunxiTuiApp,
) -> TranscriptMetrics {
    let layout = compute_layout(Rect::new(0, 0, size.width, size.height), bottom_pane_height);
    let visible_height = layout.transcript_inner.height.max(1) as usize;
    let wrapped = build_wrapped_transcript(
        app.transcript().cells(),
        layout.transcript_inner.width as usize,
    );
    let content_height = wrapped.rows.len();
    let start = app.viewport().view_start(&wrapped, visible_height);
    let scrollbar = TranscriptScrollbarGeometry::new(
        layout.transcript_scrollbar,
        content_height,
        visible_height,
        start,
    );

    TranscriptMetrics {
        layout,
        visible_height,
        content_height,
        start,
        scrollbar,
        wrapped,
    }
}

fn redraw_reason_for_agent_event(event: &AgentEvent) -> RedrawReason {
    match event {
        AgentEvent::Message {
            stream: Some(stream),
            ..
        } => match stream.phase {
            AgentMessageStreamPhase::Started | AgentMessageStreamPhase::Delta => {
                RedrawReason::StreamDelta
            }
            AgentMessageStreamPhase::Final => RedrawReason::StreamFinalized,
        },
        AgentEvent::Completed { .. } | AgentEvent::Cancelled { .. } => {
            RedrawReason::StreamFinalized
        }
        AgentEvent::ProviderError { .. } | AgentEvent::Error { .. } => RedrawReason::Error,
        _ => RedrawReason::StatusChanged,
    }
}

fn redraw_reason_for_tui_event(event: &TuiEvent) -> RedrawReason {
    if event.kind == crate::presentation::TuiCellKind::ErrorSummary {
        return RedrawReason::Error;
    }
    match event.stream.as_ref().map(|stream| stream.identity.phase) {
        Some(crate::presentation::TuiStreamPhase::Started)
        | Some(crate::presentation::TuiStreamPhase::Delta)
        | Some(crate::presentation::TuiStreamPhase::Retry) => RedrawReason::StreamDelta,
        Some(crate::presentation::TuiStreamPhase::Final)
        | Some(crate::presentation::TuiStreamPhase::Finish)
        | Some(crate::presentation::TuiStreamPhase::Cancel) => RedrawReason::StreamFinalized,
        None => RedrawReason::StatusChanged,
    }
}

fn is_ctrl_d(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.code == KeyCode::Char('d')
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_ctrl_c_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
    )
}

fn paste_newline_key() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
}

const WINDOWS_PASTE_MAX_EVENT_GAP: Duration = Duration::from_millis(25);
const WINDOWS_PASTE_MIN_TEXT_EVENTS: usize = 4;

#[derive(Default)]
struct WindowsInputBurst {
    recent_text_events: usize,
    last_text_at: Option<Instant>,
}

impl WindowsInputBurst {
    fn observe(&mut self, event: &Event, now: Instant) -> bool {
        if !cfg!(windows) {
            return false;
        }
        match event {
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    && matches!(key.code, KeyCode::Char(_))
                    && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let continues_burst = self.last_text_at.is_some_and(|last| {
                    now.saturating_duration_since(last) <= WINDOWS_PASTE_MAX_EVENT_GAP
                });
                self.recent_text_events = if continues_burst {
                    self.recent_text_events.saturating_add(1)
                } else {
                    1
                };
                self.last_text_at = Some(now);
                false
            }
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    && key.code == KeyCode::Enter
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                let is_paste_newline = self.recent_text_events >= WINDOWS_PASTE_MIN_TEXT_EVENTS
                    && self.last_text_at.is_some_and(|last| {
                        now.saturating_duration_since(last) <= WINDOWS_PASTE_MAX_EVENT_GAP
                    });
                if is_paste_newline {
                    self.last_text_at = Some(now);
                } else {
                    self.reset();
                }
                is_paste_newline
            }
            Event::Key(key) if key.kind != KeyEventKind::Press => false,
            Event::Paste(_) | Event::Key(_) => {
                self.reset();
                false
            }
            _ => false,
        }
    }

    fn reset(&mut self) {
        self.recent_text_events = 0;
        self.last_text_at = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalLifecycleAction {
    EnableRawMode,
    EnterAlternateScreen,
    EnableBracketedPaste,
    EnableFocusChange,
    EnableMouseCapture,
    HideCursor,
    ShowCursor,
    DisableMouseCapture,
    DisableFocusChange,
    DisableBracketedPaste,
    LeaveAlternateScreen,
    DisableRawMode,
    ResetStyle,
}

impl TerminalLifecycleAction {
    fn recovery(self) -> Self {
        match self {
            Self::EnableRawMode => Self::DisableRawMode,
            Self::EnterAlternateScreen => Self::LeaveAlternateScreen,
            Self::EnableBracketedPaste => Self::DisableBracketedPaste,
            Self::EnableFocusChange => Self::DisableFocusChange,
            Self::EnableMouseCapture => Self::DisableMouseCapture,
            Self::HideCursor => Self::ShowCursor,
            recovery => recovery,
        }
    }
}

const TERMINAL_ENTER_ACTIONS: [TerminalLifecycleAction; 6] = [
    TerminalLifecycleAction::EnableRawMode,
    TerminalLifecycleAction::EnterAlternateScreen,
    TerminalLifecycleAction::EnableBracketedPaste,
    TerminalLifecycleAction::EnableFocusChange,
    TerminalLifecycleAction::EnableMouseCapture,
    TerminalLifecycleAction::HideCursor,
];

trait TerminalLifecycleSink {
    fn apply(&mut self, action: TerminalLifecycleAction) -> Result<()>;
}

struct CrosstermLifecycleSink;

impl TerminalLifecycleSink for CrosstermLifecycleSink {
    fn apply(&mut self, action: TerminalLifecycleAction) -> Result<()> {
        match action {
            TerminalLifecycleAction::EnableRawMode => enable_raw_mode()?,
            TerminalLifecycleAction::EnterAlternateScreen => {
                execute!(io::stdout(), EnterAlternateScreen)?;
            }
            TerminalLifecycleAction::EnableBracketedPaste => {
                execute!(io::stdout(), EnableBracketedPaste)?;
            }
            TerminalLifecycleAction::EnableFocusChange => {
                execute!(io::stdout(), EnableFocusChange)?;
            }
            TerminalLifecycleAction::EnableMouseCapture => {
                execute!(io::stdout(), EnableMouseCapture)?;
            }
            TerminalLifecycleAction::HideCursor => {
                execute!(io::stdout(), Hide)?;
            }
            TerminalLifecycleAction::ShowCursor => {
                execute!(io::stdout(), Show)?;
            }
            TerminalLifecycleAction::DisableMouseCapture => {
                execute!(io::stdout(), DisableMouseCapture)?;
            }
            TerminalLifecycleAction::DisableFocusChange => {
                execute!(io::stdout(), DisableFocusChange)?;
            }
            TerminalLifecycleAction::DisableBracketedPaste => {
                execute!(io::stdout(), DisableBracketedPaste)?;
            }
            TerminalLifecycleAction::LeaveAlternateScreen => {
                execute!(io::stdout(), LeaveAlternateScreen)?;
            }
            TerminalLifecycleAction::DisableRawMode => disable_raw_mode()?,
            TerminalLifecycleAction::ResetStyle => {
                let mut stdout = io::stdout();
                stdout.write_all(b"\x1b[0m")?;
                stdout.flush()?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct TerminalLifecycleState {
    entered: Vec<TerminalLifecycleAction>,
}

impl TerminalLifecycleState {
    fn enter(sink: &mut impl TerminalLifecycleSink) -> Result<Self> {
        let mut state = Self::default();
        for action in TERMINAL_ENTER_ACTIONS {
            if let Err(error) = sink.apply(action) {
                state.restore(sink);
                return Err(error);
            }
            state.entered.push(action);
        }
        Ok(state)
    }

    fn restore(&mut self, sink: &mut impl TerminalLifecycleSink) {
        let _ = sink.apply(TerminalLifecycleAction::ResetStyle);
        while let Some(action) = self.entered.pop() {
            let _ = sink.apply(action.recovery());
        }
    }
}

struct TerminalGuard {
    state: TerminalLifecycleState,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        let mut sink = CrosstermLifecycleSink;
        let state = TerminalLifecycleState::enter(&mut sink)?;
        Ok(Self { state })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.state.restore(&mut CrosstermLifecycleSink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bottom_pane::BottomPaneMode;
    use crossterm::Command;
    use crossterm::style::ResetColor;
    use crossterm::terminal::SetSize;

    #[test]
    fn idle_prompt_polls_only_while_the_welcome_animation_is_on_screen() {
        // issue #38：欢迎界面属于空闲态，`read_prompt` 却只会阻塞读，
        // 没有任何人推进动画。这里钉住宿主侧的契约：
        // 动画在播 -> 带超时轮询（能推进动画）；其余情况 -> 纯阻塞读。
        let mut app = YunxiTuiApp::default();
        assert_eq!(idle_animation_poll_timeout(&app), Some(IDLE_ANIMATION_TICK));

        while app.tick_welcome() {}
        assert_eq!(idle_animation_poll_timeout(&app), None);

        let mut app = YunxiTuiApp::default();
        app.push_user("你好");
        assert_eq!(idle_animation_poll_timeout(&app), None);
    }

    #[test]
    fn slash_commands_are_not_rendered_as_user_messages() {
        assert!(!should_render_submitted_user_prompt("/voice"));
        assert!(!should_render_submitted_user_prompt("  /status  "));
        assert!(!should_render_submitted_user_prompt(""));
        assert!(should_render_submitted_user_prompt("和云熙聊一会儿"));
    }

    #[test]
    fn realtime_voice_stop_draft_accepts_only_explicit_commands() {
        for value in ["q", " Q ", "quit", "/voice off", "/voice realtime off"] {
            assert!(
                is_realtime_voice_stop_draft(value),
                "expected stop: {value}"
            );
        }
        for value in ["", "question", "/voice realtime on", "关闭实时语音"] {
            assert!(
                !is_realtime_voice_stop_draft(value),
                "unexpected stop: {value}"
            );
        }
    }

    #[derive(Default)]
    struct RecordingLifecycleSink {
        actions: Vec<TerminalLifecycleAction>,
        fail_on: Option<TerminalLifecycleAction>,
    }

    impl TerminalLifecycleSink for RecordingLifecycleSink {
        fn apply(&mut self, action: TerminalLifecycleAction) -> Result<()> {
            self.actions.push(action);
            if self.fail_on == Some(action) {
                anyhow::bail!("injected terminal lifecycle failure");
            }
            Ok(())
        }
    }

    #[test]
    fn terminal_lifecycle_restores_every_state_in_reverse_order() {
        let mut sink = RecordingLifecycleSink::default();
        let mut state = TerminalLifecycleState::enter(&mut sink).expect("terminal enter");
        state.restore(&mut sink);

        assert_eq!(
            sink.actions,
            [
                TERMINAL_ENTER_ACTIONS.as_slice(),
                &[
                    TerminalLifecycleAction::ResetStyle,
                    TerminalLifecycleAction::ShowCursor,
                    TerminalLifecycleAction::DisableMouseCapture,
                    TerminalLifecycleAction::DisableFocusChange,
                    TerminalLifecycleAction::DisableBracketedPaste,
                    TerminalLifecycleAction::LeaveAlternateScreen,
                    TerminalLifecycleAction::DisableRawMode,
                ],
            ]
            .concat()
        );
        assert!(state.entered.is_empty());
    }

    #[test]
    fn partial_terminal_enter_rolls_back_only_completed_actions() {
        let mut sink = RecordingLifecycleSink {
            fail_on: Some(TerminalLifecycleAction::EnableFocusChange),
            ..RecordingLifecycleSink::default()
        };

        assert!(TerminalLifecycleState::enter(&mut sink).is_err());
        assert_eq!(
            sink.actions,
            vec![
                TerminalLifecycleAction::EnableRawMode,
                TerminalLifecycleAction::EnterAlternateScreen,
                TerminalLifecycleAction::EnableBracketedPaste,
                TerminalLifecycleAction::EnableFocusChange,
                TerminalLifecycleAction::ResetStyle,
                TerminalLifecycleAction::DisableBracketedPaste,
                TerminalLifecycleAction::LeaveAlternateScreen,
                TerminalLifecycleAction::DisableRawMode,
            ]
        );
    }

    fn push_vt_command(command: impl Command, transcript: &mut String) {
        command.write_ansi(transcript).expect("write VT100 command");
    }

    fn lifecycle_vt100_transcript() -> String {
        let mut transcript = String::new();
        push_vt_command(EnterAlternateScreen, &mut transcript);
        push_vt_command(EnableBracketedPaste, &mut transcript);
        push_vt_command(EnableFocusChange, &mut transcript);
        push_vt_command(EnableMouseCapture, &mut transcript);
        push_vt_command(Hide, &mut transcript);
        push_vt_command(SetSize(58, 18), &mut transcript);
        push_vt_command(ResetColor, &mut transcript);
        push_vt_command(Show, &mut transcript);
        push_vt_command(DisableMouseCapture, &mut transcript);
        push_vt_command(DisableFocusChange, &mut transcript);
        push_vt_command(DisableBracketedPaste, &mut transcript);
        push_vt_command(LeaveAlternateScreen, &mut transcript);
        transcript
    }

    fn visible_vt100(transcript: &str) -> String {
        transcript
            .replace('\u{1b}', "<ESC>")
            .replace('\r', "<CR>")
            .replace('\n', "<LF>")
    }

    #[test]
    fn vt100_exit_scenarios_reset_resize_and_restore_terminal_state() {
        let transcript = lifecycle_vt100_transcript();
        for token in [
            "\u{1b}[?1049h",
            "\u{1b}[?2004h",
            "\u{1b}[?1004h",
            "\u{1b}[?25l",
            "\u{1b}[8;18;58t",
            "\u{1b}[0m",
            "\u{1b}[?25h",
            "\u{1b}[?1004l",
            "\u{1b}[?2004l",
            "\u{1b}[?1049l",
        ] {
            assert!(transcript.contains(token), "missing VT100 token {token:?}");
        }
        let reset = transcript.find("\u{1b}[0m").expect("ANSI reset");
        let cursor = transcript.find("\u{1b}[?25h").expect("cursor restore");
        let leave = transcript.find("\u{1b}[?1049l").expect("alternate leave");
        assert!(reset < cursor && cursor < leave);

        let visible = visible_vt100(&transcript);
        let snapshot = [
            "normal-exit",
            "ctrl-c-exit",
            "tool-failure-exit",
            "provider-error-exit",
        ]
        .map(|scenario| format!("{scenario}|{visible}"))
        .join("\n");
        if std::env::var_os("YUNXI_UPDATE_SNAPSHOTS").is_some() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("snapshots")
                .join("vt100_lifecycle_v210.txt");
            std::fs::write(path, format!("{snapshot}\n")).expect("write VT100 golden");
            return;
        }
        assert_eq!(
            snapshot,
            include_str!("snapshots/vt100_lifecycle_v210.txt").trim_end_matches(['\r', '\n'])
        );
    }

    #[test]
    fn transcript_visible_height_matches_tui_layout() {
        let app = YunxiTuiApp::default();
        assert_eq!(
            transcript_metrics_for_size(
                Size {
                    width: 100,
                    height: 18,
                },
                3,
                &app,
            )
            .visible_height,
            10
        );
        assert_eq!(
            transcript_metrics_for_size(
                Size {
                    width: 100,
                    height: 6,
                },
                3,
                &app,
            )
            .visible_height,
            1
        );
    }

    #[test]
    fn raw_mode_ctrl_c_is_classified_as_turn_cancellation() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(is_ctrl_c_event(&event));
        assert!(!is_ctrl_c_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE,
        ))));
    }

    #[test]
    fn windows_conpty_text_burst_reclassifies_only_rapid_embedded_enter() {
        let mut burst = WindowsInputBurst::default();
        let started = Instant::now();
        for (index, ch) in ['p', 'a', 's', 't', 'e'].into_iter().enumerate() {
            assert!(!burst.observe(
                &Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
                started + Duration::from_millis(index as u64),
            ));
            assert!(!burst.observe(
                &Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Char(ch),
                    KeyModifiers::NONE,
                    KeyEventKind::Release,
                )),
                started + Duration::from_millis(index as u64),
            ));
        }
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert_eq!(
            burst.observe(&enter, started + Duration::from_millis(6)),
            cfg!(windows)
        );

        burst.reset();
        for (index, ch) in ['t', 'y', 'p', 'e'].into_iter().enumerate() {
            burst.observe(
                &Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
                started + Duration::from_millis(index as u64 * 40),
            );
        }
        assert!(!burst.observe(&enter, started + Duration::from_millis(200)));
    }

    #[test]
    fn active_turn_routes_committed_text_and_paste_to_the_composer_draft() {
        let mut app = YunxiTuiApp::default();
        assert!(apply_turn_draft_event(
            &mut app,
            &Event::Key(KeyEvent::new(KeyCode::Char('输'), KeyModifiers::NONE,)),
        ));
        assert!(apply_turn_draft_event(
            &mut app,
            &Event::Paste("入法\r\n第二行👩‍💻".to_string()),
        ));
        assert!(!apply_turn_draft_event(
            &mut app,
            &Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ));

        assert_eq!(
            app.bottom_pane().composer_buffer().text(),
            "输入法\n第二行👩‍💻"
        );
    }

    #[test]
    fn active_views_take_input_without_overwriting_the_composer_draft() {
        let mut app = YunxiTuiApp::default();
        app.bottom_pane_mut().paste("composer draft");
        app.start_approval(ApprovalRequestView {
            id: None,
            tool_name: "shell".to_string(),
            cwd: ".".to_string(),
            command: None,
            reason: "test".to_string(),
            risk_label: None,
        });
        assert!(!apply_turn_draft_event(
            &mut app,
            &Event::Paste("ignored".to_string()),
        ));
        app.start_prompt("yunxi> ");
        assert_eq!(app.bottom_pane().composer_buffer().text(), "composer draft");

        app.start_user_input(UserInputRequestView {
            id: None,
            prompt: "answer".to_string(),
        });
        assert!(apply_turn_draft_event(
            &mut app,
            &Event::Paste("overlay answer".to_string()),
        ));
        let BottomPaneMode::UserInput { buffer, .. } = app.bottom_pane().mode() else {
            panic!("user input mode");
        };
        assert_eq!(buffer.text(), "overlay answer");
        app.start_prompt("yunxi> ");
        assert_eq!(app.bottom_pane().composer_buffer().text(), "composer draft");
    }

    #[test]
    fn transcript_metrics_uses_wrapped_content_height() {
        let mut app = YunxiTuiApp::default();
        app.push_agent_event(&yunxi_agent_core::AgentEvent::Message {
            content: "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz".to_string(),
            stream: None,
        });

        let metrics = transcript_metrics_for_size(
            Size {
                width: 24,
                height: 18,
            },
            3,
            &app,
        );

        assert!(metrics.content_height > app.transcript().render_line_count());
        assert!(metrics.scrollbar.is_none() || metrics.content_height > metrics.visible_height);
    }

    #[test]
    fn approval_mouse_wheel_click_drag_and_release_freeze_transcript() {
        let mut app = populated_app();
        app.bottom_pane_mut().paste("approval draft 中文👩‍💻");
        app.start_approval(approval_request());
        let approval = app.bottom_pane().mode().clone();
        let metrics = metrics_for(&app);
        let scrollbar = metrics.scrollbar.expect("scrollable transcript");
        let before = app.viewport().clone();
        let mut drag = None;

        assert!(handle_navigation_event_with_metrics(
            &mut app,
            &mut drag,
            &mouse(
                MouseEventKind::ScrollUp,
                metrics.layout.transcript.x + 1,
                metrics.layout.transcript.y + 1,
            ),
            &metrics,
        ));
        assert!(handle_navigation_event_with_metrics(
            &mut app,
            &mut drag,
            &mouse(
                MouseEventKind::Down(MouseButton::Left),
                scrollbar.track.x,
                scrollbar.track.y,
            ),
            &metrics,
        ));
        drag = Some(TranscriptScrollDrag { grab_offset: 0 });
        assert!(!handle_navigation_event_with_metrics(
            &mut app,
            &mut drag,
            &mouse(
                MouseEventKind::Drag(MouseButton::Left),
                scrollbar.track.x,
                scrollbar.track.y + scrollbar.track.height - 1,
            ),
            &metrics,
        ));
        assert!(!handle_navigation_event_with_metrics(
            &mut app,
            &mut drag,
            &mouse(
                MouseEventKind::Up(MouseButton::Left),
                scrollbar.track.x,
                scrollbar.track.y + scrollbar.track.height - 1,
            ),
            &metrics,
        ));

        assert_eq!(app.viewport(), &before);
        assert_eq!(app.bottom_pane().mode(), &approval);
        assert!(drag.is_none());
    }

    #[test]
    fn details_wheel_and_page_keys_only_change_details_scroll() {
        let mut app = populated_app();
        app.bottom_pane_mut().paste("details draft 中文👩‍💻");
        let draft = app.bottom_pane().composer_snapshot();
        let metrics = metrics_for(&app);
        app.scroll_up(6, &metrics.wrapped, metrics.visible_height);
        let viewport = app.viewport().clone();
        app.show_details(None);
        let mut drag = None;

        assert!(handle_navigation_event_with_metrics(
            &mut app,
            &mut drag,
            &mouse(
                MouseEventKind::ScrollDown,
                metrics.layout.transcript.x + 1,
                metrics.layout.transcript.y + 1,
            ),
            &metrics,
        ));
        assert_eq!(app.details_scroll(), 3);
        assert!(handle_navigation_event_with_metrics(
            &mut app,
            &mut drag,
            &Event::Key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            &metrics,
        ));
        assert_eq!(
            app.details_scroll(),
            3u16.saturating_add(metrics.visible_height as u16)
        );
        assert_eq!(app.viewport(), &viewport);

        let scrollbar = metrics.scrollbar.expect("scrollable transcript");
        assert!(handle_navigation_event_with_metrics(
            &mut app,
            &mut drag,
            &mouse(
                MouseEventKind::Down(MouseButton::Left),
                scrollbar.track.x,
                scrollbar.track.y,
            ),
            &metrics,
        ));
        assert!(drag.is_none());
        assert_eq!(app.viewport(), &viewport);

        assert!(handle_navigation_event_with_metrics(
            &mut app,
            &mut drag,
            &Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &metrics,
        ));
        assert_eq!(app.focus_target(), FocusTarget::Composer);
        assert_eq!(app.viewport(), &viewport);
        assert_eq!(app.bottom_pane().composer_snapshot(), draft);
    }

    #[test]
    fn composer_and_history_keep_transcript_mouse_scrolling() {
        let mut app = populated_app();
        let metrics = metrics_for(&app);
        let tail = app.viewport().clone();
        let mut drag = None;

        assert!(handle_navigation_event_with_metrics(
            &mut app,
            &mut drag,
            &mouse(
                MouseEventKind::ScrollUp,
                metrics.layout.transcript.x + 1,
                metrics.layout.transcript.y + 1,
            ),
            &metrics,
        ));
        assert_ne!(app.viewport(), &tail);

        app.focus_next();
        let history_before = app.viewport().clone();
        assert!(handle_navigation_event_with_metrics(
            &mut app,
            &mut drag,
            &mouse(
                MouseEventKind::ScrollUp,
                metrics.layout.transcript.x + 1,
                metrics.layout.transcript.y + 1,
            ),
            &metrics,
        ));
        assert_ne!(app.viewport(), &history_before);
        assert!(allows_transcript_scrollbar(FocusTarget::Composer));
        assert!(allows_transcript_scrollbar(FocusTarget::History));
        assert!(!allows_transcript_scrollbar(FocusTarget::Approval));
        assert!(!allows_transcript_scrollbar(FocusTarget::Details));
    }

    fn populated_app() -> YunxiTuiApp {
        let mut app = YunxiTuiApp::default();
        for index in 0..48 {
            app.push_notice(
                "history",
                &format!("history row {index:02} with enough content for mouse scrolling"),
            );
        }
        app
    }

    fn metrics_for(app: &YunxiTuiApp) -> TranscriptMetrics {
        transcript_metrics_for_size(
            Size {
                width: 100,
                height: 30,
            },
            app.bottom_pane().desired_height(),
            app,
        )
    }

    fn approval_request() -> ApprovalRequestView {
        ApprovalRequestView {
            id: Some("approval-focus-test".to_string()),
            tool_name: "shell".to_string(),
            cwd: ".".to_string(),
            command: Some("echo safe".to_string()),
            reason: "focus routing test".to_string(),
            risk_label: None,
        }
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }
}
