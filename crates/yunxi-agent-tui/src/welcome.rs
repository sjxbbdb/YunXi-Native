//! 云熙欢迎界面 - 空会话的大厅模式
//!
//! 设计理念：
//! - 空会话显示居中的动画欢迎界面
//! - 星空背景 + 艺术字 + 提示信息
//! - 第一条消息发送后，欢迎界面撤走，进入对话模式
//! - 40ms 一帧的流畅动画

use crate::text_layout::TextLayout;
use crate::yunxi_starfield::{
    BannerArt, Seg, YUNXI_INK, YUNXI_LAVENDER, YUNXI_PURPLE, YUNXI_SILVER, gradient_t, lerp_color,
    star_seg,
};
use ratatui::style::Modifier;

/// 星空密度（值越大越稀）
const STAR_SPARSITY: u32 = 9;
/// 星域比艺术字大一圈（左右各这么多列）
const STAR_PAD_X: usize = 14;
/// 检查清单最少需要几行才值得显示（空行 + 标题 + 至少一项）
const CHECKLIST_MIN_ROWS: usize = 3;
/// 检查清单标题（与旧卡片的文案保持一致）
const CHECKLIST_TITLE: &str = "首次启动检查";

/// 欢迎场景状态
#[derive(Clone, Debug)]
pub struct WelcomeScene {
    art: BannerArt,
    tick: usize,
    born: usize, // 淡入帧数
}

impl WelcomeScene {
    /// 创建新的欢迎场景
    pub fn new() -> Self {
        Self {
            art: BannerArt::yunxi_builtin(false),
            tick: 0,
            born: 0,
        }
    }

    /// 走一帧动画
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if self.born < 24 {
            self.born += 1;
        }
    }

    /// 淡入进度 (0.0 -> 1.0)
    fn fade_in(&self) -> f32 {
        (self.born as f32 / 24.0).min(1.0)
    }

    /// 艺术字宽度
    pub fn art_cols(&self) -> usize {
        self.art.cols()
    }

    /// 艺术字高度
    pub fn art_rows(&self) -> usize {
        self.art.rows()
    }

    /// 渲染欢迎界面
    ///
    /// 返回每行的文本片段
    ///
    /// 等价于 [`WelcomeScene::render_with_checklist`] 且不带检查清单；
    /// 既有调用方（`render.rs` 与示例）无需改动。
    pub fn render(&self, cols: usize, rows: usize) -> Vec<Vec<Seg>> {
        self.render_with_checklist(cols, rows, &[])
    }

    /// 便捷入口：`None` 或空清单等价于 [`WelcomeScene::render`]
    ///
    /// 调用方可以直接把 `app.welcome_checklist()` 包成 `Some(..)` 传进来：
    /// 非首启时该切片为空，渲染结果与不带清单时逐字节相同。
    pub fn render_with_optional_checklist(
        &self,
        cols: usize,
        rows: usize,
        checklist: Option<&[String]>,
    ) -> Vec<Vec<Seg>> {
        match checklist {
            Some(items) => self.render_with_checklist(cols, rows, items),
            None => self.render(cols, rows),
        }
    }

    /// 渲染欢迎界面，并在动画下方追加可选的首次启动检查清单
    ///
    /// 首个启动原来走的是 `render.rs` 里的旧卡片，只有非首启用户才能看到这个
    /// 动画界面——新界面在最需要它的时刻反而不可见。把清单交给本函数渲染后，
    /// 首启也能看到动画，同时不丢检查清单：
    ///
    /// ```ignore
    /// let output = scene.render_with_checklist(width, height, app.welcome_checklist());
    /// ```
    ///
    /// `checklist` 为空时输出与原 [`WelcomeScene::render`] 完全一致（逐行逐片段
    /// 相等），所以调用方可以无条件透传 `welcome_checklist()`。
    /// 行数不够时从清单末尾开始截断，艺术字与提示行始终优先保留。
    pub fn render_with_checklist(
        &self,
        cols: usize,
        rows: usize,
        checklist: &[String],
    ) -> Vec<Vec<Seg>> {
        let art_cols = self.art.cols();
        let art_rows = self.art.rows();
        let fade_t = self.fade_in();

        // 渐变艺术字
        let mut banner = self.gradient_banner();
        if fade_t < 1.0 {
            for row in &mut banner {
                for seg in row {
                    seg.style = seg.style.add_modifier(Modifier::DIM);
                }
            }
        }

        // 提示行
        let hint_rows = self.hint_rows(cols);
        // 首启检查清单接在提示行下方。只有艺术字之外的剩余行才分给清单，
        // 空间不足时先丢清单末尾（乃至整个清单），绝不把艺术字挤出屏幕。
        let mut checklist_rows = self.checklist_rows(cols, checklist);
        let footer_space = rows
            .saturating_sub(art_rows)
            .saturating_sub(hint_rows.len());
        if checklist_rows.len() > footer_space {
            if footer_space < CHECKLIST_MIN_ROWS {
                checklist_rows.clear();
            } else {
                checklist_rows.truncate(footer_space);
            }
        }
        let block_rows = art_rows + hint_rows.len() + checklist_rows.len();

        // 垂直居中
        let top = rows.saturating_sub(block_rows) / 2;
        let left = cols.saturating_sub(art_cols) / 2;

        // 星域范围
        let star_left = left.saturating_sub(STAR_PAD_X);
        let star_right = (left + art_cols + STAR_PAD_X).min(cols);
        let star_top = top.saturating_sub(2);
        let star_bottom = top + art_rows + 2;

        let mut out = Vec::with_capacity(rows);
        for y in 0..rows {
            if y >= top && y < top + art_rows {
                // 艺术字行
                let mut row = Vec::new();
                for x in 0..left {
                    row.push(self.star_or_blank(x, y, star_left, star_right));
                }
                row.extend(banner[y - top].iter().cloned());
                for x in (left + art_cols)..cols {
                    row.push(self.star_or_blank(x, y, star_left, star_right));
                }
                out.push(row);
            } else if y >= top + art_rows && y < top + block_rows {
                // 提示行 + 首启检查清单
                let index = y - top - art_rows;
                if index < hint_rows.len() {
                    out.push(hint_rows[index].clone());
                } else {
                    out.push(checklist_rows[index - hint_rows.len()].clone());
                }
            } else if y >= star_top && y < star_bottom {
                // 星空区域
                let mut row = Vec::new();
                for x in 0..cols {
                    row.push(self.star_or_blank(x, y, star_left, star_right));
                }
                out.push(row);
            } else {
                out.push(Vec::new());
            }
        }
        out
    }

    /// 生成渐变艺术字
    fn gradient_banner(&self) -> Vec<Vec<Seg>> {
        let cols = self.art.cols();
        let rows = self.art.rows();
        let mut result = Vec::with_capacity(rows);

        for (y, line) in self.art.lines.iter().enumerate() {
            let mut row = Vec::new();
            for (x, ch) in line.chars().enumerate() {
                if ch == ' ' {
                    row.push(Seg::raw(" "));
                } else {
                    // 斜向渐变：银白 -> 紫色
                    let t = gradient_t(x, y, cols, rows);
                    let style = lerp_color(YUNXI_SILVER, YUNXI_PURPLE, t);
                    row.push(Seg::new(ch.to_string(), style));
                }
            }
            result.push(row);
        }
        result
    }

    /// 提示行
    fn hint_rows(&self, cols: usize) -> Vec<Vec<Seg>> {
        let fade_t = self.fade_in();
        let dim_style = lerp_color(YUNXI_INK, YUNXI_LAVENDER, fade_t * 0.6);
        let highlight_style = lerp_color(YUNXI_INK, YUNXI_PURPLE, fade_t * 0.9);

        vec![
            Vec::new(), // 空行
            self.center(vec![Seg::new(&self.art.subtitle, dim_style)], cols),
            Vec::new(), // 空行
            self.center(
                vec![Seg::new("用自然语言直接说你想做什么", dim_style)],
                cols,
            ),
            self.center(vec![Seg::new("shell 命令照常可用", dim_style)], cols),
            Vec::new(), // 空行
            self.center(
                vec![
                    Seg::new("/help", highlight_style),
                    Seg::new(" 了解更多  ", dim_style),
                    // 这里原来是 `/config 设置`，但 main.rs 从未注册过 /config，
                    // 敲下去会落进普通对话：界面把一个不存在的命令当成设置入口推荐。
                    // `/status` 已经是真实存在的功能面板（人格/灵魂/记忆/陪伴/情书/
                    // 云端控制的开关状态与各自的启用环境变量），也就是用户要找的
                    // “设置入口”。所以改成指向真实命令；用“查看设置”而不是“设置”，
                    // 是因为该面板只展示状态与启用方式，并不在界面里直接改配置，
                    // 这样既保留了原来的引导意图，也不过度承诺。
                    Seg::new("/status", highlight_style),
                    Seg::new(" 查看设置", dim_style),
                ],
                cols,
            ),
        ]
    }

    /// 首次启动检查清单行
    ///
    /// 逐字复刻旧卡片（`render.rs::render_welcome`）的排版：空行 + 标题 + 左对齐的
    /// 条目块，标签补空格对齐标记列，`完成。` 开头的收尾行单独放在最后。
    /// 这样首启从旧卡片切到本动画界面时，清单的列对齐不会变化。
    fn checklist_rows(&self, cols: usize, checklist: &[String]) -> Vec<Vec<Seg>> {
        if checklist.is_empty() {
            return Vec::new();
        }
        let fade_t = self.fade_in();
        let dim_style = lerp_color(YUNXI_INK, YUNXI_LAVENDER, fade_t * 0.6);
        let title_style = lerp_color(YUNXI_INK, YUNXI_PURPLE, fade_t * 0.9);

        let mut items = Vec::new();
        let mut completion = None;
        for item in checklist {
            let item = item.trim();
            if item.starts_with("完成。") {
                completion = Some(item.to_string());
                continue;
            }
            let Some((marker_offset, marker)) = item
                .find('✓')
                .map(|offset| (offset, '✓'))
                .or_else(|| item.find('-').map(|offset| (offset, '-')))
            else {
                items.push((item.to_string(), ' ', String::new()));
                continue;
            };
            items.push((
                item[..marker_offset].trim().to_string(),
                marker,
                item[marker_offset + marker.len_utf8()..].trim().to_string(),
            ));
        }

        let label_width = items
            .iter()
            .map(|(label, _, _)| TextLayout::measure(label))
            .max()
            .unwrap_or_default();

        let mut rows = vec![
            Vec::new(), // 空行，与艺术字提示行分开
            vec![Seg::new(
                TextLayout::truncate(&format!("  {CHECKLIST_TITLE}"), cols),
                title_style,
            )],
        ];
        for (label, marker, value) in items {
            let line = if marker == ' ' {
                format!("  {label}")
            } else {
                let padding = " ".repeat(label_width.saturating_sub(TextLayout::measure(&label)));
                format!("  {label}{padding} {marker} {value}")
            };
            rows.push(vec![Seg::new(TextLayout::truncate(&line, cols), dim_style)]);
        }
        if let Some(completion) = completion {
            rows.push(Vec::new());
            rows.push(vec![Seg::new(
                TextLayout::truncate(&format!("  {completion}"), cols),
                dim_style,
            )]);
        }
        rows
    }

    /// 居中一行
    ///
    /// 宽度必须按**显示宽度**算：`String::len()` 是字节数，中文一个字占 3 字节，
    /// 用它做 `pad` 会让每条中文提示行都往左偏（副标题实测偏 6 列）。这里复用
    /// 项目里已有的 `TextLayout::measure`，与对话区、审批面板同一套度量。
    fn center(&self, segs: Vec<Seg>, width: usize) -> Vec<Seg> {
        let text_width: usize = segs.iter().map(|s| TextLayout::measure(&s.text)).sum();
        if text_width >= width {
            return segs;
        }
        let pad = (width - text_width) / 2;
        let mut result = vec![Seg::raw(" ".repeat(pad))];
        result.extend(segs);
        result
    }

    /// 星星或空白
    fn star_or_blank(&self, x: usize, y: usize, left: usize, right: usize) -> Seg {
        if x >= left && x < right {
            star_seg(x, y, self.tick, 0.55 * self.fade_in(), STAR_SPARSITY)
        } else {
            Seg::raw(" ")
        }
    }
}

impl Default for WelcomeScene {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    /// 把一行片段拼回纯文本，便于断言渲染内容
    fn row_text(row: &[Seg]) -> String {
        row.iter().map(|seg| seg.text.as_str()).collect()
    }

    fn rendered_lines(
        scene: &WelcomeScene,
        cols: usize,
        rows: usize,
        checklist: &[String],
    ) -> Vec<String> {
        scene
            .render_with_checklist(cols, rows, checklist)
            .iter()
            .map(|row| row_text(row))
            .collect()
    }

    /// 与 `yunxi-agent-linux/src/main.rs::claim_first_run_checklist` 同构的清单
    /// （含标记列、无标记项与收尾行）
    fn demo_checklist() -> Vec<String> {
        vec![
            "  工作区                 ✓ /tmp/yunxi".to_string(),
            "  YunXi 状态目录         ✓ /home/yunxi/.local/state/yunxi".to_string(),
            "  Provider / 模型        ✓ deepseek / static".to_string(),
            "  会话与记忆目录         ✓ 已初始化".to_string(),
            "  默认知识库             - 未配置，稍后可接入".to_string(),
            "  完成。直接输入目标即可开始，/help 查看帮助".to_string(),
        ]
    }

    #[test]
    fn welcome_scene_creates() {
        let scene = WelcomeScene::new();
        assert_eq!(scene.tick, 0);
        assert_eq!(scene.born, 0);
    }

    #[test]
    fn welcome_scene_ticks() {
        let mut scene = WelcomeScene::new();
        scene.tick();
        assert_eq!(scene.tick, 1);
        assert_eq!(scene.born, 1);
    }

    #[test]
    fn fade_in_reaches_full() {
        let mut scene = WelcomeScene::new();
        for _ in 0..30 {
            scene.tick();
        }
        assert_eq!(scene.fade_in(), 1.0);
    }

    #[test]
    fn render_produces_output() {
        let scene = WelcomeScene::new();
        let output = scene.render(80, 24);
        assert_eq!(output.len(), 24);
    }

    #[test]
    fn plain_render_is_unchanged_by_the_checklist_entry_points() {
        let scene = WelcomeScene::new();
        let plain = scene.render(100, 30);
        assert_eq!(plain, scene.render_with_checklist(100, 30, &[]));
        assert_eq!(plain, scene.render_with_optional_checklist(100, 30, None));
        assert_eq!(
            plain,
            scene.render_with_optional_checklist(100, 30, Some(&[]))
        );
    }

    #[test]
    fn hint_row_advertises_the_real_status_command() {
        // `/config` 从未在 `yunxi-agent-linux/src/main.rs` 注册：欢迎界面不能把
        // 不存在的命令当作设置入口推荐给用户。
        let scene = WelcomeScene::new();
        let lines = rendered_lines(&scene, 100, 30, &[]);
        let text = lines.join("\n");
        assert!(text.contains("/help"));
        assert!(text.contains("/status"));
        assert!(!text.contains("/config"));

        let hint = lines
            .iter()
            .find(|line| line.contains("/help"))
            .expect("hint row");
        assert_eq!(hint.trim(), "/help 了解更多  /status 查看设置");
    }

    #[test]
    fn checklist_is_appended_below_the_animation() {
        let mut scene = WelcomeScene::new();
        for _ in 0..30 {
            scene.tick();
        }
        let output = scene.render_with_checklist(100, 30, &demo_checklist());
        assert_eq!(output.len(), 30);

        let lines: Vec<String> = output.iter().map(|row| row_text(row)).collect();
        let text = lines.join("\n");
        assert!(text.contains("首次启动检查"));
        assert!(text.contains("/tmp/yunxi"));
        assert!(text.contains("deepseek / static"));
        assert!(text.contains("完成。直接输入目标即可开始"));

        let position = |needle: &str| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("missing row: {needle}"))
        };
        // 艺术字已改成无框块字，不再有 "云  熙" 这行；改认副标题。
        let banner = position("接管终端交互的陪伴型 Agent");
        let hint = position("/help");
        let title = position("首次启动检查");
        let completion = position("完成。");
        assert!(banner < hint && hint < title && title < completion);
    }

    #[test]
    fn checklist_markers_keep_their_aligned_column() {
        let scene = WelcomeScene::new();
        let lines = rendered_lines(&scene, 100, 30, &demo_checklist());
        let columns: Vec<usize> = lines
            .iter()
            .filter_map(|line| {
                line.find('✓')
                    .or_else(|| line.find("- 未"))
                    .map(|offset| UnicodeWidthStr::width(&line[..offset]))
            })
            .collect();
        assert_eq!(columns.len(), 5, "应渲染 5 个带标记的条目：{lines:?}");
        assert!(
            columns.windows(2).all(|pair| pair[0] == pair[1]),
            "标记列必须对齐：{columns:?}"
        );
    }

    #[test]
    fn short_terminal_keeps_the_banner_and_clips_the_checklist() {
        let scene = WelcomeScene::new();
        let checklist = demo_checklist();

        // 艺术字(6) + 提示行(7) 之后只剩 4 行：清单尾部（收尾行）先被截断
        let clipped: Vec<String> = scene
            .render_with_checklist(100, 17, &checklist)
            .iter()
            .map(|row| row_text(row))
            .collect();
        assert_eq!(clipped.len(), 17);
        let text = clipped.join("\n");
        assert!(text.contains("接管终端交互的陪伴型 Agent"));
        assert!(text.contains("首次启动检查"));
        assert!(!text.contains("完成。"));

        // 连标题都放不下时整个清单让位，动画界面不能消失
        let tiny: Vec<String> = scene
            .render_with_checklist(100, 13, &checklist)
            .iter()
            .map(|row| row_text(row))
            .collect();
        assert_eq!(tiny.len(), 13);
        let text = tiny.join("\n");
        assert!(text.contains("接管终端交互的陪伴型 Agent"));
        assert!(text.contains("/help"));
        assert!(!text.contains("首次启动检查"));
    }
}
