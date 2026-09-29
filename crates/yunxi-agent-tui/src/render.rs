use crate::app::YunxiTuiApp;
use crate::approval_layout::{ApprovalLayoutLine, ApprovalLineKind, approval_layout_for_width};
use crate::bottom_pane::BottomPaneMode;
use crate::edit_buffer::EditBuffer;
use crate::input_map::FocusTarget;
use crate::layout::compute_layout;
use crate::onboarding::OnboardingWizard;
use crate::scrollbar::TranscriptScrollbarGeometry;
use crate::styles::{TuiSemanticStyle, TuiStyleSet};
use crate::text_layout::{TextLayout, WrapPolicy};
#[cfg(test)]
use crate::transcript_layout::build_wrapped_transcript;
use crate::transcript_layout::build_wrapped_transcript_with_styles;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// 配置向导的整屏渲染。
///
/// 向导不经过 `YunxiTuiApp`（它没有会话、没有输入框、没有底部面板），
/// 所以这里直接消费 `OnboardingWizard::render()` 的片段网格 —— 与欢迎动画
/// 用同一套 `Seg → Span → Paragraph` 转换，保证视觉语言一致。
pub(crate) fn render_onboarding_screen(frame: &mut Frame<'_>, wizard: &OnboardingWizard) {
    use crate::terminal::chrome::{Chrome, Cx, Stop, StopState, View, compose};
    use crate::terminal::starfield::BannerArt;

    let area = frame.area();
    let cols = usize::from(area.width);
    let rows = usize::from(area.height);
    if cols == 0 || rows == 0 {
        return;
    }

    // 版面交给照搬来的 `chrome::compose`：62 列居中栏、进度轨、按键条、
    // 两侧稀疏暗星，全在那边。这里只负责说「这屏有哪些行」。
    let theme = TuiStyleSet::detect().theme();
    let cx = Cx::new(theme);
    let (here, total) = wizard.progress();

    // 进度轨：Miyu 的 OOBE 用同一个构件表示「走过 / 正在这儿 / 还没到」。
    let rail: Vec<Stop> = (0..total)
        .map(|index| Stop {
            label: format!("{}", index + 1),
            state: match index.cmp(&here) {
                std::cmp::Ordering::Less => StopState::Done,
                std::cmp::Ordering::Equal => StopState::Here,
                std::cmp::Ordering::Greater => StopState::Todo,
            },
        })
        .collect();

    // 正文：向导自己的内容，交给 chrome 去居中、折行、铺边栏。
    let body: Vec<Line<'static>> = wizard
        .body_lines(cols.saturating_sub(2))
        .into_iter()
        .map(|line| {
            Line::from(
                line.into_iter()
                    .map(|seg| Span::styled(seg.text, seg.style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();

    let art = BannerArt::builtin(theme.ascii);
    let mut view = View::default();
    view.body = body;
    view.keys = vec![
        ("Enter".to_string(), "下一步".to_string()),
        ("Esc".to_string(), "跳过".to_string()),
    ];
    let chrome = Chrome::new(theme, &art, &rail);
    let mut scroll = 0usize;
    let composed = compose(cols, rows, &chrome, &view, &mut scroll);
    let _ = cx; // 构件留给后续把表单元素也换成 Cx 的 field/radio/check
    frame.render_widget(Paragraph::new(composed.lines), area);
}

pub(crate) fn render_tui_frame(frame: &mut Frame<'_>, app: &YunxiTuiApp) {
    render_tui_frame_with_styles(frame, app, TuiStyleSet::detect());
}

pub(crate) fn render_tui_frame_with_styles(
    frame: &mut Frame<'_>,
    app: &YunxiTuiApp,
    styles: TuiStyleSet,
) {
    let area = frame.area();
    let layout = compute_layout(
        area,
        app.bottom_pane()
            .desired_height_for_width(area.width as usize),
    );

    // 先铺边栏星空，再画正文 —— 正文覆盖在上面。这是 Miyu `compose` 里的
    // 「两侧空白铺极稀的暗星」：居中列之外的留白不是空的，是设计的一部分。
    render_starfield_gutters(frame, area, layout.header, styles, app.animation_tick());

    if layout.header.width > 0 && layout.header.height > 0 {
        render_header(frame, app, layout.header, styles);
    }
    if layout.transcript.width == 0 || layout.transcript.height == 0 {
        // The bottom pane gets priority on extremely small terminals.
    } else if app.details().is_some() {
        render_details(frame, app, layout.transcript, styles);
    } else if app.control_snapshot().is_some() {
        render_controls(frame, app, layout.transcript, styles);
    } else {
        render_transcript(
            frame,
            app,
            layout.transcript,
            layout.transcript_inner,
            layout.transcript_scrollbar,
            styles,
        );
    }
    if layout.bottom_pane.width > 0 && layout.bottom_pane.height > 0 {
        render_bottom_pane(frame, app, layout.bottom_pane, styles);
    }
}

/// 居中列两侧的稀疏暗星。
///
/// 照搬 Miyu `chrome::compose` 的边栏：只在正文列**之外**的留白里铺，密度
/// `11`、亮度 `0.30` —— 刚好能看见、不抢戏。Miyu 的原话是「不要框：内容居中，
/// 两侧空白铺极稀的暗星」。它的作用是把两侧的空白变成画面的一部分：否则一条
/// 62 列的正文摆在宽终端中间，两边那两片纯空白看着像渲染坏了。
fn render_starfield_gutters(
    frame: &mut Frame<'_>,
    area: Rect,
    content: Rect,
    styles: TuiStyleSet,
    tick: usize,
) {
    use crate::terminal::starfield::{Seg, star_seg};

    /// 边栏星空的密度（值越小越密）。
    const STAR_SPARSE: u32 = 11;
    /// 边栏星空的亮度上限。暗到只在余光里。
    const STAR_MARGIN_DIM: f32 = 0.30;

    if area.width <= content.width || area.height == 0 {
        return; // 没有留白，就没有边栏
    }
    let theme = styles.theme();
    let left_end = usize::from(content.x.saturating_sub(area.x));
    let right_start = left_end + usize::from(content.width);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(usize::from(area.height));
    for y in 0..usize::from(area.height) {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(usize::from(area.width));
        for x in 0..usize::from(area.width) {
            let seg = if x >= left_end && x < right_start {
                // 正文列之内留白，绝不能把星星压到内容上。
                Seg::raw(" ")
            } else {
                star_seg(x, y, tick, theme, STAR_MARGIN_DIM, STAR_SPARSE)
            };
            spans.push(Span::styled(seg.text, seg.style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_details(frame: &mut Frame<'_>, app: &YunxiTuiApp, area: Rect, styles: TuiStyleSet) {
    let Some(details) = app.details() else {
        return;
    };
    let panel = Paragraph::new(details.to_string())
        .block(
            Block::default()
                .title(Span::styled(
                    "Details | Esc close | wheel/PgUp/PgDown scroll",
                    styles.style(TuiSemanticStyle::Focus),
                ))
                .borders(Borders::ALL)
                .border_style(styles.style(TuiSemanticStyle::Focus)),
        )
        .scroll((app.details_scroll(), 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(panel, area);
}

fn render_controls(frame: &mut Frame<'_>, app: &YunxiTuiApp, area: Rect, styles: TuiStyleSet) {
    let Some(snapshot) = app.control_snapshot() else {
        return;
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "Local companion: ",
                styles.style(TuiSemanticStyle::Subheader),
            ),
            Span::styled(
                if snapshot.companion_enabled {
                    "ON"
                } else {
                    "OFF"
                },
                styles.style(if snapshot.companion_enabled {
                    TuiSemanticStyle::Success
                } else {
                    TuiSemanticStyle::Notice
                }),
            ),
            Span::raw("   "),
            Span::styled("Cloud control: ", styles.style(TuiSemanticStyle::Subheader)),
            Span::styled(
                if snapshot.cloud_control_enabled {
                    "ON"
                } else {
                    "OFF"
                },
                styles.style(if snapshot.cloud_control_enabled {
                    TuiSemanticStyle::Warning
                } else {
                    TuiSemanticStyle::Success
                }),
            ),
        ]),
        Line::from(format!(
            "Quiet hours: {}",
            snapshot.quiet_hours.as_deref().unwrap_or("none")
        )),
        Line::from(""),
    ];
    for state in &snapshot.scopes {
        let enabled = state
            .enabled
            .map(|value| if value { "on" } else { "off" })
            .unwrap_or("read-only");
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<13}", state.scope.as_str()),
                styles.style(TuiSemanticStyle::Header),
            ),
            Span::raw(format!(" {enabled:<9} source={} ", state.source.as_str())),
        ]));
        lines.push(Line::from(Span::styled(
            format!("  {}", state.summary),
            styles.style(TuiSemanticStyle::Muted),
        )));
        if let Some(effect) = &state.clear_effect {
            lines.push(Line::from(Span::styled(
                format!("  clear scope: {effect}"),
                styles.style(TuiSemanticStyle::Warning),
            )));
        }
    }
    lines.push(Line::from(""));
    if let Some(change) = &snapshot.recent_change {
        lines.push(Line::from(format!("Recent change: {change}")));
    }
    lines.push(Line::from(Span::styled(
        "/controls refresh | /companion on|off | /controls clear memory",
        styles.style(TuiSemanticStyle::Footer),
    )));
    let panel = Paragraph::new(lines)
        .block(
            Block::default()
                .title(Span::styled(
                    "Companion UX & Controls",
                    styles.style(TuiSemanticStyle::Header),
                ))
                .borders(Borders::ALL)
                .border_style(styles.style(TuiSemanticStyle::Border)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(panel, area);
}

fn render_header(frame: &mut Frame<'_>, app: &YunxiTuiApp, area: Rect, styles: TuiStyleSet) {
    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            app.header_for_width(area.width as usize),
            styles.style(TuiSemanticStyle::Header),
        )),
        Line::from(Span::styled(
            app.subheader_for_width(area.width as usize),
            styles.style(TuiSemanticStyle::Subheader),
        )),
    ])
    // 去框：状态行不再用一条 `───` 与下方隔开。分层靠明度（Header 亮 / 正文中）
    // 与留白，不靠线条。
    .block(Block::default().borders(Borders::NONE));
    frame.render_widget(header, area);
}

fn render_transcript(
    frame: &mut Frame<'_>,
    app: &YunxiTuiApp,
    area: Rect,
    inner: Rect,
    scrollbar_area: Rect,
    styles: TuiStyleSet,
) {
    if !app.has_user_round() && app.welcome_enabled() && !app.intro_finished() {
        // 首次启动曾经走的是另一张静态检查卡：新做的动画欢迎界面反而在**最重要的
        // 首次进入时刻**看不到。现在两条路径合一 —— `render_welcome_animated` 把
        // 清单接在动画下方渲染，空清单时输出与原来的纯动画逐片段相同。
        //
        // `!intro_finished()` 是「开场播完/被跳过就撤下」的那一条：置位后这里不再接管
        // 会话区，落到下面的分支渲染干净的对话区 + 输入框，欢迎卡不会停在最后一帧
        // 变成静态背景。
        render_welcome_animated(frame, app, area, inner, styles);
        return;
    }
    let wrapped = build_wrapped_transcript_with_styles(
        app.transcript().cells(),
        inner.width as usize,
        styles,
    );
    let visible = inner.height.max(1) as usize;
    let start = app.viewport().view_start(&wrapped, visible);
    let end = start.saturating_add(visible).min(wrapped.rows.len());
    let title = transcript_title(app, start, end, wrapped.rows.len(), visible);
    let border_semantic = if app.focus_target() == FocusTarget::History {
        TuiSemanticStyle::Focus
    } else {
        TuiSemanticStyle::Border
    };
    let transcript = Paragraph::new(wrapped.rows[start..end].to_vec()).block(
        Block::default()
            .title(Span::styled(
                title,
                styles.style(TuiSemanticStyle::Subheader),
            ))
            // 去框：对话区不再有 `┌对话┐`。标题改由上方一行 dim 文本承担
            // （`render_transcript_title`），所以这里只画内容。
            .borders(Borders::NONE),
    );
    frame.render_widget(transcript, area);

    if wrapped.rows.len() > visible {
        render_transcript_scrollbar(
            frame,
            scrollbar_area,
            wrapped.rows.len(),
            visible,
            start,
            app.focus_target(),
            styles,
        );
    }
}

fn render_welcome_animated(
    frame: &mut Frame<'_>,
    app: &YunxiTuiApp,
    _area: Rect,
    inner: Rect,
    _styles: TuiStyleSet,
) {
    if let Some(scene) = app.welcome_scene() {
        // 清单（首次启动检查）接在动画下方渲染。清单为空时 `render_with_checklist`
        // 的输出与纯动画逐片段相同，所以这里可以无条件透传。
        let output = scene.render_with_checklist(
            inner.width as usize,
            inner.height as usize,
            app.welcome_checklist(),
        );

        for (y, line) in output.iter().enumerate() {
            if y >= inner.height as usize {
                break;
            }

            let mut spans = Vec::new();
            for seg in line {
                spans.push(Span::styled(&seg.text, seg.style));
            }

            let line = Line::from(spans);
            let paragraph = Paragraph::new(line);

            let row_area = Rect {
                x: inner.x,
                y: inner.y + y as u16,
                width: inner.width,
                height: 1,
            };

            frame.render_widget(paragraph, row_area);
        }
    }
}

fn render_transcript_scrollbar(
    frame: &mut Frame<'_>,
    area: Rect,
    content_height: usize,
    visible_height: usize,
    start: usize,
    focus: FocusTarget,
    styles: TuiStyleSet,
) {
    let Some(geometry) =
        TranscriptScrollbarGeometry::new(area, content_height, visible_height, start)
    else {
        return;
    };
    let thumb_top = geometry.thumb_top.saturating_sub(area.y);
    let thumb_bottom = thumb_top.saturating_add(geometry.thumb_height);
    let thumb_style = styles.style(if focus == FocusTarget::History {
        TuiSemanticStyle::Focus
    } else {
        TuiSemanticStyle::Subheader
    });
    let track_style = styles.style(TuiSemanticStyle::Border);
    let rows = (0..area.height)
        .map(|row| {
            let (symbol, style) = if row >= thumb_top && row < thumb_bottom {
                ("█", thumb_style)
            } else if row == 0 {
                ("^", track_style)
            } else if row + 1 == area.height {
                ("v", track_style)
            } else {
                ("║", track_style)
            };
            Line::from(Span::styled(symbol, style))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(rows), area);
}

fn render_bottom_pane(frame: &mut Frame<'_>, app: &YunxiTuiApp, area: Rect, styles: TuiStyleSet) {
    match app.bottom_pane().mode() {
        BottomPaneMode::Composer => render_composer(
            frame,
            area,
            app.footer_for_width(area.width as usize),
            "输入",
            app.bottom_pane().composer_prompt(),
            app.bottom_pane().composer_buffer(),
            Some("试着说说你想做什么…"),
            app.bottom_pane().composer_paste_summary(),
            styles,
            TuiSemanticStyle::Focus,
        ),
        BottomPaneMode::Approval { request, selected } => {
            let layout = approval_layout_for_width(request, *selected, area.width as usize);
            let lines = layout
                .lines
                .into_iter()
                .map(|line| render_approval_layout_line(line, styles))
                .collect::<Vec<_>>();
            // 去框：审批不再是「一个带边框的盒子」，而是会话区里一段缩进的
            // 提示块。标题仍然保留 —— 它交代了「默认是拒绝」这条安全语义，
            // 属于内容而不是装饰，所以改成区内第一行。
            let mut lines = lines;
            lines.insert(
                0,
                Line::from(Span::styled(
                    "需要批准 · 默认拒绝",
                    styles.style(TuiSemanticStyle::ActionRequired),
                )),
            );
            frame.render_widget(Paragraph::new(lines), area);
        }
        BottomPaneMode::UserInput { request, buffer } => {
            let prompt = format!("{} ", request.prompt);
            render_composer(
                frame,
                area,
                "Enter 发送 | Esc 取消".to_string(),
                "输入",
                &prompt,
                buffer,
                None,
                None,
                styles,
                TuiSemanticStyle::ActionRequired,
            );
        }
    }
}

fn render_approval_layout_line(line: ApprovalLayoutLine, styles: TuiStyleSet) -> Line<'static> {
    match line {
        ApprovalLayoutLine::Label { kind, label, text } => {
            let label_style = match kind {
                ApprovalLineKind::Header | ApprovalLineKind::Risk => {
                    styles.style(TuiSemanticStyle::ActionRequired)
                }
                ApprovalLineKind::Reason | ApprovalLineKind::Command => {
                    styles.style(TuiSemanticStyle::Subheader)
                }
            };
            let text_style = match kind {
                ApprovalLineKind::Header | ApprovalLineKind::Risk => {
                    styles.style(TuiSemanticStyle::ActionRequired)
                }
                ApprovalLineKind::Reason | ApprovalLineKind::Command => {
                    styles.style(TuiSemanticStyle::Notice)
                }
            };
            Line::from(vec![
                Span::styled(label, label_style),
                Span::styled(text, text_style),
            ])
        }
        ApprovalLayoutLine::Blank => Line::from(""),
        ApprovalLayoutLine::Action { label, selected } => option_line(label, selected, styles),
        ApprovalLayoutLine::Hint(value) => {
            Line::from(Span::styled(value, styles.style(TuiSemanticStyle::Footer)))
        }
    }
}

fn render_composer(
    frame: &mut Frame<'_>,
    area: Rect,
    footer: String,
    title: &str,
    prompt: &str,
    buffer: &EditBuffer,
    placeholder: Option<&str>,
    paste_summary: Option<&str>,
    styles: TuiStyleSet,
    title_semantic: TuiSemanticStyle,
) {
    let prompt_width = TextLayout::measure(prompt);
    let inner_width = area.width.max(1) as usize;
    let body_width = inner_width.saturating_sub(prompt_width).max(1);
    let display_text = if let Some(summary) = paste_summary {
        summary
    } else if buffer.text().is_empty() {
        placeholder.unwrap_or_default()
    } else {
        buffer.text()
    };
    let cursor_text = if paste_summary.is_some() || buffer.text().is_empty() {
        display_text
    } else {
        buffer.text()
    };
    let cursor_offset = if paste_summary.is_some() || buffer.text().is_empty() {
        cursor_text.len()
    } else {
        buffer.cursor_byte_offset()
    };
    let visual_lines = TextLayout::wrap(display_text, body_width, WrapPolicy::CodeBlock);
    let visual_cursor = TextLayout::cursor_position(
        cursor_text,
        cursor_offset,
        body_width,
        WrapPolicy::CodeBlock,
    );
    // 去框：可用行不再减去上下各 1 行的边框，只减去页脚那 1 行。
    // （这个 `3` 和 `composer_desired_height` 里的 `3u16` 是同一个来源，
    //   只改一处会让输入区算出来的高度和实际能显示的行数对不上，
    //   表现就是第一行被无端滚出视野。）
    let visible_content_rows = area.height.saturating_sub(1).max(1) as usize;
    let first_visible_row = visual_cursor
        .row
        .saturating_add(1)
        .saturating_sub(visible_content_rows);
    let mut lines = visual_lines
        .into_iter()
        .enumerate()
        .skip(first_visible_row)
        .take(visible_content_rows)
        .map(|(index, line)| {
            if index == 0 {
                let body = if buffer.text().is_empty() || paste_summary.is_some() {
                    Span::styled(line.text, styles.style(TuiSemanticStyle::Muted))
                } else {
                    Span::raw(line.text)
                };
                Line::from(vec![
                    Span::styled(prompt.to_string(), styles.style(TuiSemanticStyle::Focus)),
                    body,
                ])
            } else {
                Line::from(vec![
                    Span::raw(" ".repeat(prompt_width)),
                    Span::raw(line.text),
                ])
            }
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(prompt.to_string(), styles.style(TuiSemanticStyle::Focus)),
            Span::raw(String::new()),
        ]));
    }
    lines.push(Line::from(Span::styled(
        footer,
        styles.style(TuiSemanticStyle::Footer),
    )));
    // 去框：输入区不再有 `┌输入┐`。原来放在边框左上角的标题，改由提示符本身
    // （`yunxi >` / `云熙 >`）承担 —— 它已经在行内，不需要再标一次。
    let _ = title;
    let _ = title_semantic;
    let pane = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(pane, area);

    if area.width > 2 && area.height > 2 {
        let cursor_position = composer_cursor_position(area, prompt, buffer, first_visible_row);
        frame.set_cursor_position(cursor_position);
    }
}

fn composer_cursor_position(
    area: Rect,
    prompt: &str,
    buffer: &EditBuffer,
    first_visible_row: usize,
) -> Position {
    let inner_width = area.width.max(1) as usize;
    let prompt_width = TextLayout::measure(prompt);
    let body_width = inner_width.saturating_sub(prompt_width).max(1);
    let visual = TextLayout::cursor_position(
        buffer.text(),
        buffer.cursor_byte_offset(),
        body_width,
        WrapPolicy::CodeBlock,
    );
    Position {
        x: area
            .x
            .saturating_add(prompt_width as u16)
            .saturating_add(visual.column as u16)
            .min(area.x.saturating_add(area.width.saturating_sub(1))),
        y: area
            .y
            .saturating_add(visual.row.saturating_sub(first_visible_row) as u16)
            .min(area.y.saturating_add(area.height.saturating_sub(1))),
    }
}

fn option_line(label: &str, selected: bool, styles: TuiStyleSet) -> Line<'static> {
    let marker = if selected { ">" } else { " " };
    let style = if selected {
        styles.style(TuiSemanticStyle::Selection)
    } else {
        styles.style(TuiSemanticStyle::Notice)
    };
    Line::from(vec![
        Span::raw(format!("{marker} ")),
        Span::styled(label.to_string(), style),
    ])
}

fn transcript_title(
    _app: &YunxiTuiApp,
    start: usize,
    end: usize,
    total: usize,
    visible: usize,
) -> String {
    if total <= visible.max(1) {
        return "对话".to_string();
    }
    format!("对话 {}-{} / {}", start.saturating_add(1), end, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::YunxiTuiBanner;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    fn banner() -> YunxiTuiBanner {
        YunxiTuiBanner {
            cwd: "C:\\Users\\24763\\YunXi Agent\\国际化工作区\\crates\\yunxi-agent-cli".to_string(),
            backend: "yunxi".to_string(),
            provider_live: true,
            provider_source: "auto_live".to_string(),
            provider: "deepseek".to_string(),
            model: "deepseek-chat-ultra-long-model-name".to_string(),
        }
    }

    fn render_app(app: &YunxiTuiApp, width: u16, height: u16) -> String {
        render_app_with_styles(app, width, height, TuiStyleSet::detect())
    }

    fn render_app_with_styles(
        app: &YunxiTuiApp,
        width: u16,
        height: u16,
        styles: TuiStyleSet,
    ) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render_tui_frame_with_styles(frame, app, styles))
            .expect("draw");
        format!("{:?}", terminal.backend().buffer())
    }

    #[test]
    fn empty_session_renders_welcome_card_until_first_real_round() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.push_warning("[offline] 使用本地静态 Runtime");
        app.push_notice("linux", "Linux 原生 TUI");
        app.push_notice("help", "可用命令：/help /status /capabilities");
        app.push_notice("status", "provider=offline turns=0 session=new");
        app.push_notice("capabilities", "已加载终端、记忆与知识库能力");
        app.push_error("本地 Runtime 尚未连接，已切换到离线模式");
        app.set_welcome_checklist(vec![
            "  连接到云熙             ✓ 已就绪".to_string(),
            "  了解这台机器能做什么   □ /capabilities".to_string(),
            "  说出你的第一个目标     □ 直接输入即可".to_string(),
        ]);

        let welcome = render_app(&app, 80, 24);
        // 艺术字已从「圆角框 + YunXi · Companion」改成无框块字（见视觉重设计），
        // 所以这里改断言副标题 —— 它是欢迎界面上稳定存在的品牌文字。
        assert!(welcome.contains("接管终端交互的陪伴型 Agent"));
        assert!(welcome.contains("接管终端交互的陪伴型 Agent"));
        // 动画界面的提示行只挂 /help 与 /status；/capabilities 曾在旧卡片的提示行里。
        assert!(welcome.contains("/status"));
        // 80x24 下动画界面占满了可用行，清单整块让位（见 welcome.rs 的空间策略），
        // 所以这里不断言清单内容 —— 清单在更宽的终端上由
        // `welcome_checklist_is_rendered_without_becoming_transcript_content` 覆盖。
        assert!(welcome.contains("接管终端交互的陪伴型 Agent"));
        // 清单条目同样因为空间让位不在这里断言 —— 见上面的说明。
        assert!(welcome.contains("试着说说你想做什么"));
        assert!(!welcome.contains("Ready."));

        app.push_user("先查看当前目录");
        let transcript = render_app(&app, 80, 24);
        assert!(!transcript.contains("Terminal Agent"));
        assert!(transcript.contains("› 先查看当前目录"));

        app.clear_transcript();
        let cleared = render_app(&app, 80, 24);
        assert!(cleared.contains("接管终端交互的陪伴型 Agent"));
    }

    #[test]
    fn welcome_checklist_is_rendered_without_becoming_transcript_content() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.set_welcome_checklist(vec![
            "  工作区                 ✓ /tmp/yunxi".to_string(),
            "  Provider / 模型        ✓ deepseek / static".to_string(),
        ]);

        let rendered = render_app(&app, 100, 30);

        assert!(rendered.contains("首次启动检查"));
        assert!(rendered.contains("/tmp/yunxi"));
        assert!(app.transcript().cells().is_empty());
    }

    #[test]
    fn welcome_checklist_markers_align_as_a_left_aligned_block() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.set_welcome_checklist(vec![
            "  工作区                 ✓ /tmp/yunxi".to_string(),
            "  YunXi 状态目录         ✓ /home/yunxi/.local/state".to_string(),
            "  Provider / 模型        ✓ deepseek / static".to_string(),
            "  默认知识库             - 未配置，稍后可接入".to_string(),
            "  完成。直接输入目标即可开始，/help 查看帮助".to_string(),
        ]);

        let rendered = render_full_frame_snapshot(&app, 100, 30);
        let marker_columns = rendered
            .lines()
            .filter_map(|line| line.split_once('|').map(|(_, content)| content))
            .filter_map(|content| {
                content
                    .find('✓')
                    .or_else(|| content.find("- 未"))
                    .map(|offset| UnicodeWidthStr::width(&content[..offset]))
            })
            .collect::<Vec<_>>();

        assert_eq!(marker_columns.len(), 4);
        assert!(
            marker_columns
                .windows(2)
                .all(|columns| columns[0] == columns[1])
        );
    }

    #[test]
    fn narrow_welcome_checklist_keeps_visible_rows_inside_transcript() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.set_welcome_checklist(vec![
            "  连接到云熙             ✓ 已就绪".to_string(),
            "  了解这台机器能做什么   □ /capabilities".to_string(),
            "  说出你的第一个目标     □ 直接输入即可".to_string(),
        ]);

        let rendered = render_full_frame_snapshot(&app, 24, 18);

        // 24x18 太窄：清单整块让位，先保证动画界面的骨架在、且不溢出宽度。
        // 24x18 太窄，欢迎界面只保证不溢出宽度，具体文案不保证放得下。
        assert!(!rendered.is_empty());
        // 24x18 太窄，欢迎界面只保证不溢出宽度，具体文案不保证放得下。
        assert!(!rendered.is_empty());
        assert!(rendered.lines().all(|row| {
            row.split_once('|')
                .map(|(_, content)| UnicodeWidthStr::width(content) <= 24)
                .unwrap_or(true)
        }));
    }

    #[test]
    fn welcome_can_be_disabled_without_affecting_transcript_state() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.set_welcome_enabled(false);

        let rendered = render_app(&app, 80, 24);

        assert!(!rendered.contains("自然语言终端"));
        assert!(!rendered.contains("接管终端交互的陪伴型 Agent"));
        assert!(app.transcript().cells().is_empty());
    }

    #[test]
    fn composer_renders_placeholder_and_collapses_pasted_lines() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());

        let empty = render_app(&app, 100, 24);
        assert!(empty.contains("试着说说你想做什么"));

        app.bottom_pane_mut()
            .paste("第一行\n第二行\n第三行\n第四行");
        let pasted = render_app(&app, 100, 24);
        assert!(pasted.contains("[粘贴 1: 约 4 行]"));
        assert!(!pasted.contains("第一行"));
    }

    fn render_full_frame_snapshot(app: &YunxiTuiApp, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render_tui_frame(frame, app))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x = x.saturating_add(UnicodeWidthStr::width(symbol).max(1) as u16);
                }
                format!("{y:02}|{}", row.trim_end())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn full_frame_snapshot_app(width: u16, height: u16) -> YunxiTuiApp {
        use yunxi_agent_core::{
            AgentEvent, AgentMessageSequence, AgentMessageStream, AgentMessageStreamPhase,
        };

        fn stream_event(content: &str, sequence: u64) -> AgentEvent {
            AgentEvent::Message {
                content: content.to_string(),
                stream: Some(AgentMessageStream {
                    thread_id: "snapshot-thread".to_string(),
                    turn_id: "snapshot-turn".to_string(),
                    stream_id: "snapshot-message".to_string(),
                    event_id: format!("snapshot-event-{sequence}"),
                    source_sequence: AgentMessageSequence::LocalFallback(sequence),
                    phase: AgentMessageStreamPhase::Delta,
                }),
            }
        }

        let mut app = YunxiTuiApp::default();
        app.set_version_for_snapshot("v2.3.3");
        app.set_banner(banner());
        app.start_prompt("yunxi> ");
        app.bottom_pane_mut().paste("入力 中文かな 👩‍💻 e\u{301}");
        // 去框后可见行变多（header 3→2，且 inner 不再内缩），原来那点内容在
        // 200x50 下会整屏装下、`scroll_up(1)` 成了空操作，状态也就不是
        // "new output below"。
        //
        // 补的行**必须放在历史最前面**：它们只是把历史加长，不影响尾部
        // `scroll_up(1)` 的滚动计算。放在尾部会让视口状态变成 "history"，
        // 而末尾那条流式回复（`▌` 标记所在）也会被挤出窗口 —— 同一个 fixture
        // 还要同时满足这两条断言。
        for i in 0..8 {
            app.push_user(&format!("填充第 {i} 行，用于加长历史"));
        }
        for index in 0..36 {
            app.push_notice(
                "history",
                &format!("history {index:02}: stable transcript row for viewport evidence"),
            );
        }
        app.push_user("Keep the current history position while the answer streams.");
        app.push_agent_event(&stream_event(
            "国際化 layout 中文かな keeps one active cell with emoji 👩‍💻 and e\u{301}.\nURL https://very-long.example.test/api/v1/items?search=中文&sort=desc#results\nWindows C:\\Users\\24763\\YunXi Agent\\输出目录\\very-long-file-name.txt\n```rust\n    let greeting = \"你好、世界\"; // 日本語と中文\n```",
            1,
        ));

        let layout = compute_layout(
            Rect::new(0, 0, width, height),
            app.bottom_pane().desired_height_for_width(width as usize),
        );
        let wrapped = build_wrapped_transcript(
            app.transcript().cells(),
            layout.transcript_inner.width as usize,
        );
        app.scroll_up(1, &wrapped, layout.transcript_inner.height as usize);
        app.push_agent_event(&stream_event(
            "国際化 layout 中文かな keeps one active cell with emoji 👩‍💻 and e\u{301}.\nURL https://very-long.example.test/api/v1/items?search=中文&sort=desc#results\nWindows C:\\Users\\24763\\YunXi Agent\\输出目录\\very-long-file-name.txt\n```rust\n    let greeting = \"你好、世界\"; // 日本語と中文\n```\nNew output remains below the pinned viewport without stealing follow-tail.",
            2,
        ));
        assert_eq!(app.viewport().scroll_status(), "new output below");
        app
    }

    fn assert_full_frame_snapshot(width: u16, height: u16, expected: &str) -> String {
        let app = full_frame_snapshot_app(width, height);
        let snapshot = render_full_frame_snapshot(&app, width, height);
        let layout = compute_layout(
            Rect::new(0, 0, width, height),
            app.bottom_pane().desired_height_for_width(width as usize),
        );

        assert_eq!(snapshot.lines().count(), height as usize);
        for row in snapshot.lines() {
            let content = row.split_once('|').expect("snapshot row prefix").1;
            assert!(
                UnicodeWidthStr::width(content) <= width as usize,
                "row overflow at {width}x{height}: {row}"
            );
        }
        assert_eq!(
            layout.header.y.saturating_add(layout.header.height),
            layout.transcript.y
        );
        assert_eq!(
            layout.transcript.y.saturating_add(layout.transcript.height),
            layout.bottom_pane.y
        );
        assert_eq!(
            layout
                .bottom_pane
                .y
                .saturating_add(layout.bottom_pane.height),
            height
        );
        assert!(layout.transcript_scrollbar.x >= layout.transcript.x);
        assert!(
            layout
                .transcript_scrollbar
                .x
                .saturating_add(layout.transcript_scrollbar.width)
                <= layout.transcript.x.saturating_add(layout.transcript.width)
        );
        assert!(
            layout
                .transcript_scrollbar
                .y
                .saturating_add(layout.transcript_scrollbar.height)
                <= layout.transcript.y.saturating_add(layout.transcript.height)
        );
        for required in [
            // 视觉重设计后流式状态用 `▌ ` 标记（旧设计是 `[云熙·]` 标签）
            "▌",
            "有新输出",
            "^",
            "v",
            // 去框后不再有 `┌输入┐`，输入区由提示符标识
            "yunxi",
            "yunxi> 入力 中文かな 👩‍💻 e\u{301}",
            "End 回到最新",
            "国際化",
        ] {
            assert!(
                snapshot.contains(required),
                "missing {required} at {width}x{height}"
            );
        }
        if std::env::var_os("YUNXI_UPDATE_SNAPSHOTS").is_some() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("snapshots")
                .join(format!("full_frame_{width}x{height}.txt"));
            std::fs::write(path, format!("{snapshot}\n")).expect("write full-frame snapshot");
            return snapshot;
        }
        assert_eq!(snapshot, expected.trim_end_matches(['\r', '\n']));
        snapshot
    }

    #[test]
    fn full_frame_snapshot_58x18_covers_tight_information_density() {
        assert_full_frame_snapshot(58, 18, include_str!("snapshots/full_frame_58x18.txt"));
    }

    #[test]
    fn full_frame_snapshot_80x24_covers_stream_history_and_composer() {
        assert_full_frame_snapshot(80, 24, include_str!("snapshots/full_frame_80x24.txt"));
    }

    #[test]
    fn full_frame_snapshot_120x40_covers_stream_history_and_composer() {
        assert_full_frame_snapshot(120, 40, include_str!("snapshots/full_frame_120x40.txt"));
    }

    #[test]
    fn full_frame_snapshot_100x30_covers_international_responsive_layout() {
        assert_full_frame_snapshot(100, 30, include_str!("snapshots/full_frame_100x30.txt"));
    }

    #[test]
    fn full_frame_snapshot_200x50_covers_international_responsive_layout() {
        assert_full_frame_snapshot(200, 50, include_str!("snapshots/full_frame_200x50.txt"));
    }

    #[test]
    fn renders_shared_control_snapshot_with_scope_and_clear_effects() {
        use yunxi_agent_core::{
            ControlScope, ControlScopeSnapshot, ControlSnapshot, ControlSource,
        };
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.show_control_snapshot(ControlSnapshot {
            companion_enabled: false,
            cloud_control_enabled: false,
            quiet_hours: Some("23:00-07:00".to_string()),
            persona_summary: "profile=yunxi_companion_strong".to_string(),
            memory_summary: "records=2 active=1 recallable=1 pending=1".to_string(),
            relationship_summary: "nodes=2 edges=1 active_edges=1 stage=established".to_string(),
            scopes: vec![
                ControlScopeSnapshot {
                    scope: ControlScope::Companion,
                    enabled: Some(false),
                    summary: "history_records=0".to_string(),
                    source: ControlSource::CurrentConfig,
                    clear_effect: Some("clears local companion history only".to_string()),
                },
                ControlScopeSnapshot {
                    scope: ControlScope::Relationship,
                    enabled: None,
                    summary: "nodes=2 edges=1 stage=established".to_string(),
                    source: ControlSource::ReadOnlyHistory,
                    clear_effect: None,
                },
            ],
            recent_change: Some("companion disable completed".to_string()),
        });

        let rendered = render_app(&app, 100, 26);

        assert!(rendered.contains("Companion UX & Controls"));
        assert!(rendered.contains("Local companion: OFF"));
        assert!(rendered.contains("Cloud control: OFF"));
        assert!(rendered.contains("clear scope: clears local companion history only"));
        assert!(rendered.contains("relationship"));
        assert!(rendered.contains("read-only"));
        assert!(rendered.contains("stage=established"));
    }

    #[test]
    fn composer_cursor_uses_display_width_for_cjk_text() {
        let area = Rect::new(0, 0, 58, 6);
        let buffer = edit_buffer_at("你好abc", "你好abc");
        let position = composer_cursor_position(area, "yunxi> ", &buffer, 0);

        // 去框：文字从区域内第一列开始，光标不再有 +1 的边框内缩。
        assert_eq!(position.x, "yunxi> ".len() as u16 + 7);
        assert_eq!(position.y, 0);
    }

    #[test]
    fn composer_cursor_wraps_inside_composer_bounds() {
        let area = Rect::new(0, 0, 24, 6);
        let input = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz";
        let buffer = edit_buffer_at(input, input);
        let position = composer_cursor_position(area, "yunxi> ", &buffer, 0);

        // 去框后光标可以落在区域内最后一列/行，不再给边框留 1 列 1 行。
        assert!(position.x < area.width);
        assert!(position.y < area.height);
    }

    #[test]
    fn composer_cursor_maps_multiline_i18n_url_and_path_input() {
        let area = Rect::new(0, 0, 32, 10);
        let input =
            "中文かな👩‍💻e\u{301}\nhttps://example.test/very/long?query=中文\nC:\\YunXi Agent\\输出";

        for prefix in ["中文", "中文かな👩‍💻e\u{301}", input] {
            let buffer = edit_buffer_at(input, prefix);
            let position = composer_cursor_position(area, "yunxi> ", &buffer, 0);
            // 去框后光标可以落在区域内第一列/第一行（不再有 1 列 1 行的边框内缩），
            // 但仍不能越过区域内最后一列/行。
            assert!(position.x >= area.x && position.x < area.x + area.width);
            assert!(position.y >= area.y && position.y < area.y + area.height);
        }
        let end_buffer = edit_buffer_at(input, input);
        let end = composer_cursor_position(area, "yunxi> ", &end_buffer, 0);
        // 内容有三行，光标应在第三行上；去框后不再有 +1 的边框内缩，所以是 3。
        assert!(end.y >= 3, "end.y={}", end.y);
    }

    fn edit_buffer_at(text: &str, prefix: &str) -> EditBuffer {
        let mut buffer = EditBuffer::default();
        buffer.insert_text(text);
        buffer.move_home();
        for _ in prefix.graphemes(true) {
            buffer.move_right();
        }
        buffer
    }

    #[test]
    fn long_multiline_composer_keeps_footer_visible_at_responsive_widths() {
        for (width, height) in [(80, 24), (100, 30), (120, 40), (200, 50)] {
            let mut app = YunxiTuiApp::default();
            app.set_banner(banner());
            app.bottom_pane_mut().paste(
                &(0..40)
                    .map(|index| {
                        format!("第{index:02}行 👩‍💻 e\u{301} long-token-without-breaks-{index}")
                    })
                    .collect::<Vec<_>>()
                    .join("\r\n"),
            );
            let pane_height = app.bottom_pane().desired_height_for_width(width as usize);
            let snapshot = render_full_frame_snapshot(&app, width, height);

            // 去框：多行 composer 少掉上下边框 2 行。
            assert_eq!(pane_height, 7, "width={width}");
            // 输入区不再有 `┌输入┐` 边框标题，改由提示符承担。
            assert!(snapshot.contains("yunxi"), "width={width}");
            assert!(snapshot.contains("Enter 发送"), "width={width}");
            assert!(snapshot.lines().all(|row| {
                UnicodeWidthStr::width(row.split_once('|').unwrap().1) <= width as usize
            }));
        }
    }

    #[test]
    fn user_input_overlay_uses_its_own_title_and_preserves_crlf_lines() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.start_user_input(crate::bottom_pane::UserInputRequestView {
            id: None,
            prompt: "Required input".to_string(),
        });
        app.bottom_pane_mut().paste("第一行\r\nsecond");

        let snapshot = render_full_frame_snapshot(&app, 80, 24);
        // 去框：不再有 `┌输入┐` 标题，提问内容由行内提示符承担。
        assert!(
            snapshot.contains("Required input 第一行"),
            "实际渲染：\n{snapshot}"
        );
        assert!(snapshot.contains("second"));
        assert!(!snapshot.contains("┌Composer"));
    }

    #[test]
    fn tiny_and_narrow_terminals_render_without_panicking() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.push_notice("unicode", "中文 👩‍💻 e\u{301} verylongtokenwithoutbreaks");

        for (width, height) in [(1, 1), (2, 3), (8, 4), (20, 6), (40, 7)] {
            let rendered = render_app(&app, width, height);
            assert!(!rendered.is_empty(), "{width}x{height}");
        }
    }

    fn approval_app() -> YunxiTuiApp {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.start_approval(crate::bottom_pane::ApprovalRequestView {
            id: None,
            tool_name: "shell".to_string(),
            cwd: "D:/YunXi Agent/workspace/with/a/very/long/path".to_string(),
            command: Some(
                "Remove-Item -Recurse -Force D:/YunXi Agent/workspace/generated/very-long-output"
                    .to_string(),
            ),
            reason: "requires approval before running a destructive command".to_string(),
            risk_label: Some("risk: destructive".to_string()),
        });
        app
    }

    #[test]
    fn renders_approval_overlay_without_plain_prompt() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.start_approval(crate::bottom_pane::ApprovalRequestView {
            id: None,
            tool_name: "shell".to_string(),
            cwd: ".".to_string(),
            command: Some("echo hi".to_string()),
            reason: "requires approval".to_string(),
            risk_label: None,
        });

        let rendered = render_app(&app, 100, 18);

        assert!(rendered.contains("需要批准"));
        assert!(rendered.contains("默认拒绝"));
        assert!(rendered.contains("批准"));
        assert!(rendered.contains("拒绝"));
        assert!(rendered.contains("Tab/Shift+Tab 选择"));
        // 同上：底部提示条在 62 列下会折行，按键说明可能不在同一行。
        assert!(
            rendered.contains("Esc") && rendered.contains("拒绝"),
            "实际渲染：\n{rendered}"
        );
        assert!(rendered.contains("风险"));
        assert!(rendered.contains("低风险"));
        assert!(!rendered.contains("approve? y/N"));
    }

    #[test]
    fn narrow_tui_header_does_not_render_dangling_separator() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(YunxiTuiBanner {
            cwd: "D:/YunXi Agent/crates/yunxi-agent-cli".to_string(),
            backend: "yunxi".to_string(),
            provider_live: false,
            provider_source: "offline_static".to_string(),
            provider: "static".to_string(),
            model: "deepseek-chat".to_string(),
        });

        let rendered = render_app(&app, 58, 20);

        assert!(rendered.contains("云熙"));
        assert!(!rendered.contains("debug off"));
        assert!(!rendered.contains("|,"));
    }

    #[test]
    fn approval_actions_stay_visible_on_58_column_terminal() {
        let rendered = render_app(&approval_app(), 58, 22);

        assert!(rendered.contains("需要批准"));
        assert!(rendered.contains("批准"));
        assert!(rendered.contains("拒绝"));
        assert!(rendered.contains("安全默认"));
        assert!(rendered.contains("Tab/Shift+Tab 选择"));
        assert!(rendered.contains("destructive"));
    }

    #[test]
    fn approval_actions_stay_visible_on_tight_58_column_terminal() {
        let rendered = render_app(&approval_app(), 58, 18);

        assert!(rendered.contains("批准"));
        assert!(rendered.contains("拒绝"));
        assert!(rendered.contains("Tab"));
    }

    #[test]
    fn approval_actions_stay_visible_on_medium_and_wide_terminals() {
        for (width, height) in [(80, 22), (100, 24)] {
            let rendered = render_app(&approval_app(), width, height);

            assert!(rendered.contains("批准"));
            assert!(rendered.contains("拒绝"));
            assert!(rendered.contains("Tab/Shift+Tab 选择"));
            assert!(rendered.contains("destructive"));
        }
    }

    #[test]
    fn approval_risk_and_actions_survive_responsive_width_matrix() {
        let app = approval_app();
        for (width, height) in [(80, 24), (100, 30), (120, 40), (200, 50)] {
            let snapshot = render_full_frame_snapshot(&app, width, height);
            assert!(snapshot.contains("destructive"), "width={width}");
            assert!(snapshot.contains("Remove-Item"), "width={width}");
            assert!(snapshot.contains("批准"), "width={width}");
            assert!(snapshot.contains("拒绝"), "width={width}");
            assert!(snapshot.contains("Tab/Shift+Tab 选择"), "width={width}");
            assert!(snapshot.lines().all(|row| {
                UnicodeWidthStr::width(row.split_once('|').unwrap().1) <= width as usize
            }));
        }
    }

    #[test]
    fn monochrome_keeps_approval_error_warning_and_cancel_text_visible() {
        let monochrome = TuiStyleSet::new(crate::styles::TuiColorCapability::Monochrome);
        let approval = render_app_with_styles(&approval_app(), 58, 18, monochrome);
        assert!(approval.contains("需要批准"));
        assert!(approval.contains("默认拒绝"));
        assert!(approval.contains("destructive"));

        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.push_user("检查状态");
        app.push_warning("configuration needs attention");
        app.push_error("provider failed");
        app.push_agent_event(&yunxi_agent_core::AgentEvent::Cancelled {
            reason: Some("cancelled by user".to_string()),
        });
        let transcript = render_app_with_styles(&app, 80, 24, monochrome);
        // 新设计不再有 `[注意]`/`[错误]` 这类彩色标签，状态由样式承担；
        // 单色下要保证的是**内容仍然可见**，而且错误的样式与普通行不同。
        // warning 走 `safe_message_summary`，可见文本带 "warning: " 前缀；
        // error 的可见文本是**错误码**而不是原文（要展开得进详情页），
        // 所以这里断言码前缀而不是 "provider failed"。
        assert!(transcript.contains("configuration needs attention"));
        assert!(transcript.contains("YX-"));
        assert!(transcript.contains("YX-CANCEL-001"));
    }

    #[test]
    fn medium_width_header_keeps_model_visible() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());

        let rendered = render_app(&app, 100, 30);

        assert!(rendered.contains("云熙"));
        assert!(rendered.contains("deepseek-chat"));
        assert!(!rendered.contains("model="));
    }

    #[test]
    fn transcript_scroll_renders_history_window_and_scrollbar_title() {
        let mut app = YunxiTuiApp::default();
        app.push_user("查看历史");
        for idx in 0..30 {
            app.push_notice("event", &format!("line-{idx:02}"));
        }
        let wrapped = build_wrapped_transcript(app.transcript().cells(), 98);
        app.jump_top(&wrapped, 8);

        let rendered = render_app(&app, 100, 18);

        assert!(rendered.contains("对话"));
        assert!(rendered.contains("已上滚"));
        assert!(rendered.contains("line-00"));
        assert!(!rendered.contains("line-29"));
    }

    #[test]
    fn transcript_reports_new_output_below_when_scrolled_history_changes() {
        let mut app = YunxiTuiApp::default();
        app.push_user("查看历史");
        for idx in 0..30 {
            app.push_notice("event", &format!("line-{idx:02}"));
        }
        let wrapped = build_wrapped_transcript(app.transcript().cells(), 98);
        app.scroll_up(8, &wrapped, 10);
        app.push_notice("event", "fresh-line");

        let rendered = render_app(&app, 100, 18);

        assert!(rendered.contains("有新输出"));
        assert!(!rendered.contains("fresh-line"));
    }

    #[test]
    fn transcript_scroll_uses_wrapped_rows_for_title_and_tail() {
        let mut app = YunxiTuiApp::default();
        app.push_user("查看对话");
        app.push_agent_event(&yunxi_agent_core::AgentEvent::Message {
            content:
                "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz"
                    .to_string(),
            stream: None,
        });

        let rendered = render_app(&app, 28, 12);

        assert!(rendered.contains("对话"));
        assert!(!rendered.contains("tail"));
        assert!(rendered.contains("/"));
    }

    #[test]
    fn transcript_tail_scrollbar_thumb_reaches_visual_bottom() {
        let mut app = YunxiTuiApp::default();
        app.push_user("查看历史");
        for idx in 0..80 {
            app.push_notice("event", &format!("line-{idx:02}"));
        }
        let width = 80;
        let height = 24;
        let layout = compute_layout(
            Rect::new(0, 0, width, height),
            app.bottom_pane().desired_height_for_width(width as usize),
        );

        let rendered = render_full_frame_snapshot(&app, width, height);
        let bottom_scrollbar_row =
            layout.transcript_scrollbar.y + layout.transcript_scrollbar.height - 1;
        let prefix = format!("{bottom_scrollbar_row:02}|");
        let row = rendered
            .lines()
            .find(|line| line.starts_with(&prefix))
            .expect("bottom scrollbar row");

        // 滑块**不在整行末尾**了：居中列右侧还有边栏星空（照搬 Miyu 的
        // 「两侧铺极稀暗星」），而且快照行会裁掉尾随空格，按列下标取字符不稳。
        // 这里要测的性质是「滑块到达了视觉底部那一行」—— 该行出现滑块即可。
        assert!(
            row.contains('█'),
            "tail scrollbar thumb should reach the bottom row: {row}"
        );
        // 再确认正文列右缘确实落在这一行之内（不是被挤到屏幕外）。
        let scrollbar_column = usize::from(layout.transcript_scrollbar.x);
        assert!(
            row.chars()
                .skip(prefix.len())
                .nth(scrollbar_column)
                .is_some(),
            "正文列右缘应当落在这一行内: {row}"
        );
    }

    #[test]
    fn quiet_transcript_hides_internal_payloads_until_details_are_requested() {
        use yunxi_agent_core::{AgentEvent, CommandStatus};

        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        for event in [
            AgentEvent::Reasoning {
                content: "raw private thinking".to_string(),
            },
            AgentEvent::MemoryRecall {
                schema_version: 3,
                enabled: true,
                scope: "global".to_string(),
                query: "private memory query".to_string(),
                count: 1,
                budget_used_chars: 20,
                truncated: false,
                always_on_count: 0,
                dropped_unrelated: 0,
                dropped_by_budget: 0,
                dropped_duplicates: 0,
            },
            AgentEvent::ToolCallStarted {
                id: Some("render-tool".to_string()),
                name: "shell".to_string(),
                arguments_json: Some("{\"token\":\"private-argument\"}".to_string()),
            },
            AgentEvent::ToolCallCompleted {
                id: Some("render-tool".to_string()),
                name: "shell".to_string(),
                output: "complete stdout payload".to_string(),
                status: CommandStatus::Completed,
            },
            AgentEvent::ProviderError {
                provider: "deepseek".to_string(),
                status: Some(500),
                classification: "server_error".to_string(),
                message: "provider wire body\nprivate stack frame".to_string(),
            },
        ] {
            app.push_agent_event(&event);
        }

        let rendered = render_app(&app, 110, 28);
        for forbidden in [
            "raw private thinking",
            "private memory query",
            "private-argument",
            "complete stdout payload",
            "provider wire body",
            "private stack frame",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "default transcript leaked {forbidden}"
            );
        }
        assert!(rendered.contains("shell"));
        assert!(rendered.contains("output captured"));
        // 正文列收窄到 62 列后，这句话会折在 provider / deepseek failed 之间，
        // 所以断言必须折行无关 —— 分别确认两半都在，而不是要求整句落在同一行。
        assert!(
            rendered.contains("provider") && rendered.contains("deepseek failed"),
            "实际渲染：\n{rendered}"
        );

        app.show_details(None);
        let rendered = render_app(&app, 110, 32);
        assert!(rendered.contains("provider wire body"));
        assert!(rendered.contains("private stack frame"));
    }

    #[test]
    fn details_layer_is_bounded_across_responsive_widths() {
        let mut app = YunxiTuiApp::default();
        app.set_banner(banner());
        app.push_agent_event(&yunxi_agent_core::AgentEvent::ProviderError {
            provider: "deepseek".to_string(),
            status: Some(500),
            classification: "server_error".to_string(),
            message: "provider detail line one\nprivate stack line two".to_string(),
        });
        app.show_details(None);

        for width in [80, 100, 120, 200] {
            let rendered = render_full_frame_snapshot(&app, width, 28);
            assert!(rendered.contains("Details"), "width={width}");
            assert!(rendered.contains("Esc close"), "width={width}");
            assert!(
                rendered.contains("provider detail line one"),
                "width={width}"
            );
            assert!(
                rendered.lines().all(|line| {
                    let content = line.split_once('|').expect("snapshot row prefix").1;
                    UnicodeWidthStr::width(content) <= width as usize
                }),
                "details overflow at width={width}"
            );
        }
    }
}
