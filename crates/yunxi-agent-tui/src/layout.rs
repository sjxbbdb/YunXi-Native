use crate::terminal::chrome::{BODY_MAX, body_w, set_body_w};
use ratatui::layout::{Margin, Rect};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TuiLayout {
    pub(crate) header: Rect,
    pub(crate) transcript: Rect,
    pub(crate) transcript_inner: Rect,
    pub(crate) transcript_scrollbar: Rect,
    pub(crate) bottom_pane: Rect,
}

/// 正文列的宽度与起始 x。
///
/// 照搬 Miyu `chrome::compose` 的版式：正文约束在**一条居中的列**里，而不是
/// 撑满全宽。Miyu 那边的注释写得很直白 ——「不要框：内容居中，两侧空白铺极稀
/// 的暗星」——去框、居中、两侧留白是同一个设计决定的三个面。
///
/// `BODY_MAX = 62` 也是从那边来的：一行中文 40 字出头就不好读了，62 列是
/// 可读性上限；窄终端跟着视口缩，但不下于 20 列。
pub(crate) fn content_column(area: Rect) -> (u16, u16) {
    set_body_w(
        BODY_MAX
            .min(usize::from(area.width).saturating_sub(10))
            .max(20),
    );
    let width = (body_w() as u16).min(area.width);
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    (x, width)
}

pub(crate) fn compute_layout(area: Rect, bottom_pane_height: u16) -> TuiLayout {
    let (content_x, content_width) = content_column(area);
    let (header_height, transcript_height, bottom_height) = if area.height >= 7 {
        // 视觉重设计：状态行不再用 `───` 分隔线隔开，靠明度差与留白分层，
        // 所以这里从 3 行（状态 + 副状态 + 分隔线）减到 2 行。
        let header_height = 2;
        let bottom_height = bottom_pane_height
            .max(3)
            .min(area.height.saturating_sub(header_height + 1));
        (
            header_height,
            area.height
                .saturating_sub(header_height)
                .saturating_sub(bottom_height),
            bottom_height,
        )
    } else {
        let bottom_height = area.height.min(3);
        let header_height = area.height.saturating_sub(bottom_height).min(2);
        (
            header_height,
            area.height
                .saturating_sub(header_height)
                .saturating_sub(bottom_height),
            bottom_height,
        )
    };
    let header = Rect::new(content_x, area.y, content_width, header_height);
    let transcript = Rect::new(
        content_x,
        area.y.saturating_add(header_height),
        content_width,
        transcript_height,
    );
    let bottom_pane = Rect::new(
        content_x,
        area.y
            .saturating_add(header_height)
            .saturating_add(transcript_height),
        content_width,
        bottom_height,
    );
    // 居中列之内的横向留白：正文不贴着列的左缘。纵向不再需要内边距
    // （原本那 1 行上下是边框）。
    let transcript_inner = transcript.inner(Margin {
        vertical: 0,
        horizontal: 1,
    });
    let transcript_scrollbar = Rect {
        x: transcript
            .x
            .saturating_add(transcript.width.saturating_sub(1)),
        y: transcript.y,
        width: transcript.width.min(1),
        height: transcript.height,
    };

    TuiLayout {
        header,
        transcript,
        transcript_inner,
        transcript_scrollbar,
        bottom_pane,
    }
}

pub(crate) fn rect_contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_shared_transcript_inner_height() {
        let layout = compute_layout(Rect::new(0, 0, 100, 18), 3);

        assert_eq!(layout.header.height, 2);
        assert_eq!(layout.bottom_pane.height, 3);
        assert_eq!(layout.transcript.height, 13);
        assert_eq!(layout.transcript_inner.height, 13);
        // 去框：滚动条不再内缩上下各 1 行，跟随对话区全高。
        assert_eq!(layout.transcript_scrollbar.height, 13);
    }

    #[test]
    fn tiny_heights_never_overlap_and_keep_bottom_pane_visible() {
        for height in 0..=6 {
            let area = Rect::new(0, 0, 20, height);
            let layout = compute_layout(area, 8);
            assert_eq!(
                layout.header.height + layout.transcript.height + layout.bottom_pane.height,
                height
            );
            assert!(layout.bottom_pane.y >= layout.transcript.y + layout.transcript.height);
            assert!(layout.bottom_pane.y + layout.bottom_pane.height <= height);
            if height > 0 {
                assert!(layout.bottom_pane.height > 0);
            }
        }
    }

    #[test]
    fn normal_layout_preserves_at_least_one_transcript_row() {
        for height in 7..=40 {
            let layout = compute_layout(Rect::new(0, 0, 80, height), 20);
            assert!(layout.transcript.height >= 1);
            assert_eq!(
                layout.header.height + layout.transcript.height + layout.bottom_pane.height,
                height
            );
        }
    }

    #[test]
    fn snapshot_dimensions_have_stable_non_overlapping_regions() {
        for (width, height, transcript_height, inner_height) in [
            (80, 24, 18, 18),
            (100, 30, 24, 24),
            (120, 40, 34, 34),
            (200, 50, 44, 44),
        ] {
            let layout = compute_layout(Rect::new(0, 0, width, height), 4);
            // 期望值**从布局函数推导**，不硬编码 —— 正文列现在是一条居中的
            // 62 列（照搬 Miyu 的版式），硬编码数字每次调宽度都要重算一遍。
            let (content_x, content_width) = content_column(Rect::new(0, 0, width, height));

            assert_eq!(layout.header, Rect::new(content_x, 0, content_width, 2));
            assert_eq!(
                layout.transcript,
                Rect::new(content_x, 2, content_width, transcript_height)
            );
            assert_eq!(
                layout.transcript_inner,
                Rect::new(
                    content_x.saturating_add(1),
                    2,
                    content_width.saturating_sub(2),
                    inner_height
                )
            );
            assert_eq!(
                layout.transcript_scrollbar,
                Rect::new(content_x + content_width - 1, 2, 1, inner_height)
            );
            assert_eq!(
                layout.bottom_pane,
                Rect::new(content_x, height - 4, content_width, 4)
            );
            // 正文列必须真的居中，且不撑满全宽（窄到放不下 62 列的终端除外）。
            let left_margin = content_x;
            let right_margin = width - content_x - content_width;
            assert!(
                left_margin.abs_diff(right_margin) <= 1,
                "{width} 列下正文列没居中：左 {left_margin} 右 {right_margin}"
            );
            assert!(
                content_width <= 62 || content_width == width,
                "{width} 列下正文列宽 {content_width} 超过 BODY_MAX 且没退化成全宽"
            );
            assert_eq!(
                layout.transcript.y + layout.transcript.height,
                layout.bottom_pane.y
            );
            assert_eq!(
                layout.transcript_scrollbar.y + layout.transcript_scrollbar.height,
                layout.transcript.y + layout.transcript.height
            );
        }
    }

    #[test]
    fn responsive_density_matrix_keeps_bottom_pane_and_transcript_disjoint() {
        for (width, height) in [(58, 7), (58, 18), (80, 24), (200, 18), (200, 50)] {
            for requested_bottom_height in [3, 4, 10, 20] {
                let layout =
                    compute_layout(Rect::new(0, 0, width, height), requested_bottom_height);

                assert_eq!(
                    layout.header.height + layout.transcript.height + layout.bottom_pane.height,
                    height,
                    "{width}x{height} bottom={requested_bottom_height}"
                );
                assert_eq!(
                    layout.transcript.y + layout.transcript.height,
                    layout.bottom_pane.y,
                    "{width}x{height} bottom={requested_bottom_height}"
                );
                assert!(layout.bottom_pane.y + layout.bottom_pane.height <= height);
                if height >= 7 {
                    assert!(layout.transcript.height >= 1);
                }
            }
        }
    }
}
