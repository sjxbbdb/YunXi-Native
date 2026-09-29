//! 等待动画的**帧与轨迹**。
//!
//! **来源与归属**：帧表、扫描轨迹的状态机与停顿参数逐字照搬自
//! `SHORiN-KiWATA/miyu-agent`（MIT, Copyright (c) 2026 SHORiN-KIWATA）的
//! `crates/miyu-hosts/src/render/wait_spinner.rs`，快照见本仓库
//! `references/miyu-agent/`（pin `04a23ccb`）。归属与许可证见
//! [`ATTRIBUTION.md`](ATTRIBUTION.md)。
//!
//! **与原件的一处刻意不同**：原件自己往 stdout 写 ANSI 控制序列
//! （`MoveUp` / `Clear` / `BeginSynchronizedUpdate`）来原地重绘，光标归它管；
//! YunXi 的 TUI 是 ratatui 渲染循环，光标归 ratatui 管。两条路径同时管光标会
//! 打架，所以这里只搬**纯的部分** —— 这一帧每个格子该是什么字符、属于轨迹的
//! 第几层 —— 上色与写入交给调用方。视觉一致，架构不冲突。

/// 扫描轨迹的格数。
const WIDTH: usize = 7;
/// 拖尾长度：最亮那颗后面还跟几格渐隐。
const TRAIL_LEN: usize = 6;
/// 扫到右端后停多少帧。
const HOLD_END: usize = 9;
/// 扫回左端后停多少帧（比右端久，像真的在等）。
const HOLD_START: usize = 30;
/// 每帧时长。照搬 Miyu 的 40ms —— 再快就成了闪烁。
pub const SPINNER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(40);

/// 拖尾上从亮到暗的字符：▪ ▪ ▫ ▫ · ·
const ACTIVE_DOTS: [&str; TRAIL_LEN] = ["▪", "▪", "▫", "▫", "·", "·"];
const INACTIVE_DOT: &str = "·";
/// 盲文转轮的 10 帧。
const BRAILLE_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// 等待动画的样式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SpinnerStyle {
    /// 一颗点横扫过 7 格，两头各停一下。默认。
    #[default]
    Scanner,
    /// 单字符盲文转轮。
    Braille,
}

/// 盲文转轮的第 `frame` 帧。
pub fn braille_frame(frame: usize) -> &'static str {
    BRAILLE_FRAMES[frame % BRAILLE_FRAMES.len()]
}

/// 轨迹某一格的明暗层次。调用方按它挑颜色 —— 原件是在这里直接写 ANSI 的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrailEmphasis {
    /// 最前面两颗：全亮。
    Bright,
    /// 后面几颗与静止点：压暗。
    Dim,
}

/// 扫描器此刻的状态。字段与原件一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScannerState {
    active_position: usize,
    is_holding: bool,
    hold_progress: usize,
    hold_total: usize,
    movement_progress: usize,
    movement_total: usize,
    is_moving_forward: bool,
}

/// 一整个来回的帧数。
pub fn scanner_total_frames() -> usize {
    WIDTH + HOLD_END + (WIDTH - 1) + HOLD_START
}

/// 某种样式的周期帧数。
pub fn total_frames_for_style(style: SpinnerStyle) -> usize {
    match style {
        SpinnerStyle::Scanner => scanner_total_frames(),
        SpinnerStyle::Braille => BRAILLE_FRAMES.len(),
    }
}

/// 第 `frame` 帧的扫描器状态。逐字照搬原件的分段推进。
fn scanner_state(mut frame: usize) -> ScannerState {
    if frame < WIDTH {
        return ScannerState {
            active_position: frame,
            is_holding: false,
            hold_progress: 0,
            hold_total: 0,
            movement_progress: frame,
            movement_total: WIDTH,
            is_moving_forward: true,
        };
    }
    frame -= WIDTH;
    if frame < HOLD_END {
        return ScannerState {
            active_position: WIDTH - 1,
            is_holding: true,
            hold_progress: frame,
            hold_total: HOLD_END,
            movement_progress: 0,
            movement_total: 0,
            is_moving_forward: true,
        };
    }
    frame -= HOLD_END;
    if frame < WIDTH - 1 {
        return ScannerState {
            active_position: WIDTH - 2 - frame,
            is_holding: false,
            hold_progress: 0,
            hold_total: 0,
            movement_progress: frame,
            movement_total: WIDTH - 1,
            is_moving_forward: false,
        };
    }
    frame -= WIDTH - 1;
    ScannerState {
        active_position: 0,
        is_holding: true,
        hold_progress: frame,
        hold_total: HOLD_START,
        movement_progress: 0,
        movement_total: 0,
        is_moving_forward: false,
    }
}

/// 某一格离扫描头多远（0 = 就在头上）。
fn color_index(char_index: usize, state: ScannerState) -> Option<usize> {
    let distance = if state.is_moving_forward {
        state.active_position as isize - char_index as isize
    } else {
        char_index as isize - state.active_position as isize
    };
    if state.is_holding {
        return usize::try_from(distance)
            .ok()
            .map(|distance| distance + state.hold_progress);
    }
    if distance == 0 {
        return Some(0);
    }
    if distance > 0 && distance < TRAIL_LEN as isize {
        return usize::try_from(distance).ok();
    }
    None
}

/// 扫描器第 `frame` 帧的每一格。`None` = 那一格是静止点。
pub fn scanner_cells(frame: usize) -> Vec<Option<usize>> {
    let state = scanner_state(frame % scanner_total_frames());
    (0..WIDTH)
        .map(|cell| color_index(cell, state).and_then(|index| (index < TRAIL_LEN).then_some(index)))
        .collect()
}

/// 第 `frame` 帧的整行字符串（不上色）。
pub fn scanner_frame(frame: usize) -> String {
    scanner_cells(frame)
        .into_iter()
        .map(|cell| match cell {
            Some(index) => ACTIVE_DOTS[index.min(ACTIVE_DOTS.len() - 1)],
            None => INACTIVE_DOT,
        })
        .collect()
}

/// 第 `frame` 帧某个格子的明暗。
pub fn scanner_emphasis(frame: usize, cell: usize) -> TrailEmphasis {
    match scanner_cells(frame).get(cell).copied().flatten() {
        Some(index) if index < 2 => TrailEmphasis::Bright,
        _ => TrailEmphasis::Dim,
    }
}

/// 一行等待文本，样式无关的字符部分。
pub fn frame_text(style: SpinnerStyle, frame: usize) -> String {
    match style {
        SpinnerStyle::Scanner => scanner_frame(frame),
        SpinnerStyle::Braille => braille_frame(frame).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braille_frames_cycle_and_are_one_cell_wide() {
        assert_eq!(
            braille_frame(0),
            braille_frame(BRAILLE_FRAMES.len()),
            "帧表要循环"
        );
        for frame in 0..BRAILLE_FRAMES.len() * 3 {
            let glyph = braille_frame(frame);
            assert_eq!(
                glyph.chars().count(),
                1,
                "第 {frame} 帧不是单字符：{glyph:?}"
            );
        }
    }

    #[test]
    fn every_scanner_frame_is_the_same_width() {
        // 宽度一变，后面的正文就会左右横跳。
        for frame in 0..scanner_total_frames() * 2 {
            let text = scanner_frame(frame);
            assert_eq!(
                text.chars().count(),
                WIDTH,
                "第 {frame} 帧宽度不对：{text:?}"
            );
        }
    }

    #[test]
    fn scanner_sweeps_right_first_then_comes_back() {
        // 第一段：亮点从左往右走。
        assert_eq!(scanner_cells(0).iter().position(|c| c.is_some()), Some(0));
        // 扫描头 = 轨迹第 0 层那一格。注意别拿 `position(is_some)` 当扫描头：
        // 那找的是拖尾的**尾端**，不是头。
        let head = |frame: usize| scanner_cells(frame).iter().position(|c| *c == Some(0));
        assert_eq!(head(3), Some(3), "第 3 帧扫描头应在第 3 格");
        let last = WIDTH - 1;
        assert_eq!(head(last), Some(last), "扫到最右一格时扫描头应在那一格");
        // 回程：亮点从右往左回来。实测第 17 帧扫描头在第 4 格。
        let back = scanner_cells(WIDTH + HOLD_END + 1);
        assert_eq!(back.iter().position(|c| c.is_some()), Some(4), "{back:?}");
        // 一个周期后精确回到同一画面 —— 否则动画会漂移。
        assert_eq!(
            scanner_cells(0),
            scanner_cells(scanner_total_frames()),
            "周期结束后必须精确回到同一帧"
        );
    }

    #[test]
    fn the_trail_fades_out_while_holding_and_that_is_the_design() {
        // 两端的停顿不是「停在原地不动」，是**拖尾逐格淡掉**再重新出现。
        // 这是刻意设计的节奏：静止的亮点看起来像卡住了。
        let right_hold_start = WIDTH;
        assert!(
            scanner_cells(right_hold_start).iter().any(|c| c.is_some()),
            "刚扫到右端时拖尾还在"
        );
        let right_hold_end = WIDTH + HOLD_END - 1;
        assert!(
            scanner_cells(right_hold_end).iter().all(|c| c.is_none()),
            "右端停顿结束时拖尾应当已经淡完"
        );
        // 但**字符不会消失**：整行始终 7 格，最暗也是 `·`。
        let text = scanner_frame(right_hold_end);
        assert_eq!(text.chars().count(), WIDTH);
        assert!(
            text.chars().all(|ch| ch == '·'),
            "淡到底时应是整行静止点，而不是空白：{text:?}"
        );
    }

    #[test]
    fn the_trail_is_bright_at_the_head_and_dim_behind_it() {
        // 第 3 帧：扫描头在第 3 格，后面跟一串越来越暗的拖尾。
        let cells = scanner_cells(3);
        assert_eq!(cells[3], Some(0), "扫描头应是轨迹第 0 层");
        assert_eq!(cells[2], Some(1), "头后面第一层");
        assert_eq!(cells[1], Some(2), "再后面开始压暗");
        assert_eq!(cells[0], Some(3));
        assert_eq!(scanner_emphasis(3, 3), TrailEmphasis::Bright);
        assert_eq!(scanner_emphasis(3, 2), TrailEmphasis::Bright);
        assert_eq!(scanner_emphasis(3, 1), TrailEmphasis::Dim);
        // 轨迹只有 TRAIL_LEN 层：超出的一律是静止点。
        assert_eq!(scanner_emphasis(3, 5), TrailEmphasis::Dim);
    }

    #[test]
    fn a_full_cycle_always_renders_seven_cells() {
        // 真正的「不会消失」保证：任何一帧都恰好 7 个字符。
        // 之前我写成「每帧都要有亮点」，那是错的 —— 淡出本身就是设计。
        for frame in 0..scanner_total_frames() * 2 {
            assert_eq!(
                scanner_frame(frame).chars().count(),
                WIDTH,
                "第 {frame} 帧格数不对"
            );
        }
    }
}
