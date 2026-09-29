use ratatui::style::{Color, Modifier, Style, Stylize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TuiColorCapability {
    Full,
    Ansi16,
    Monochrome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TuiSemanticStyle {
    User,
    Assistant,
    Progress,
    Tool,
    ActionRequired,
    Notice,
    Warning,
    Error,
    Muted,
    Header,
    Subheader,
    Footer,
    Border,
    Focus,
    Success,
    Selection,
}

impl TuiSemanticStyle {
    #[cfg(test)]
    const ALL: [Self; 16] = [
        Self::User,
        Self::Assistant,
        Self::Progress,
        Self::Tool,
        Self::ActionRequired,
        Self::Notice,
        Self::Warning,
        Self::Error,
        Self::Muted,
        Self::Header,
        Self::Subheader,
        Self::Footer,
        Self::Border,
        Self::Focus,
        Self::Success,
        Self::Selection,
    ];
}

pub(crate) type TuiStyle = Style;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TuiStyleSet {
    capability: TuiColorCapability,
}

impl TuiStyleSet {
    pub(crate) const fn new(capability: TuiColorCapability) -> Self {
        Self { capability }
    }

    pub(crate) fn detect() -> Self {
        if std::env::var_os("NO_COLOR").is_some()
            || std::env::var("TERM").is_ok_and(|term| term.eq_ignore_ascii_case("dumb"))
        {
            return Self::new(TuiColorCapability::Monochrome);
        }

        let rich_terminal = std::env::var("COLORTERM").is_ok_and(|value| !value.is_empty())
            || std::env::var("TERM").is_ok_and(|term| {
                let term = term.to_ascii_lowercase();
                term.contains("256color") || term.contains("truecolor")
            });
        Self::new(if rich_terminal {
            TuiColorCapability::Full
        } else {
            TuiColorCapability::Ansi16
        })
    }

    pub(crate) fn style(self, semantic: TuiSemanticStyle) -> TuiStyle {
        match self.capability {
            TuiColorCapability::Full => rich_style(semantic),
            TuiColorCapability::Ansi16 => ansi16_style(semantic),
            TuiColorCapability::Monochrome => monochrome_style(semantic),
        }
    }
}

/// 真彩路径：**单色相 + 三档明度**。
///
/// 依据 `yunxi-TUI视觉重设计.md`（用户已确认）：
/// - 色相全部锁在 H=240°（蓝紫），取自用户的角色设定图；
///   旧表用了 Cyan / Green / Magenta / Yellow / Red 等 **9 种色相**，
///   与角色气质冲突，已整体作废。
/// - **错误不用红色**：改用「最亮 + 反白」，靠亮度跳出来而不是靠色相。
/// - 层级只用明度表达：BRIGHT 是焦点（用户、当前输入），MID 是主体（云熙的回复），
///   DIM 是元信息（工具名、token、进度）。
/// 角色配色的三个明度档（与 `yunxi_starfield` 的 YUNXI_* 同源，H=240°）。
const BRIGHT: Color = Color::Rgb(0xDC, 0xDC, 0xF2);
const MID: Color = Color::Rgb(0xA8, 0xA8, 0xC4);
const DIM: Color = Color::Rgb(0x6A, 0x6A, 0x88);
const ACCENT: Color = Color::Rgb(0x85, 0x85, 0xD6);

fn rich_style(semantic: TuiSemanticStyle) -> TuiStyle {
    use TuiSemanticStyle as S;
    match semantic {
        // 用户说的：最亮 + 粗体，是画面上唯一的「焦点」
        S::User => Style::default().fg(BRIGHT).bold(),
        // 云熙说的：主体内容，中间调
        S::Assistant => Style::default().fg(MID),
        // 元信息：工具、进度、页脚，全部压暗
        S::Progress | S::Tool | S::Notice => Style::default().fg(DIM),
        // 需要动作：最亮 + 粗体 + 下划线（唯一的强调装饰）
        S::ActionRequired => Style::default().fg(BRIGHT).bold().underlined(),
        // 警告 / 错误：**不用黄红**，靠亮度与反白区分
        S::Warning => Style::default().fg(BRIGHT).bold(),
        S::Error => Style::default().fg(BRIGHT).reversed().bold(),
        S::Muted | S::Footer | S::Border => Style::default().fg(DIM),
        S::Header => Style::default().fg(BRIGHT).bold(),
        S::Subheader => Style::default().fg(MID),
        S::Focus => Style::default().fg(ACCENT).bold(),
        S::Success => Style::default().fg(MID),
        S::Selection => Style::default().reversed().bold(),
    }
}

/// 16 色路径：同上，但降到 ANSI 的亮/中/暗三档。
///
/// **不允许退回彩色**：`BrightWhite` / `White` / `DarkGray` 是同一条灰阶，
/// 换成 `Red`/`Yellow` 就等于偷偷把旧设计带回来。
fn ansi16_style(semantic: TuiSemanticStyle) -> TuiStyle {
    use TuiSemanticStyle as S;
    match semantic {
        S::User => Style::default().fg(Color::White).bold(),
        S::Assistant => Style::default().fg(Color::White),
        S::Progress | S::Tool | S::Notice => Style::default().fg(Color::DarkGray),
        S::ActionRequired => Style::default().fg(Color::White).bold().underlined(),
        S::Warning => Style::default().fg(Color::White).bold(),
        S::Error => Style::default().fg(Color::White).reversed().bold(),
        S::Muted | S::Footer | S::Border => Style::default().fg(Color::DarkGray),
        S::Header => Style::default().fg(Color::White).bold(),
        S::Subheader => Style::default().fg(Color::White),
        S::Focus | S::Selection => Style::default().reversed().bold(),
        S::Success => Style::default().fg(Color::White),
    }
}

/// 单色路径（无彩色终端）：**语义必须靠修饰符存活**。
///
/// 这是新设计最容易翻车的地方：彩色路径靠明度三档分层，一旦没有颜色，
/// 「云熙说的话」与「工具名」就都变成普通文字、分不出来了。
/// 所以这里把三档明度映射成三档修饰符：
///
/// | 明度档 | 语义 | 单色表达 |
/// |---|---|---|
/// | BRIGHT | 用户 / 警告 / 表头 | **粗体** |
/// | —— | 云熙的回复 | 普通 |
/// | DIM | 工具 / 进度 / 通知 / 页脚 | 变暗 |
/// | 反白 | 错误 | **粗体 + 反白** |
///
/// 修复了一个旧设计的遗留：`Tool` 原来落在「粗体」那一档，因为旧设计用品红
/// 标工具；新设计里工具是**元信息**，必须归到 DIM，否则单色下工具行比云熙的
/// 回复还显眼。
fn monochrome_style(semantic: TuiSemanticStyle) -> TuiStyle {
    use TuiSemanticStyle as S;
    match semantic {
        // 元信息：压暗
        S::Muted | S::Subheader | S::Footer | S::Border => {
            Style::default().add_modifier(Modifier::DIM)
        }
        S::Progress | S::Tool | S::Notice => Style::default().add_modifier(Modifier::DIM),
        // 焦点：粗体
        S::Header | S::Focus | S::User | S::Success => Style::default().bold(),
        // 需要动作：粗体 + 下划线
        S::ActionRequired => Style::default().bold().underlined(),
        // 警告：粗体；错误：粗体 + 反白 —— 两者仍能区分
        S::Warning => Style::default().bold(),
        S::Error => Style::default().bold().reversed(),
        // 主体内容：不着修饰
        S::Assistant => Style::default(),
        S::Selection => Style::default().reversed().bold(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_semantic_style_is_stable_in_each_color_capability() {
        for capability in [
            TuiColorCapability::Full,
            TuiColorCapability::Ansi16,
            TuiColorCapability::Monochrome,
        ] {
            let styles = TuiStyleSet::new(capability);
            for semantic in TuiSemanticStyle::ALL {
                assert_eq!(styles.style(semantic), styles.style(semantic));
            }
        }
    }

    #[test]
    fn monochrome_keeps_important_states_distinct_without_color() {
        let styles = TuiStyleSet::new(TuiColorCapability::Monochrome);
        for semantic in [
            TuiSemanticStyle::ActionRequired,
            TuiSemanticStyle::Warning,
            TuiSemanticStyle::Error,
            TuiSemanticStyle::Focus,
        ] {
            let style = styles.style(semantic);
            assert_eq!(style.fg, None);
            assert_eq!(style.bg, None);
            assert!(style.add_modifier.contains(Modifier::BOLD));
        }
        assert!(
            styles
                .style(TuiSemanticStyle::Selection)
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn palettes_stay_on_one_hue_and_drop_the_traffic_light_colors() {
        // 新设计的核心不变量（见 `yunxi-TUI视觉重设计.md`，用户已确认）：
        //   1. **错误不用红、警告不用黄** —— 改由亮度与反白表达
        //   2. 整体单色相 —— 不再出现绿/青/品红等色相色
        //   3. ansi16 路径仍然只走灰阶，不引入 RGB
        let rich = TuiStyleSet::new(TuiColorCapability::Full);
        let ansi = TuiStyleSet::new(TuiColorCapability::Ansi16);

        let banned = [
            Color::Red,
            Color::LightRed,
            Color::Yellow,
            Color::LightYellow,
            Color::Green,
            Color::LightGreen,
            Color::Magenta,
            Color::LightMagenta,
            Color::Cyan,
            Color::LightCyan,
            Color::Blue,
            Color::LightBlue,
        ];
        let all = [
            TuiSemanticStyle::User,
            TuiSemanticStyle::Assistant,
            TuiSemanticStyle::Progress,
            TuiSemanticStyle::Tool,
            TuiSemanticStyle::ActionRequired,
            TuiSemanticStyle::Notice,
            TuiSemanticStyle::Warning,
            TuiSemanticStyle::Error,
            TuiSemanticStyle::Muted,
            TuiSemanticStyle::Header,
            TuiSemanticStyle::Subheader,
            TuiSemanticStyle::Footer,
            TuiSemanticStyle::Border,
            TuiSemanticStyle::Focus,
            TuiSemanticStyle::Success,
            TuiSemanticStyle::Selection,
        ];
        for semantic in all {
            for (label, style) in [
                ("rich", rich.style(semantic)),
                ("ansi", ansi.style(semantic)),
            ] {
                if let Some(fg) = style.fg {
                    assert!(
                        !banned.contains(&fg),
                        "{label} 路径的 {semantic:?} 用了色相色 {fg:?}；新设计只用明度档"
                    );
                }
            }
        }

        // ansi16 只允许灰阶 —— 低色终端上 RGB 会被降级成近似色，不如直接给灰阶。
        for semantic in all {
            if let Some(fg) = ansi.style(semantic).fg {
                assert!(
                    matches!(fg, Color::White | Color::Gray | Color::DarkGray),
                    "ansi16 路径的 {semantic:?} 应为灰阶，实际 {fg:?}"
                );
            }
        }

        // 错误靠**反白**跳出来，而不是靠红色。
        assert!(
            rich.style(TuiSemanticStyle::Error)
                .add_modifier
                .contains(Modifier::REVERSED),
            "错误必须用反白表达"
        );
    }
}
