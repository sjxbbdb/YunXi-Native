//! 云熙星空与渐变艺术字
//!
//! YunXi 独立实现的视觉动效系统。
//!
//! 设计原则：
//! - 星星用尺寸变化（· → ⋆ → ✦ → ✧），每颗独立闪烁
//! - 温暖色调（浅棕、杏色、米色）体现滋补品的温润感
//! - 渐变从浅棕到杏色
//! - 40ms 一帧的流畅动画

use ratatui::style::{Color, Style};

/// 云熙角色配色系统（从角色设定图提取）
pub type Rgb = (u8, u8, u8);

pub const YUNXI_SILVER: Rgb = (0xE0, 0xE0, 0xE8); // 发色：银白带蓝紫调
pub const YUNXI_PURPLE: Rgb = (0xC8, 0xB0, 0xD8); // 主色：柔和紫色
pub const YUNXI_LAVENDER: Rgb = (0xD8, 0xC8, 0xE0); // 浅紫/薰衣草
pub const YUNXI_WHITE: Rgb = (0xF0, 0xF0, 0xF4); // 服饰白色
pub const YUNXI_PINK: Rgb = (0xE0, 0xC0, 0xD0); // 紫粉色
pub const YUNXI_TEXT: Rgb = (0x2C, 0x2C, 0x2C); // 主文字
pub const YUNXI_DIM: Rgb = (0x88, 0x88, 0x88); // 次要文字
pub const YUNXI_INK: Rgb = (0x18, 0x18, 0x18); // 深色背景

/// 文本片段（带样式）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seg {
    pub text: String,
    pub style: Style,
}

impl Seg {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }

    pub fn raw(text: impl Into<String>) -> Self {
        Self::new(text, Style::new())
    }
}

/// 云熙艺术字（温柔圆润风格）
pub const YUNXI_BANNER_UNICODE: [&str; 6] = [
    "        🌸                    ",
    "   ╭────────────────────╮    ",
    "   │    云  熙  在  此    │   ",
    "   │  YunXi · Companion  │   ",
    "   ╰────────────────────╯    ",
    "                              ",
];

/// ASCII 兜底版本
pub const YUNXI_BANNER_ASCII: [&str; 5] = [
    "  __   __            __  __ ",
    "  \\ \\ / /   _ _ __   \\ \\/ / ",
    "   \\ V / | | | '_ \\   \\  /  ",
    "    | || |_| | | | |  /  \\  ",
    "    |_| \\__,_|_| |_| /_/\\_\\ ",
];

/// 艺术字定义
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BannerArt {
    pub lines: Vec<String>,
    pub subtitle: String,
}

impl BannerArt {
    /// 创建云熙的内置艺术字
    pub fn yunxi_builtin(ascii: bool) -> Self {
        let lines: &[&str] = if ascii {
            &YUNXI_BANNER_ASCII
        } else {
            &YUNXI_BANNER_UNICODE
        };

        Self {
            lines: lines.iter().map(|s| s.to_string()).collect(),
            subtitle: "陪伴型终端助手 · Arch Linux".to_string(),
        }
    }

    pub fn cols(&self) -> usize {
        self.lines.iter().map(|line| line.len()).max().unwrap_or(0)
    }

    pub fn rows(&self) -> usize {
        self.lines.len()
    }
}

// ─────────────────── 星空动画 ───────────────────

/// 2D 哈希函数（用于星空随机分布）
pub fn hash2(x: u32, y: u32, seed: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^ (h >> 13)
}

/// 星星的形状阶梯（温暖风格）
const STAR_UNICODE: [&str; 8] = ["·", "⋆", "✦", "✧", "✦", "⋆", "·", " "];
const STAR_ASCII: [&str; 8] = [".", "+", "*", "#", "*", "+", ".", " "];

/// 计算某一格此刻的星星状态
///
/// 返回 (字形, 亮度) 或 None（无星）
pub fn star_at(
    x: u32,
    y: u32,
    frame: u32,
    ascii: bool,
    sparsity: u32,
) -> Option<(&'static str, f32)> {
    // 稀疏度：越大越稀
    if hash2(x, y, 3) % sparsity.max(1) != 0 {
        return None;
    }

    // 每颗星独立的速度和相位
    let speed = 2 + hash2(x, y, 11) % 3; // 2~4 帧/档
    let offset = hash2(x, y, 9) % 24; // 相位偏移
    let stage = (((frame + offset) / speed) % 8) as usize;

    let glyph = if ascii {
        STAR_ASCII[stage]
    } else {
        STAR_UNICODE[stage]
    };

    if glyph == " " {
        return None;
    }

    // 亮度跟随尺寸
    let bright = [0.15, 0.4, 0.7, 1.0, 0.7, 0.4, 0.15, 0.0][stage];
    Some((glyph, bright))
}

/// 生成一格星星的片段
pub fn star_seg(x: usize, y: usize, frame: usize, scale: f32, sparsity: u32) -> Seg {
    match star_at(x as u32, y as u32, frame as u32, false, sparsity) {
        Some((glyph, bright)) => {
            // 云熙配色轮换：银白、紫色、薰衣草
            let color = if x % 3 == 0 {
                YUNXI_SILVER // 银白色
            } else if x % 3 == 1 {
                YUNXI_PURPLE // 柔和紫色
            } else {
                YUNXI_LAVENDER // 薰衣草
            };

            let final_bright = (bright * scale).clamp(0.0, 1.0);
            Seg::new(glyph, lerp_color(YUNXI_INK, color, final_bright))
        }
        None => Seg::raw(" "),
    }
}

// ─────────────────── 颜色工具 ───────────────────

/// 颜色插值
pub fn lerp_color(from: Rgb, to: Rgb, t: f32) -> Style {
    let t = t.clamp(0.0, 1.0);
    let r = (from.0 as f32 + (to.0 as f32 - from.0 as f32) * t) as u8;
    let g = (from.1 as f32 + (to.1 as f32 - from.1 as f32) * t) as u8;
    let b = (from.2 as f32 + (to.2 as f32 - from.2 as f32) * t) as u8;
    Style::default().fg(Color::Rgb(r, g, b))
}

/// 渐变位置计算（斜向：左上到右下）
pub fn gradient_t(x: usize, y: usize, cols: usize, rows: usize) -> f32 {
    let x_t = x as f32 / cols.max(1) as f32;
    let y_t = y as f32 / rows.max(1) as f32;
    // 横向权重 > 纵向（因为艺术字是扁的）
    x_t * 0.7 + y_t * 0.3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_art_has_valid_dimensions() {
        let art = BannerArt::yunxi_builtin(false);
        assert!(art.cols() > 0);
        assert!(art.rows() > 0);
        assert!(!art.subtitle.is_empty());
    }

    #[test]
    fn star_hash_is_deterministic() {
        let h1 = hash2(10, 20, 0);
        let h2 = hash2(10, 20, 0);
        assert_eq!(h1, h2);
    }

    #[test]
    fn star_cycles_through_stages() {
        let mut seen_glyphs = Vec::new();
        for frame in 0..100 {
            if let Some((glyph, _)) = star_at(5, 5, frame, false, 1) {
                seen_glyphs.push(glyph);
            }
        }
        // 应该看到多种形状
        assert!(seen_glyphs.len() > 10);
    }
}
