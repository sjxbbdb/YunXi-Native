mod app;
mod approval_layout;
mod bottom_pane;
mod chat;
mod debug;
mod edit_buffer;
mod error_presentation;
mod event_filter;
mod frame;
mod host;
mod input_map;
#[cfg(test)]
mod integrated_regression;
mod layout;
// 从 Miyu 移植的终端外壳与版式机制（归属见 terminal/ATTRIBUTION.md）。
pub mod onboarding;
pub mod oobe;
mod output_summary;
mod presentation;
mod render;
mod scrollbar;
pub mod streaming;
mod styles;
pub mod terminal;

/// 显示工具参数/输出前的脱敏。设计原则来自 Miyu，见模块注释。
mod redact;
mod text_layout;
mod timeline;
mod timeline_store;
mod transcript_layout;
mod viewport;
pub mod welcome;
pub mod yunxi_starfield;

pub use app::YunxiTuiBanner;
pub use bottom_pane::{
    ApprovalDecision, ApprovalRequestView, UserInputRequestView, UserInputResponse,
};
pub use host::{TuiTickAction, YunxiTui};
pub use onboarding::{
    ChoiceOption, OnboardingAnswers, OnboardingOutcome, OnboardingStep, OnboardingStepId,
    OnboardingWizard, StepKind,
};
pub use presentation::{
    PresentationDetail, TuiCellId, TuiCellKind, TuiEvent, TuiSourceSequence, TuiStreamIdentity,
    TuiStreamPhase, TuiStreamState,
};
