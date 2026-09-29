//! YunXi 终端色板与色深降级。
//!
//! **来源与归属**：本模块的机制（四档色深降级、`to_256`/`to_16` 近似、字符集
//! 兜底、`Theme` 的 `fg`/`dim`/`select`/`lerp`/`lift` 接口）移植自
//! `SHORiN-KiWATA/miyu-agent`（MIT License, Copyright (c) 2026 SHORiN-KiWATA）
//! 的 `crates/miyu-base/src/terminal/palette.rs`，快照见本仓库
//! `references/miyu-agent/`（pin `04a23ccb`）。
//!
//! 按本仓库 `references/REFERENCE-SOURCES.md` 的要求：保留归属与许可证声明、
//! 经 YunXi 适配层落地、并带行为测试。**配色不同**：YunXi 用的是用户原创角色
//! 设定图里提取的 240° 蓝紫色阶，不是 Miyu 的雾蓝/酒红/暖金。
//!
//! 设计原则（承袭 Miyu）：一个颜色只写一次 RGB，落地时按终端能力分四档。
//! 降级不是「颜色变少」，是**换一种手段表达同一件事** —— 16 色以下选中态改反显。

use ratatui::style::{Color, Modifier, Style};

/// 终端色深。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Depth {
    /// 24 位真彩。渐变才有意义。
    True,
    /// xterm-256。渐变仍然能看，靠 6×6×6 色立方近似。
    X256,
    /// 只有 16 个基础色。渐变塌成单色，底色高亮换成反显。
    Ansi16,
    /// 完全不上色。只剩粗体 / 暗淡 / 反显。
    Mono,
}

impl Depth {
    pub fn detect() -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            return Depth::Mono;
        }
        if let Ok(value) = std::env::var("YUNXI_COLOR") {
            return match value.trim().to_ascii_lowercase().as_str() {
                "truecolor" | "24bit" | "true" => Depth::True,
                "256" | "xterm256" => Depth::X256,
                "16" | "ansi" => Depth::Ansi16,
                "none" | "mono" | "0" => Depth::Mono,
                _ => Depth::X256,
            };
        }
        let colorterm = std::env::var("COLORTERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return Depth::True;
        }
        let term = std::env::var("TERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if term.is_empty() || term == "dumb" {
            return Depth::Mono;
        }
        // kitty / wezterm / 新 alacritty 即便没设 COLORTERM 也是真彩。
        if term.contains("kitty") || term.contains("wezterm") || term.contains("alacritty") {
            return Depth::True;
        }
        if term.contains("256color") || term.contains("direct") {
            return Depth::X256;
        }
        Depth::Ansi16
    }

    /// 渐变值不值得画。16 色以下画出来是一串跳变的色块，不如单色干净。
    pub fn gradient_ok(self) -> bool {
        matches!(self, Depth::True | Depth::X256)
    }
}

/// 设计稿里的颜色一律写成 RGB，落地时才降级。
pub type Rgb = (u8, u8, u8);

// ── 色板：取自用户原创角色设定图，色相统一在 H=240°（蓝紫）──
//
// 角色的色阶是从「衣物冷白高光」到「靴子近黑蓝」的一条明度梯，没有第二色相。
// 所以这里不是「多种颜色」，而是同一条梯子上的几档 —— 层级靠明度表达。

/// 冷白高光（角色衣物高光 `#d8d8f0` 提亮一点）。用于最需要跳出来的东西。
pub const LIGHT: Rgb = (0xdc, 0xdc, 0xf2);
/// 银灰蓝（角色衣物中间调 `#a8a8c0`）。主体内容。
pub const MID: Rgb = (0xa8, 0xa8, 0xc4);
/// 强调紫蓝（取自角色眼瞳与发丝高光）。焦点、选中、正在发生的事。
pub const PRIMARY: Rgb = (0x85, 0x85, 0xd6);
/// 暗蓝灰（元信息：工具名、token、页脚）。
pub const DIM: Rgb = (0x6a, 0x6a, 0x88);
/// 更暗一档，给按键条的说明文字。
pub const FAINT: Rgb = (0x4a, 0x4a, 0x62);
/// 底色贴近的暗蓝黑（角色发根/靴子 `#181830`）。淡入的起点色。
pub const INK: Rgb = (0x18, 0x18, 0x30);
/// 选中行底色。
pub const SEL_BG: Rgb = (0x2a, 0x2a, 0x48);

// ── Miyu 的标识符名（照搬时保留，好让移植过来的文件逐字编译）──
//
// 全量照搬 `chrome.rs` / `starfield.rs` / `oobe` 时，这些名字出现在几百处。
// 保留名字、只换常量值，移植就是机械的复制 + 改导入，不用逐处改写语义。
// 值全部来自用户原创角色的 240° 蓝紫色阶，不是 Miyu 的雾蓝/酒红/暖金。

/// Miyu 的 `BLUE`（primary / 瞳色雾蓝）→ 角色的强调紫蓝。
pub const BLUE: Rgb = PRIMARY;
/// Miyu 的 `CORAL`（tertiary / 丝带酒红）→ 角色色阶的亮档（渐变终点）。
pub const CORAL: Rgb = LIGHT;
/// Miyu 的 `GOLD`（secondary / 发色暖金）→ 角色的强调色。
pub const GOLD: Rgb = PRIMARY;
/// Miyu 的 `GREEN` → 角色色阶的中间调（YunXi 不用第二色相）。
pub const GREEN: Rgb = MID;

/// xterm-256 的 16 个基础色近似值，用来找最近色。
const ANSI16: [Rgb; 16] = [
    (0, 0, 0),
    (170, 0, 0),
    (0, 170, 0),
    (170, 85, 0),
    (0, 0, 170),
    (170, 0, 170),
    (0, 170, 170),
    (170, 170, 170),
    (85, 85, 85),
    (255, 85, 85),
    (85, 255, 85),
    (255, 255, 85),
    (85, 85, 255),
    (255, 85, 255),
    (85, 255, 255),
    (255, 255, 255),
];

/// RGB → xterm-256 索引。6×6×6 色立方 + 24 级灰阶，标准近似法。
fn to_256(color: Rgb) -> u8 {
    let (r, g, b) = color;
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return 232 + ((u16::from(r) - 8) * 24 / 247) as u8;
    }
    let quantize = |value: u8| -> u16 {
        if value < 48 {
            0
        } else if value < 115 {
            1
        } else {
            (u16::from(value).saturating_sub(35)) / 40
        }
    };
    (16 + 36 * quantize(r) + 6 * quantize(g) + quantize(b)).min(255) as u8
}

/// RGB → 16 色里最近的一个。欧氏距离足够，不必上 CIELAB。
fn to_16(color: Rgb) -> u8 {
    let (r, g, b) = (i32::from(color.0), i32::from(color.1), i32::from(color.2));
    let mut best = 7u8;
    let mut best_distance = i32::MAX;
    for (index, candidate) in ANSI16.iter().enumerate() {
        let distance = (r - i32::from(candidate.0)).pow(2)
            + (g - i32::from(candidate.1)).pow(2)
            + (b - i32::from(candidate.2)).pow(2);
        if distance < best_distance {
            best_distance = distance;
            best = index as u8;
        }
    }
    best
}

/// 色深 + 字符集，两根正交的轴：有的终端色彩很好但字体缺 Unicode，反过来也有。
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub depth: Depth,
    pub ascii: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self::detect()
    }
}

impl Theme {
    pub fn detect() -> Self {
        let ascii = std::env::var("YUNXI_ASCII")
            .map(|value| value != "0" && !value.is_empty())
            .unwrap_or(false);
        Self {
            depth: Depth::detect(),
            ascii,
        }
    }

    /// 前景色。`Mono` 档一律不上色，交给 modifier 拉开层次。
    pub fn fg(self, color: Rgb) -> Style {
        match self.depth {
            Depth::True => Style::new().fg(Color::Rgb(color.0, color.1, color.2)),
            Depth::X256 => Style::new().fg(Color::Indexed(to_256(color))),
            Depth::Ansi16 => Style::new().fg(Color::Indexed(to_16(color))),
            Depth::Mono => Style::new(),
        }
    }

    /// 同 [`Theme::fg`]，给直接写 ANSI 的地方用（不走 ratatui 的输出）：
    /// 两边按同一个色深降级，同一个颜色才画得一样。
    pub fn fg_ansi(self, color: Rgb) -> String {
        match self.depth {
            Depth::True => format!("\x1b[38;2;{};{};{}m", color.0, color.1, color.2),
            Depth::X256 => format!("\x1b[38;5;{}m", to_256(color)),
            Depth::Ansi16 => format!("\x1b[38;5;{}m", to_16(color)),
            Depth::Mono => String::new(),
        }
    }

    /// 暗淡。低色深下没有「更暗的灰」可用，退回 DIM modifier。
    pub fn dim(self, color: Rgb) -> Style {
        match self.depth {
            Depth::True | Depth::X256 => self.fg(color),
            _ => Style::new().add_modifier(Modifier::DIM),
        }
    }

    /// 选中行的底色。16 色以下没有像样的深底可用，**换成反显**——
    /// 这是终端里最稳的「选中」表达，一路退到 vt100 都有。
    pub fn select(self, style: Style) -> Style {
        match self.depth {
            Depth::True => style.bg(Color::Rgb(SEL_BG.0, SEL_BG.1, SEL_BG.2)),
            Depth::X256 => style.bg(Color::Indexed(to_256(SEL_BG))),
            _ => style.add_modifier(Modifier::REVERSED),
        }
    }

    /// 两色之间取插值；不支持渐变的档位直接返回起点色。
    pub fn lerp(self, from: Rgb, to: Rgb, t: f32) -> Style {
        if !self.depth.gradient_ok() {
            return self.fg(from);
        }
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        self.fg((mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2)))
    }

    /// 往白里提亮，用来做扫光。
    pub fn lift(self, color: Rgb, t: f32) -> Style {
        self.lerp(color, (255, 255, 255), t)
    }

    // ── 字符集 ──
    pub fn dot_done(self) -> &'static str {
        if self.ascii { "*" } else { "●" }
    }
    pub fn dot_here(self) -> &'static str {
        if self.ascii { "@" } else { "◉" }
    }
    pub fn dot_todo(self) -> &'static str {
        if self.ascii { "-" } else { "○" }
    }
    pub fn cursor(self) -> &'static str {
        if self.ascii { "> " } else { "▸ " }
    }
    pub fn radio_on(self) -> &'static str {
        if self.ascii { "(o)" } else { "●" }
    }
    pub fn radio_off(self) -> &'static str {
        if self.ascii { "( )" } else { "○" }
    }
    pub fn hline(self) -> &'static str {
        if self.ascii { "-" } else { "─" }
    }
    pub fn arrow(self) -> &'static str {
        if self.ascii { ">" } else { "›" }
    }
    pub fn check(self) -> &'static str {
        if self.ascii { "+" } else { "✓" }
    }
    pub fn spinner(self, frame: usize) -> &'static str {
        const UNICODE: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];
        const ASCII: [&str; 4] = ["|", "/", "-", "\\"];
        if self.ascii {
            ASCII[frame % 4]
        } else {
            UNICODE[frame % 4]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantization_hits_expected_cube_cells() {
        // 纯白落在色立方的顶角，纯黑落在 16。
        assert_eq!(to_256((255, 255, 255)), 231);
        assert_eq!(to_256((0, 0, 0)), 16);
        // 灰阶走 232..=255 那条梯子。
        assert!((232..=255).contains(&to_256((128, 128, 128))));
        // 角色的强调紫蓝在 16 色里最近的是亮蓝/亮白一带，绝不会掉到黑。
        assert_ne!(to_16(PRIMARY), 0);
        // 角色色阶的每一档都能落到 16 色里的非黑项 —— 否则 Mono 之外的
        // 低色终端会把主体内容画成黑字黑底。
        for step in [LIGHT, MID, PRIMARY, DIM] {
            assert_ne!(to_16(step), 0, "色阶 {step:?} 掉进黑色了");
        }
    }

    #[test]
    fn low_depth_never_emits_rgb() {
        let mono = Theme {
            depth: Depth::Mono,
            ascii: false,
        };
        assert_eq!(mono.fg(PRIMARY), Style::new());
        assert_eq!(mono.lerp((0, 0, 0), (255, 255, 255), 0.5), Style::new());
        let sixteen = Theme {
            depth: Depth::Ansi16,
            ascii: false,
        };
        assert_eq!(
            sixteen.select(Style::new()),
            Style::new().add_modifier(Modifier::REVERSED)
        );
    }

    #[test]
    fn mono_depth_falls_back_to_modifiers_not_colour() {
        // 降级不是「颜色变少」，是换一种手段表达同一件事：Mono 档下
        // 暗淡必须变成 DIM modifier，否则元信息与正文分不开。
        let mono = Theme {
            depth: Depth::Mono,
            ascii: false,
        };
        assert_eq!(
            mono.dim(DIM),
            Style::new().add_modifier(Modifier::DIM),
            "Mono 档的 dim 必须退回 DIM modifier"
        );
        // 真彩档才真的给颜色。
        let t = Theme {
            depth: Depth::True,
            ascii: false,
        };
        assert_eq!(t.dim(DIM), t.fg(DIM));
    }

    #[test]
    fn ascii_mode_never_emits_wide_glyphs() {
        let a = Theme {
            depth: Depth::True,
            ascii: true,
        };
        for glyph in [
            a.dot_done(),
            a.dot_here(),
            a.dot_todo(),
            a.cursor(),
            a.radio_on(),
            a.radio_off(),
            a.hline(),
            a.arrow(),
            a.check(),
            a.spinner(0),
        ] {
            assert!(glyph.is_ascii(), "ASCII 档吐出了非 ASCII 字形：{glyph:?}");
        }
    }
}
