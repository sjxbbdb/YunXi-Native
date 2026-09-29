//! YunXi 终端渲染层。
//!
//! 这个模块承载从 Miyu 移植过来的**终端外壳与版式机制**（色深降级、共用
//! chrome、`compose` 版式）。它是一层**适配边界**：Miyu 的类型不散布进
//! YunXi 的既有代码，`render.rs` 通过这里的公开接口渲染。
//!
//! 归属与许可证见 [`ATTRIBUTION.md`](ATTRIBUTION.md)（与本文件同目录）。

pub mod chrome;
pub mod palette;
pub mod starfield;

pub use chrome::{BODY_MAX, Chrome, Composed, Cx, Stop, View, compose};
/// 一条命令流是 stdout 还是 stderr。
///
/// 照搬自 `miyu-base::terminal`（见 ATTRIBUTION.md）：`text.rs` 的终端控制序列
/// 清洗要按流区分 —— 有的 TUI 只把状态行画到 stderr，把它拼进正文会串帧。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOutputStream {
    Stdout,
    Stderr,
}

/// 这些行在终端上实际占几行（长行会折）。
///
/// 等待动画要把光标精确地移回块首，所以必须知道**物理行数**，不能按逻辑行算。
pub fn rendered_physical_rows(widths: &[usize], terminal_width: usize) -> u16 {
    if terminal_width == 0 {
        return widths.len().min(usize::from(u16::MAX)) as u16;
    }
    let rows: usize = widths
        .iter()
        .map(|width| {
            if *width == 0 {
                1
            } else {
                width.div_ceil(terminal_width)
            }
        })
        .sum();
    rows.min(usize::from(u16::MAX)) as u16
}

/// 终端列数。`--cols` 之类的覆盖优先，其次 `COLUMNS`，最后给个合理默认。
pub fn terminal_cols(fallback: usize) -> usize {
    if let Some(value) = columns_override() {
        return value;
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|cols| *cols > 0)
        .unwrap_or(fallback)
}

/// 有没有显式的列数覆盖（`YUNXI_COLS`）。
pub fn columns_override() -> Option<usize> {
    std::env::var("YUNXI_COLS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|cols| *cols > 0)
}

/// 当前是否在用显式列数覆盖。
pub fn cols_override_active() -> bool {
    columns_override().is_some()
}

pub mod spinner;
pub mod text;
// `wait_spinner.rs` 已照搬在盘上（810 行），但**暂不接入编译**：它直接写 ANSI
// 控制序列（`MoveUp`/`Clear`），而 YunXi 的 TUI 是 ratatui 渲染循环，两者要接
// 得先把「谁负责光标」这件事定下来。硬接会留下编译不过的工作树。
// mod wait_spinner;

pub use spinner::{SPINNER_INTERVAL, SpinnerStyle};
pub use text::{
    clip_progress_line, clip_to_display_width, display_width_skipping_escapes, escape_len,
    sanitize_terminal_text, strip_ansi_text,
};

pub use palette::{Depth, Rgb, Theme};
pub use starfield::{BannerArt, Seg};
