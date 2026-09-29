use ratatui::layout::{Margin, Rect};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TuiLayout {
    pub(crate) header: Rect,
    pub(crate) transcript: Rect,
    pub(crate) transcript_inner: Rect,
    pub(crate) transcript_scrollbar: Rect,
    pub(crate) bottom_pane: Rect,
}

pub(crate) fn compute_layout(area: Rect, bottom_pane_height: u16) -> TuiLayout {
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
    let header = Rect::new(area.x, area.y, area.width, header_height);
    let transcript = Rect::new(
        area.x,
        area.y.saturating_add(header_height),
        area.width,
        transcript_height,
    );
    let bottom_pane = Rect::new(
        area.x,
        area.y
            .saturating_add(header_height)
            .saturating_add(transcript_height),
        area.width,
        bottom_height,
    );
    // 去框之后不再需要纵向内边距（原本那 1 行上下是边框）；横向保留 1 列留白，
    // 让正文不贴着屏幕左缘 —— 这是新设计里唯一的「边距」。
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

            assert_eq!(layout.header, Rect::new(0, 0, width, 2));
            assert_eq!(layout.transcript, Rect::new(0, 2, width, transcript_height));
            assert_eq!(
                layout.transcript_inner,
                Rect::new(1, 2, width - 2, inner_height)
            );
            assert_eq!(
                layout.transcript_scrollbar,
                Rect::new(width - 1, 2, 1, inner_height)
            );
            assert_eq!(layout.bottom_pane, Rect::new(0, height - 4, width, 4));
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
