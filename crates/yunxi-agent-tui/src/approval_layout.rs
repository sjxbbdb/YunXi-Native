use crate::bottom_pane::ApprovalRequestView;
use crate::text_layout::{TextLayout, WrapPolicy};

const HEADER_LINES: usize = 1;
const REASON_LINES: usize = 1;
const RISK_LINES: usize = 1;

/// Approval key semantics are intentionally spelled out in the approval pane.
/// Enter confirms the highlighted action, so the safe default remains a decline.
///
/// Labels stay in the same language as the transcript: the conversation area is
/// fully localized, so an all-English pane sitting directly beneath it reads as
/// a different product.  Column padding is measured with `TextLayout::measure`
/// (display width), so the CJK labels below align on the same 9-column rail as
/// the ASCII ones they replace.
pub(crate) const APPROVAL_HINT_PRIMARY: &str = "Tab/Shift+Tab 选择 | Enter 确认选中项 | Esc 拒绝";
pub(crate) const APPROVAL_HINT_SECONDARY: &str = "Ctrl+C 取消 | Y 批准 | N 拒绝";
const APPROVAL_HINT_NARROW: &str = "Tab/Shift+Tab 选择 | Enter 确认 | Esc/N 拒绝";

/// Label column: every entry is padded to 9 display columns so wrapped
/// continuations line up under the value, not under the label.
const LABEL_TOOL: &str = "工具     ";
const LABEL_REASON: &str = "原因     ";
const LABEL_RISK: &str = "风险     ";
const LABEL_COMMAND: &str = "命令     ";

/// Risk values arrive prefixed with their own field name (`risk: destructive`),
/// which duplicated the label column.  The label already says 风险, so strip the
/// redundant prefix when rendering.
fn strip_field_prefix(value: &str) -> &str {
    for prefix in ["risk:", "risk：", "风险:"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return rest.trim_start();
        }
    }
    value
}

/// Policy reasons are stable identifiers: the sandbox compares against
/// `tool execution requires approval` internally, so the value is not translated
/// at its source.  Map the ones that reach the approval pane, and leave anything
/// else verbatim — upstream reasons often carry detail worth reading as-is.
fn localized_reason(reason: &str) -> String {
    match reason {
        "tool execution requires approval" => "该工具需要你的批准才能执行".to_string(),
        other => other.to_string(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApprovalLayout {
    pub(crate) lines: Vec<ApprovalLayoutLine>,
}

impl ApprovalLayout {
    pub(crate) fn desired_height(&self) -> u16 {
        // 去框：不再有上下边框的 2 行；调用方会额外插入一行标题，
        // 那 1 行由 `render.rs` 自己算进去。
        (self.lines.len() as u16).saturating_add(1)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ApprovalLayoutLine {
    Label {
        kind: ApprovalLineKind,
        label: String,
        text: String,
    },
    Blank,
    Action {
        label: &'static str,
        selected: bool,
    },
    Hint(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApprovalLineKind {
    Header,
    Reason,
    Risk,
    Command,
}

pub(crate) fn approval_desired_height(request: &ApprovalRequestView, width: usize) -> u16 {
    approval_layout_for_width(request, 0, width).desired_height()
}

pub(crate) fn approval_layout_for_width(
    request: &ApprovalRequestView,
    selected: usize,
    width: usize,
) -> ApprovalLayout {
    // 去框：不再给左右边框各留 1 列。
    let inner_width = width.max(20);
    let mut lines = Vec::new();

    push_labeled(
        &mut lines,
        ApprovalLineKind::Header,
        LABEL_TOOL,
        &format!("{} in {}", request.tool_name, request.cwd),
        inner_width,
        HEADER_LINES,
        WrapPolicy::WindowsPathAware,
    );
    push_labeled(
        &mut lines,
        ApprovalLineKind::Reason,
        LABEL_REASON,
        &localized_reason(&request.reason),
        inner_width,
        REASON_LINES,
        WrapPolicy::NaturalText,
    );
    push_labeled(
        &mut lines,
        ApprovalLineKind::Risk,
        LABEL_RISK,
        strip_field_prefix(&request.risk_label()),
        inner_width,
        RISK_LINES,
        WrapPolicy::NaturalText,
    );
    if let Some(command) = &request.command {
        let command_lines = if width < 80 {
            1
        } else if width < 120 {
            2
        } else {
            3
        };
        push_labeled(
            &mut lines,
            ApprovalLineKind::Command,
            LABEL_COMMAND,
            command,
            inner_width,
            command_lines,
            command_policy(command),
        );
    }

    lines.push(ApprovalLayoutLine::Blank);
    lines.push(ApprovalLayoutLine::Action {
        label: "批准",
        selected: selected == 0,
    });
    lines.push(ApprovalLayoutLine::Action {
        label: "拒绝（安全默认）",
        selected: selected == 1,
    });
    if width < 80 {
        // Keep the approval pane within the established narrow-terminal height
        // budget while retaining the safe selection semantics on screen.
        lines.push(ApprovalLayoutLine::Hint(APPROVAL_HINT_NARROW));
    } else {
        lines.push(ApprovalLayoutLine::Hint(APPROVAL_HINT_PRIMARY));
        lines.push(ApprovalLayoutLine::Hint(APPROVAL_HINT_SECONDARY));
    }

    ApprovalLayout { lines }
}

fn push_labeled(
    lines: &mut Vec<ApprovalLayoutLine>,
    kind: ApprovalLineKind,
    label: &'static str,
    text: &str,
    width: usize,
    max_lines: usize,
    policy: WrapPolicy,
) {
    let label_width = TextLayout::measure(label);
    let text_width = width.saturating_sub(label_width).max(8);
    let wrapped = wrap_text(text, text_width, max_lines.max(1), policy);
    let continuation = " ".repeat(label_width);
    for (index, text) in wrapped.into_iter().enumerate() {
        lines.push(ApprovalLayoutLine::Label {
            kind,
            label: if index == 0 {
                label.to_string()
            } else {
                continuation.clone()
            },
            text,
        });
    }
}

fn wrap_text(value: &str, width: usize, max_lines: usize, policy: WrapPolicy) -> Vec<String> {
    let mut rows = TextLayout::wrap(value.trim(), width, policy)
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>();
    if rows.len() <= max_lines {
        return rows;
    }

    rows.truncate(max_lines);
    if let Some(last) = rows.last_mut() {
        *last = append_ellipsis(last, width);
    }
    rows
}

fn append_ellipsis(value: &str, width: usize) -> String {
    if width <= 3 {
        return String::new();
    }
    let candidate = format!("{value}...");
    if TextLayout::measure(&candidate) <= width {
        candidate
    } else {
        truncate_end(&candidate, width)
    }
}

fn truncate_end(value: &str, width: usize) -> String {
    TextLayout::truncate(value, width)
}

fn command_policy(command: &str) -> WrapPolicy {
    if command.contains("://") {
        WrapPolicy::UrlAware
    } else if command.contains('\\') || command.contains(":\\") {
        WrapPolicy::WindowsPathAware
    } else {
        WrapPolicy::NaturalText
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ApprovalRequestView {
        ApprovalRequestView {
            id: None,
            tool_name: "shell".to_string(),
            cwd: "D:/YunXi Agent/workspace/with/a/very/long/path".to_string(),
            command: Some(
                "Remove-Item -Recurse -Force D:/YunXi Agent/workspace/generated/very-long-output"
                    .to_string(),
            ),
            reason:
                "tool execution requires approval before running a potentially destructive command"
                    .to_string(),
            risk_label: Some("risk: destructive".to_string()),
        }
    }

    #[test]
    fn approval_layout_reserves_decline_and_shortcut_lines_on_narrow_width() {
        let layout = approval_layout_for_width(&request(), 0, 58);
        let labels = layout
            .lines
            .iter()
            .filter_map(|line| match line {
                ApprovalLayoutLine::Action { label, .. } => Some(*label),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["批准", "拒绝（安全默认）"]);
        assert!(
            layout
                .lines
                .contains(&ApprovalLayoutLine::Hint(APPROVAL_HINT_NARROW))
        );
        assert!(layout.desired_height() <= 10);
    }

    #[test]
    fn narrow_approval_preserves_risk_path_and_destructive_command_identity() {
        let request = ApprovalRequestView {
            cwd: "C:\\Users\\24763\\YunXi Agent\\包含空格\\危险输出目录".to_string(),
            command: Some(
                "Remove-Item -Recurse -Force C:\\Users\\24763\\YunXi Agent\\包含空格\\危险输出目录"
                    .to_string(),
            ),
            reason: "削除前に利用者の明示的な承認が必要です".to_string(),
            ..request()
        };
        let layout = approval_layout_for_width(&request, 1, 58);
        let rendered = layout
            .lines
            .iter()
            .filter_map(|line| match line {
                ApprovalLayoutLine::Label { label, text, .. } => Some(format!("{label}{text}")),
                ApprovalLayoutLine::Action { label, .. } => Some((*label).to_string()),
                ApprovalLayoutLine::Hint(value) => Some((*value).to_string()),
                ApprovalLayoutLine::Blank => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        // The `risk:` prefix is stripped because the label column already reads
        // 风险; the value itself must survive verbatim.
        assert!(rendered.contains("destructive"));
        assert!(!rendered.contains("risk: destructive"));
        assert!(rendered.contains("Remove-Item"));
        assert!(rendered.contains("C:\\"));
        assert!(rendered.contains("批准"));
        assert!(rendered.contains("拒绝"));
        assert!(rendered.contains("安全默认"));
        assert!(rendered.contains("Tab/Shift+Tab 选择"));
        assert!(rendered.contains("Esc/N 拒绝"));
    }
}
