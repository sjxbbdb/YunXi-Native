//! 云熙欢迎界面 - 空会话的大厅模式
//!
//! 设计理念：
//! - 空会话显示居中的动画欢迎界面
//! - 星空背景 + 艺术字 + 提示信息
//! - 第一条消息发送后，欢迎界面撤走，进入对话模式
//! - 40ms 一帧的流畅动画

use crate::yunxi_starfield::{
    BannerArt, Seg, YUNXI_INK, YUNXI_LAVENDER, YUNXI_PURPLE, YUNXI_SILVER, gradient_t, lerp_color,
    star_seg,
};
use ratatui::style::Modifier;

/// 星空密度（值越大越稀）
const STAR_SPARSITY: u32 = 9;
/// 星域比艺术字大一圈（左右各这么多列）
const STAR_PAD_X: usize = 14;

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
    pub fn render(&self, cols: usize, rows: usize) -> Vec<Vec<Seg>> {
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
        let block_rows = art_rows + hint_rows.len();

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
                // 提示行
                out.push(hint_rows[y - top - art_rows].clone());
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
            self.center(vec![Seg::new("直接说出你的想法就好", dim_style)], cols),
            self.center(vec![Seg::new("不需要记住任何命令", dim_style)], cols),
            Vec::new(), // 空行
            self.center(
                vec![
                    Seg::new("/help", highlight_style),
                    Seg::new(" 了解更多  ", dim_style),
                    Seg::new("/config", highlight_style),
                    Seg::new(" 设置", dim_style),
                ],
                cols,
            ),
        ]
    }

    /// 居中一行
    fn center(&self, segs: Vec<Seg>, width: usize) -> Vec<Seg> {
        let text_width: usize = segs.iter().map(|s| s.text.len()).sum();
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
}
