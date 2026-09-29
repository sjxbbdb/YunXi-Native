//! 云熙配置向导 - 首次启动的逐项引导界面
//!
//! 设计理念：
//! - 复刻欢迎界面（`welcome.rs`）的视觉语言：星空底 + 圆角框 + 云熙配色 + 居中
//! - 一屏一步：进度、标题、「为什么问这个」、输入区或选项列表、底部按键提示
//! - 本模块**只做渲染与键盘收集**：不碰文件系统、不碰网络
//!
//! 契约说明（见方案第三节）：宿主负责准备步骤（[`OnboardingStep`]）并把收集到的
//! [`OnboardingAnswers`] 落盘；向导本身只回显与暂存，不解释语义。
//!
//! ```ignore
//! let mut wizard = OnboardingWizard::new(steps);
//! // 事件循环里：
//! if let Some(outcome) = wizard.handle_key(key) { /* Completed / Skipped */ }
//! let output = wizard.render(cols, rows);   // 每行 `cols` 显示宽度、共 `rows` 行
//! ```

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use unicode_segmentation::UnicodeSegmentation;

use crate::text_layout::TextLayout;
use crate::yunxi_starfield::{
    Seg, YUNXI_INK, YUNXI_LAVENDER, YUNXI_PURPLE, YUNXI_SILVER, YUNXI_WHITE, lerp_color, star_seg,
};

/// 圆角框最大宽度：80 列终端上占 80%（64 列），120 列终端上到 76 列就不再变宽 ——
/// 再宽正文会散，行也难读（和欢迎界面艺术字的体量感保持一致）
const FRAME_MAX_WIDTH: usize = 76;
/// 框内左右各缩进几列
const FRAME_PAD_X: usize = 2;
/// 框内上下各留几行空白
const FRAME_PAD_Y: usize = 1;
/// 星空稀疏度（与欢迎界面同量级，向导界面要更安静一点）
const STAR_SPARSITY: u32 = 11;
/// 星空亮度（比欢迎界面更收敛，别抢正文）
const STAR_SCALE: f32 = 0.45;
/// 选中项高亮的背景色（把云熙主色压暗，保证银白前景可读）
const SELECTION_BG: Color = Color::Rgb(0x3A, 0x30, 0x50);
/// 遮掩回显用的圆点
const MASK_GLYPH: &str = "•";
/// 输入光标（块状，跟随文字尾部）
const CURSOR_GLYPH: &str = "▌";
/// 未选中项的引导符
const CHOICE_MARK: &str = "○";
/// 选中项的引导符
const CHOICE_GLYPH: &str = "◆";
/// 用户卡片一步里三个小问题的引导语
const CARD_PROMPTS: [&str; 3] = [
    "我该怎么称呼你？",
    "你是做什么的？",
    "有什么要我记住的偏好？",
];

// ─────────────────── 契约类型 ───────────────────

/// 向导步骤的身份
///
/// 内置的五个变体对应方案第二节的五步：API、人格、关于你（用户卡片）、记忆库、
/// 知识库。宿主按自己的需要排列步骤顺序（`UserCard` 一步内部依次问称呼、身份、
/// 偏好三个小问题）。[`OnboardingStepId::Custom`] 给宿主预留：向导会正常渲染与
/// 收集，但因为没有对应字段，文本会停留在界面上、不写入 [`OnboardingAnswers`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OnboardingStepId {
    /// 第 1 步：先连上模型（API key / base url / 模型名）
    ApiKey,
    /// 同一个 API 步骤里的 base url 小问
    ApiBaseUrl,
    /// 同一个 API 步骤里的模型名小问
    ApiModel,
    /// 第 2 步：人格 profile
    Profile,
    /// 第 3 步：关于你（用户卡片），内含称呼 / 身份 / 偏好三小问
    UserCard,
    /// 第 3 步的第 1 小问：称呼
    UserName,
    /// 第 3 步的第 2 小问：身份
    UserIdentity,
    /// 第 3 步的第 3 小问：偏好
    UserPreference,
    /// 第 4 步：记忆库开关
    Memory,
    /// 第 5 步：知识库开关
    Knowledge,
    /// 宿主自定义步骤（向导不解释、不落字段）
    Custom(String),
}

/// 向导的一步
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnboardingStep {
    pub id: OnboardingStepId,
    /// 标题，例："先连上模型"
    pub title: String,
    /// 一句话说明为什么要问
    pub hint: String,
    pub kind: StepKind,
}

impl OnboardingStep {
    /// 建一步（id + 标题 + 说明 + 交互类型）
    pub fn new(
        id: OnboardingStepId,
        title: impl Into<String>,
        hint: impl Into<String>,
        kind: StepKind,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            hint: hint.into(),
            kind,
        }
    }

    /// 文本步骤的便捷构造（默认不遮掩、无占位提示）
    pub fn text(id: OnboardingStepId, title: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(
            id,
            title,
            hint,
            StepKind::Text {
                masked: false,
                placeholder: String::new(),
            },
        )
    }

    /// 遮掩文本步骤（API key 用）
    pub fn masked(
        id: OnboardingStepId,
        title: impl Into<String>,
        hint: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Self {
        Self::new(
            id,
            title,
            hint,
            StepKind::Text {
                masked: true,
                placeholder: placeholder.into(),
            },
        )
    }

    /// 单选项步骤
    pub fn choice(
        id: OnboardingStepId,
        title: impl Into<String>,
        hint: impl Into<String>,
        options: Vec<ChoiceOption>,
    ) -> Self {
        Self::new(id, title, hint, StepKind::Choice { options })
    }

    /// 是 / 否步骤
    pub fn yes_no(
        id: OnboardingStepId,
        title: impl Into<String>,
        hint: impl Into<String>,
        default_yes: bool,
    ) -> Self {
        Self::new(id, title, hint, StepKind::YesNo { default_yes })
    }
}

/// 一步的交互类型
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepKind {
    /// 单行文本（API key 用这个，回显需遮掩）
    Text { masked: bool, placeholder: String },
    /// 从若干选项里选一个
    Choice { options: Vec<ChoiceOption> },
    /// 是 / 否
    YesNo { default_yes: bool },
}

/// 单选项
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceOption {
    /// 落进 [`OnboardingAnswers`] 的值（宿主用它做语义解释）
    pub id: String,
    /// 列表里显示的名字
    pub label: String,
    /// 选中项下方显示的一句话说明
    pub detail: String,
}

impl ChoiceOption {
    pub fn new(id: impl Into<String>, label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: detail.into(),
        }
    }
}

/// 向导收集到的原始答案（TUI 不解释语义，原样交给宿主）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OnboardingAnswers {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub profile: String,
    pub user_name: String,
    pub user_identity: String,
    pub user_preference: String,
    pub memory_enabled: bool,
    pub knowledge_enabled: bool,
}

/// 结果：走完了 / 用户跳过了
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OnboardingOutcome {
    Completed(OnboardingAnswers),
    /// `at_step` 是用户按下 Esc 时停留的那一步
    Skipped {
        at_step: OnboardingStepId,
    },
}

// ─────────────────── 向导状态 ───────────────────

/// 一步的收集状态
#[derive(Clone, Debug, PartialEq, Eq)]
enum StepState {
    /// 文本缓冲（始终与 `answers` 同步，宿主随时可以取部分答案）
    Text {
        value: String,
    },
    /// 文本步骤的校验结果（宿主通过 [`OnboardingStep::new`] 之外的入口设置）
    Choice {
        selected: usize,
    },
    YesNo {
        yes: bool,
    },
}

impl StepState {
    fn for_kind(kind: &StepKind) -> Self {
        match kind {
            StepKind::Text { .. } => Self::Text {
                value: String::new(),
            },
            StepKind::Choice { .. } => Self::Choice { selected: 0 },
            StepKind::YesNo { default_yes } => Self::YesNo { yes: *default_yes },
        }
    }
}

/// 交互式配置向导
///
/// 状态机：`steps[index]` 是当前步；[`OnboardingWizard::handle_key`] 返回 `None`
/// 表示还在等输入，返回 `Some(_)` 表示这一轮向导结束。
#[derive(Clone, Debug)]
pub struct OnboardingWizard {
    steps: Vec<OnboardingStep>,
    /// 当前步（空步骤表时为 0，但 `handle_key` 会直接返回 `Skipped`）
    index: usize,
    /// 用户卡片一步的内部小问下标 0..3（0 称呼 / 1 身份 / 2 偏好）
    card_prompt: usize,
    states: Vec<StepState>,
    answers: OnboardingAnswers,
}

impl OnboardingWizard {
    /// 用宿主准备好的步骤建一个向导
    pub fn new(steps: Vec<OnboardingStep>) -> Self {
        // 预填默认值：是 / 否步骤跟 `default_yes` 对齐，其余取 `Default`
        let mut answers = OnboardingAnswers::default();
        for step in &steps {
            match &step.kind {
                StepKind::YesNo { default_yes } => match step.id {
                    OnboardingStepId::Memory => answers.memory_enabled = *default_yes,
                    OnboardingStepId::Knowledge => answers.knowledge_enabled = *default_yes,
                    _ => {}
                },
                StepKind::Choice { options } => {
                    if step.id == OnboardingStepId::Profile {
                        answers.profile = options.first().map(|o| o.id.clone()).unwrap_or_default();
                    }
                }
                StepKind::Text { .. } => {}
            }
        }
        let states = steps
            .iter()
            .map(|step| StepState::for_kind(&step.kind))
            .collect();

        Self {
            steps,
            index: 0,
            card_prompt: 0,
            states,
            answers,
        }
    }

    /// 当前步（没有步骤时为 `None`）
    pub fn current_step(&self) -> Option<&OnboardingStep> {
        self.steps.get(self.index)
    }

    /// 当前步的 id（没有步骤时为 `None`）
    pub fn current_id(&self) -> Option<&OnboardingStepId> {
        self.current_step().map(|step| &step.id)
    }

    /// `(当前步, 总步数)`，当前步从 1 开始（空步骤表返回 `(0, 0)`）
    pub fn progress(&self) -> (usize, usize) {
        if self.steps.is_empty() {
            (0, 0)
        } else {
            (self.index + 1, self.steps.len())
        }
    }

    /// 到目前为止收到的答案（宿主可以在向导中途取用，用于「边填边验证」）
    pub fn answers(&self) -> &OnboardingAnswers {
        &self.answers
    }

    /// 当前文本步骤缓冲区里的内容（Choice / YesNo 步骤返回空串）
    pub fn buffer(&self) -> &str {
        match self.states.get(self.index) {
            Some(StepState::Text { value }) => value,
            _ => "",
        }
    }

    /// 当前 Choice 步骤选中的下标（其它类型返回 0）
    pub fn selected_index(&self) -> usize {
        match self.states.get(self.index) {
            Some(StepState::Choice { selected }) => *selected,
            _ => 0,
        }
    }

    /// 当前 YesNo 步骤选中的值（其它类型返回 false）
    pub fn selected_yes(&self) -> bool {
        match self.states.get(self.index) {
            Some(StepState::YesNo { yes }) => *yes,
            _ => false,
        }
    }

    /// 是否停在最后一步的最后一小问（此时 Enter 表示「完成」）
    pub fn is_last(&self) -> bool {
        if self.steps.is_empty() {
            return true;
        }
        self.index + 1 >= self.steps.len() && self.card_prompt + 1 >= self.card_prompts()
    }

    /// 处理一个按键；返回 `None` 表示还在等输入
    ///
    /// 键盘语义：`Enter` 确认并前进（最后一步返回
    /// [`OnboardingOutcome::Completed`]）；`Esc` 跳过（返回
    /// [`OnboardingOutcome::Skipped`]）；Choice 用 `Tab` / `↑` / `↓` 切换；YesNo 用
    /// `←` / `→` 或 `y` / `n`；文本用 `Backspace` 编辑。留空的文本步骤不会前进
    /// （空 = 用户不想填这一项），可以直接 `Esc` 跳过。
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<OnboardingOutcome> {
        // 只认按下事件，忽略 Release / Repeat（与 `input_map.rs`、`bottom_pane.rs` 一致）
        if key.kind != KeyEventKind::Press {
            return None;
        }
        // Ctrl/Alt 组合键在向导里没有语义，直接忽略，别把它当普通字符吞进输入框
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        if self.steps.is_empty() {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter => Some(OnboardingOutcome::Skipped {
                    at_step: OnboardingStepId::Custom(String::new()),
                }),
                _ => None,
            };
        }

        let last = self.is_last();
        match key.code {
            KeyCode::Esc => Some(OnboardingOutcome::Skipped {
                at_step: self.step_id(),
            }),
            KeyCode::Enter if !ctrl && !alt => self.submit(last),
            KeyCode::Backspace if !ctrl && !alt => {
                self.edit_backspace();
                None
            }
            KeyCode::Up => {
                self.cycle_choice(-1);
                None
            }
            KeyCode::Down => {
                self.cycle_choice(1);
                None
            }
            KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.cycle_choice(1);
                None
            }
            KeyCode::BackTab => {
                self.cycle_choice(-1);
                None
            }
            KeyCode::Left => {
                self.cycle_yes_no(-1);
                None
            }
            KeyCode::Right => {
                self.cycle_yes_no(1);
                None
            }
            KeyCode::Char(ch) if !ctrl && !alt => {
                match self.kind() {
                    // 是 / 否步骤里 y / n 是「选定并确认」
                    Some(StepKind::YesNo { .. }) => match ch {
                        'y' | 'Y' => self.pick_yes_no(true, last),
                        'n' | 'N' => self.pick_yes_no(false, last),
                        _ => None,
                    },
                    // 文本步骤里 y / n 就是普通字符
                    _ => {
                        self.push_char(ch);
                        None
                    }
                }
            }
            _ => None,
        }
    }

    /// 渲染一屏
    ///
    /// 返回的行是「屏幕行 → 文本片段」：**恰好 `rows` 行**，每行拼接后的显示宽度
    /// **恰好 `cols`**（用 [`TextLayout::measure`] 按显示宽度算，中文不会偏）。
    /// 宽高不足时内容整体裁剪，不会 panic、不会溢出屏幕。
    pub fn render(&self, cols: usize, rows: usize) -> Vec<Vec<Seg>> {
        let mut grid = StarGrid::new(cols, rows);
        if cols == 0 || rows == 0 || self.steps.is_empty() {
            return grid.into_rows();
        }

        // 圆角框宽度：占屏幕 80%，上限 `FRAME_MAX_WIDTH`；窄屏至少留两列边距
        let box_width = (cols * 4 / 5)
            .clamp(16, FRAME_MAX_WIDTH)
            .min(cols.saturating_sub(2).max(1));
        let left = (cols - box_width) / 2;

        let inner_width = box_width.saturating_sub(2 * (FRAME_PAD_X + 1)).max(1);
        // 正文、页脚各占一行，正文超屏时先砍尾部
        let capacity = rows.saturating_sub(2 + 2 * FRAME_PAD_Y + 1);
        let mut body = self.body_lines(inner_width);
        body.truncate(capacity);
        // 框高按**实际画得下**的行数算：屏幕放不下时框底和页脚会掉出屏幕，
        // 这时不能再拿完整高度去居中，否则整块内容会偏上留一条空白
        let painted = body.len() + 2 * FRAME_PAD_Y + 2;
        let box_height = painted.min(rows).max(1);
        let top = (rows - box_height) / 2;

        self.paint_frame(&mut grid, left, top, box_width, box_height);
        self.paint_body(&mut grid, left, top, box_width, box_height, &body);
        self.paint_footer(&mut grid, left, top, box_width, box_height);

        grid.into_rows()
    }

    // ─────────── 状态迁移 ───────────

    fn step(&self) -> Option<&OnboardingStep> {
        self.steps.get(self.index)
    }

    fn step_id(&self) -> OnboardingStepId {
        match self.current_id() {
            Some(id) => id.clone(),
            // 兜底：步骤表为空时只能给出自定义占位（宿主不该走到这条路径）
            None => OnboardingStepId::Custom(String::new()),
        }
    }

    fn kind(&self) -> Option<StepKind> {
        self.step().map(|step| step.kind.clone())
    }

    /// 用户卡片一步里有几个小问（其它步骤恒为 1）
    fn card_prompts(&self) -> usize {
        match self.current_id() {
            Some(OnboardingStepId::UserCard) => CARD_PROMPTS.len(),
            _ => 1,
        }
    }

    /// Enter：收集当前值并前进
    fn submit(&mut self, last: bool) -> Option<OnboardingOutcome> {
        match self.kind() {
            Some(StepKind::Text { .. }) => {
                if self.buffer().is_empty() {
                    // 留空 = 用户放弃这一项：留在原地，让宿主决定要不要强制
                    return None;
                }
            }
            Some(StepKind::Choice { options }) => {
                if options.is_empty() {
                    return None;
                }
                self.commit_choice();
            }
            Some(StepKind::YesNo { .. }) => self.commit_yes_no(),
            None => {}
        }

        if last {
            return Some(OnboardingOutcome::Completed(self.answers.clone()));
        }
        self.advance();
        None
    }

    /// 前进一步；用户卡片内部先走小问
    fn advance(&mut self) {
        if self.card_prompt + 1 < self.card_prompts() {
            self.card_prompt += 1;
            self.reset_state_for_current();
            return;
        }
        self.card_prompt = 0;
        if self.index + 1 < self.steps.len() {
            self.index += 1;
            self.reset_state_for_current();
        }
    }

    /// 进入新一步时载入已有答案（回退 / 重放时不会丢用户填过的东西）
    fn reset_state_for_current(&mut self) {
        let Some(step) = self.steps.get(self.index) else {
            return;
        };
        let id = step.id.clone();
        let state = match (&step.kind, self.card_prompt) {
            (StepKind::Text { .. }, _) => StepState::Text {
                value: self.field_value(&id),
            },
            (StepKind::Choice { options }, _) => StepState::Choice {
                selected: options
                    .iter()
                    .position(|option| option.id == self.answers.profile)
                    .unwrap_or(0),
            },
            (StepKind::YesNo { default_yes }, _) => StepState::YesNo {
                yes: match id {
                    OnboardingStepId::Memory => self.answers.memory_enabled,
                    OnboardingStepId::Knowledge => self.answers.knowledge_enabled,
                    _ => *default_yes,
                },
            },
        };
        if let Some(slot) = self.states.get_mut(self.index) {
            *slot = state;
        }
    }

    /// 某一步已有的文本答案（用户卡片按小问分别取）
    fn field_value(&self, id: &OnboardingStepId) -> String {
        match id {
            OnboardingStepId::ApiKey => self.answers.api_key.clone(),
            OnboardingStepId::ApiBaseUrl => self.answers.base_url.clone(),
            OnboardingStepId::ApiModel => self.answers.model.clone(),
            OnboardingStepId::UserName => self.answers.user_name.clone(),
            OnboardingStepId::UserIdentity => self.answers.user_identity.clone(),
            OnboardingStepId::UserPreference => self.answers.user_preference.clone(),
            // 用户卡片一步：小问下标决定填哪个字段
            OnboardingStepId::UserCard => match self.card_prompt {
                1 => self.answers.user_identity.clone(),
                2 => self.answers.user_preference.clone(),
                _ => self.answers.user_name.clone(),
            },
            _ => String::new(),
        }
    }

    /// 把当前文本缓冲写回答案
    fn store_text(&mut self) {
        let id = self.step_id();
        let value = self.buffer().to_string();
        match id {
            OnboardingStepId::ApiKey => self.answers.api_key = value,
            OnboardingStepId::ApiBaseUrl => self.answers.base_url = value,
            OnboardingStepId::ApiModel => self.answers.model = value,
            OnboardingStepId::UserName => self.answers.user_name = value,
            OnboardingStepId::UserIdentity => self.answers.user_identity = value,
            OnboardingStepId::UserPreference => self.answers.user_preference = value,
            OnboardingStepId::UserCard => match self.card_prompt {
                1 => self.answers.user_identity = value,
                2 => self.answers.user_preference = value,
                _ => self.answers.user_name = value,
            },
            // API key / base url / 模型名的「先测 API」小问由宿主自行排列
            OnboardingStepId::Profile
            | OnboardingStepId::Memory
            | OnboardingStepId::Knowledge
            | OnboardingStepId::Custom(_) => {}
        }
    }

    fn commit_choice(&mut self) {
        let Some(StepKind::Choice { options }) = self.kind() else {
            return;
        };
        let selected = self.selected_index().min(options.len().saturating_sub(1));
        let Some(option) = options.get(selected) else {
            return;
        };
        let id = self.step_id();
        if id == OnboardingStepId::Profile {
            self.answers.profile = option.id.clone();
        }
    }

    fn commit_yes_no(&mut self) {
        let yes = self.selected_yes();
        match self.step_id() {
            OnboardingStepId::Memory => self.answers.memory_enabled = yes,
            OnboardingStepId::Knowledge => self.answers.knowledge_enabled = yes,
            _ => {}
        }
    }

    fn push_char(&mut self, ch: char) {
        // 只接受可见字符：控制字符（含 Ctrl 组合留下的残留）不进输入框
        if ch.is_control() {
            return;
        }
        match self.states.get_mut(self.index) {
            Some(StepState::Text { value }) => {
                value.push(ch);
            }
            _ => return,
        }
        self.store_text();
    }

    fn edit_backspace(&mut self) {
        match self.states.get_mut(self.index) {
            Some(StepState::Text { value }) => {
                // 按字素簇删除：中文 / emoji 一次退一个，不留半个字符
                if let Some((offset, _)) = value.grapheme_indices(true).next_back() {
                    value.truncate(offset);
                } else {
                    return;
                }
            }
            _ => return,
        }
        self.store_text();
    }

    fn cycle_choice(&mut self, delta: isize) {
        let Some(StepKind::Choice { options }) = self.kind() else {
            return;
        };
        let count = options.len();
        if count == 0 {
            return;
        }
        if let Some(StepState::Choice { selected }) = self.states.get_mut(self.index) {
            let current = (*selected).min(count - 1) as isize;
            *selected = (current + delta).rem_euclid(count as isize) as usize;
        }
        self.commit_choice();
    }

    fn cycle_yes_no(&mut self, delta: isize) {
        let Some(StepKind::YesNo { .. }) = self.kind() else {
            return;
        };
        if delta == 0 {
            return;
        }
        if let Some(StepState::YesNo { yes }) = self.states.get_mut(self.index) {
            *yes = delta > 0;
        }
        self.commit_yes_no();
    }

    fn pick_yes_no(&mut self, yes: bool, last: bool) -> Option<OnboardingOutcome> {
        let Some(StepKind::YesNo { .. }) = self.kind() else {
            return None;
        };
        if let Some(StepState::YesNo { yes: slot }) = self.states.get_mut(self.index) {
            *slot = yes;
        }
        self.submit(last)
    }

    // ─────────── 渲染 ───────────

    /// 框内正文（不含圆角框与页脚）
    fn body_lines(&self, inner_width: usize) -> Vec<Vec<Seg>> {
        let mut lines: Vec<Vec<Seg>> = Vec::new();
        let (current, total) = self.progress();
        if total == 0 {
            return lines;
        }
        let title = self
            .step()
            .map(|step| step.title.clone())
            .unwrap_or_default();
        let hint = self
            .step()
            .map(|step| step.hint.clone())
            .unwrap_or_default();

        // 进度行：「第 2 步 / 共 5 步」
        lines.push(vec![
            Seg::new(format!("第 {current} 步"), style_accent()),
            Seg::new(" / ", style_dim()),
            Seg::new(format!("共 {total} 步"), style_dim()),
        ]);
        // 标题
        lines.push(vec![Seg::new(
            TextLayout::truncate(&title, inner_width),
            style_title(),
        )]);
        // 用户卡片一步：再补一行小问引导语（第 n/3 问）
        if let Some(prompt) = self.card_prompt_line() {
            lines.push(vec![
                Seg::new(
                    TextLayout::truncate(&prompt, inner_width.saturating_sub(6)),
                    style_accent(),
                ),
                Seg::new(format!("  {}", self.card_prompt_progress()), style_dim()),
            ]);
        }
        lines.push(Vec::new());
        // 「为什么问这个」
        for line in wrap_text(&hint, inner_width) {
            lines.push(vec![Seg::new(line, style_hint())]);
        }
        lines.push(Vec::new());
        // 输入区或选项列表
        lines.extend(self.kind_lines(inner_width));
        lines
    }

    /// 用户卡片一步的小问引导语（其它步骤为 `None`）
    fn card_prompt_line(&self) -> Option<String> {
        match self.current_id() {
            Some(OnboardingStepId::UserCard) => {
                Some(CARD_PROMPTS[self.card_prompt.min(CARD_PROMPTS.len() - 1)].to_string())
            }
            _ => None,
        }
    }

    /// 用户卡片的小问序号，例："2/3"
    fn card_prompt_progress(&self) -> String {
        match self.current_id() {
            Some(OnboardingStepId::UserCard) => {
                format!("{}/{}", self.card_prompt + 1, CARD_PROMPTS.len())
            }
            _ => String::new(),
        }
    }

    /// 输入控件
    fn kind_lines(&self, inner_width: usize) -> Vec<Vec<Seg>> {
        match self.kind() {
            Some(StepKind::Text {
                masked,
                placeholder,
            }) => self.text_lines(masked, &placeholder, inner_width),
            Some(StepKind::Choice { options }) => self.choice_lines(&options, inner_width),
            Some(StepKind::YesNo { .. }) => self.yes_no_lines(inner_width),
            None => Vec::new(),
        }
    }

    /// 单行文本输入区
    fn text_lines(&self, masked: bool, placeholder: &str, inner_width: usize) -> Vec<Vec<Seg>> {
        let value = self.buffer();
        // 遮掩：只回显等长的圆点，真实值留在缓冲里（绝不进渲染结果）
        let shown = if masked {
            MASK_GLYPH.repeat(value.graphemes(true).count())
        } else {
            value.to_string()
        };
        let cursor_width = TextLayout::measure(CURSOR_GLYPH);
        let shown_width = TextLayout::measure(&shown);
        let placeholder_width = TextLayout::measure(placeholder);

        let mut line: Vec<Seg> = Vec::new();
        if value.is_empty() {
            // 空输入：光标在前，占位提示在后
            line.push(Seg::new(CURSOR_GLYPH, style_cursor()));
            if !placeholder.is_empty() {
                line.push(Seg::new(
                    TextLayout::truncate(placeholder, inner_width.saturating_sub(cursor_width)),
                    style_placeholder(),
                ));
            }
        } else {
            let room = inner_width.saturating_sub(cursor_width);
            line.push(Seg::new(tail_clamp(&shown, room), style_value()));
            line.push(Seg::new(CURSOR_GLYPH, style_cursor()));
            if placeholder_width > 0 {
                let used = shown_width.min(room);
                let rest = inner_width.saturating_sub(used + cursor_width);
                if rest > 0 {
                    line.push(Seg::new(
                        TextLayout::truncate(&format!("  {placeholder}"), rest),
                        style_placeholder(),
                    ));
                }
            }
        }
        vec![line]
    }

    /// 单选项列表：当前项高亮，`detail` 显示在下方
    fn choice_lines(&self, options: &[ChoiceOption], inner_width: usize) -> Vec<Vec<Seg>> {
        let mut lines = Vec::new();
        if options.is_empty() {
            lines.push(vec![Seg::new("（这一步没有可选项）", style_dim())]);
            return lines;
        }
        let selected = self.selected_index().min(options.len() - 1);
        for (index, option) in options.iter().enumerate() {
            let active = index == selected;
            let prefix = if active { "› " } else { "  " };
            let mark = if active { CHOICE_GLYPH } else { CHOICE_MARK };
            let body_width = inner_width.saturating_sub(4);
            let label = TextLayout::truncate(&option.label, body_width);
            let pad = body_width.saturating_sub(TextLayout::measure(&label));
            let (fg, background) = if active {
                (style_selected(), Style::default().bg(SELECTION_BG))
            } else {
                (style_choice(), Style::new())
            };
            lines.push(vec![
                Seg::new(prefix, background),
                Seg::new(mark, background.patch(fg)),
                Seg::new(" ", background),
                Seg::new(label, background.patch(fg)),
                Seg::new(" ".repeat(pad), background),
            ]);
        }
        if let Some(detail) = options.get(selected).map(|option| option.detail.clone()) {
            if !detail.is_empty() {
                lines.push(Vec::new());
                lines.push(vec![Seg::new(
                    TextLayout::truncate(&format!("     {detail}"), inner_width),
                    style_hint(),
                )]);
            }
        }
        lines
    }

    /// 是 / 否选择区
    fn yes_no_lines(&self, inner_width: usize) -> Vec<Vec<Seg>> {
        let yes = self.selected_yes();
        let mut line = Vec::new();
        // 两个选项用同样的串长（" 是 " / " 否 " 都是 4 列），位置不会因为选中项而跳
        for (label, active) in [("是", yes), ("否", !yes)] {
            let (fg, background) = if active {
                (style_selected(), Style::default().bg(SELECTION_BG))
            } else {
                (style_choice(), Style::new())
            };
            line.push(Seg::new("  ", Style::new()));
            line.push(Seg::new(
                if active { CHOICE_GLYPH } else { CHOICE_MARK },
                background.patch(fg),
            ));
            line.push(Seg::new(format!(" {label} "), background.patch(fg)));
        }
        vec![
            line,
            Vec::new(),
            vec![Seg::new(
                TextLayout::truncate(
                    &format!(
                        "     当前选择：{}",
                        if yes {
                            "是（启用）"
                        } else {
                            "否（本次先不启用）"
                        }
                    ),
                    inner_width,
                ),
                style_hint(),
            )],
        ]
    }

    /// 底部按键提示
    fn footer_line(&self) -> Vec<Seg> {
        let mut footer = vec![
            Seg::new("Enter", style_key()),
            Seg::new(
                if self.is_last() {
                    " 完成"
                } else {
                    " 下一步"
                },
                style_dim(),
            ),
            Seg::new("  ·  ", style_faint()),
            Seg::new("Esc", style_key()),
            Seg::new(" 跳过", style_dim()),
        ];
        match self.kind() {
            Some(StepKind::Text { .. }) => {
                footer.push(Seg::new("  ·  ", style_faint()));
                footer.push(Seg::new("Backspace", style_key()));
                footer.push(Seg::new(" 删字", style_dim()));
            }
            Some(StepKind::Choice { options }) if options.len() > 1 => {
                footer.push(Seg::new("  ·  ", style_faint()));
                footer.push(Seg::new("↑↓/Tab", style_key()));
                footer.push(Seg::new(" 换选项", style_dim()));
            }
            Some(StepKind::YesNo { .. }) => {
                footer.push(Seg::new("  ·  ", style_faint()));
                footer.push(Seg::new("←→/y/n", style_key()));
                footer.push(Seg::new(" 切换", style_dim()));
            }
            _ => {}
        }
        footer
    }

    /// 画圆角框（复刻欢迎界面的圆角观感）
    fn paint_frame(
        &self,
        grid: &mut StarGrid,
        left: usize,
        top: usize,
        box_width: usize,
        box_height: usize,
    ) {
        if box_width < 2 || box_height == 0 {
            return;
        }
        let right = left + box_width - 1;
        let border = style_border();

        for row in 0..box_height {
            let y = top + row;
            match row {
                0 => {
                    grid.put(left, y, "╭", border);
                    for x in (left + 1)..right {
                        grid.put(x, y, "─", border);
                    }
                    grid.put(right, y, "╮", border);
                }
                row if row + 1 == box_height => {
                    grid.put(left, y, "╰", border);
                    for x in (left + 1)..right {
                        grid.put(x, y, "─", border);
                    }
                    grid.put(right, y, "╯", border);
                }
                _ => {
                    grid.put(left, y, "│", border);
                    grid.put(right, y, "│", border);
                }
            }
        }

        // 框顶嵌一行小标：云熙配置向导
        let label = " 云熙配置向导 ";
        let label_width = TextLayout::measure(label);
        if box_width > label_width + 4 {
            grid.put_text(left + 2, top, &[Seg::new(label, style_accent())]);
        }
    }

    /// 画框内正文
    fn paint_body(
        &self,
        grid: &mut StarGrid,
        left: usize,
        top: usize,
        box_width: usize,
        box_height: usize,
        body: &[Vec<Seg>],
    ) {
        let inner_width = box_width.saturating_sub(2 * (FRAME_PAD_X + 1));
        let x = left + FRAME_PAD_X + 1;
        let available = box_height.saturating_sub(2 + 2 * FRAME_PAD_Y);
        let start = top + 1 + FRAME_PAD_Y;

        for (offset, line) in body.iter().take(available).enumerate() {
            let y = start + offset;
            grid.put_text(x, y, line);
            // 文本行不足处补空格：行内已经按 inner_width 对齐，尾随空格只为好看
            let used: usize = line.iter().map(|seg| TextLayout::measure(&seg.text)).sum();
            if used < inner_width {
                grid.put_text(
                    x + used,
                    y,
                    &[Seg::new(" ".repeat(inner_width - used), Style::new())],
                );
            }
        }
    }

    /// 画页脚（底色横条 + 居中按键提示）
    fn paint_footer(
        &self,
        grid: &mut StarGrid,
        left: usize,
        top: usize,
        box_width: usize,
        box_height: usize,
    ) {
        if box_height < 3 {
            return;
        }
        let y = top + box_height - 1;
        let strip_start = left + 1;
        let strip_width = box_width.saturating_sub(2);
        if strip_width == 0 {
            return;
        }

        let footer = self.footer_line();
        let footer_width: usize = footer
            .iter()
            .map(|seg| TextLayout::measure(&seg.text))
            .sum();
        // 提示过长就尽量保留（截断而不是换行，底部只有一行）
        let footer = if footer_width > strip_width {
            let mut cut = Vec::new();
            let mut used = 0;
            for seg in footer {
                let width = TextLayout::measure(&seg.text);
                if used + width <= strip_width {
                    used += width;
                    cut.push(seg);
                } else if used < strip_width {
                    let text = TextLayout::truncate(&seg.text, strip_width - used);
                    used += TextLayout::measure(&text);
                    cut.push(Seg::new(text, seg.style));
                }
            }
            cut
        } else {
            footer
        };
        let footer_width: usize = footer
            .iter()
            .map(|seg| TextLayout::measure(&seg.text))
            .sum();
        let pad = strip_width.saturating_sub(footer_width) / 2;

        let strip = Style::default().bg(SELECTION_BG);
        // 整条底纹（正文只盖到 `box_height - 2`，所以页脚要自己把这一行铺满）
        grid.put_text(strip_start, y, &[Seg::new(" ".repeat(strip_width), strip)]);
        // 居中提示（`put_text` 自带右边界裁剪）
        let mut line = vec![Seg::new(" ".repeat(pad), strip)];
        line.extend(
            footer
                .into_iter()
                .map(|seg| Seg::new(seg.text, strip.patch(seg.style))),
        );
        grid.put_text(strip_start, y, &line);
    }
}

impl Default for OnboardingWizard {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

// ─────────────────── 星空底 ───────────────────

/// 一屏的「一格子一单元」缓冲：先铺星空，再往上盖章
///
/// 与 `welcome.rs` 同一套星空（[`star_seg`]），所以向导和欢迎界面的底纹是同一种
/// 质感。格子按**显示宽度**推进：中文占两格，右格是续格（[`Cell::Hold`]，输出成
/// 一个空格）。续格不能再被写入，否则「第几列」和实际列号会错开、整行宽度溢出。
struct StarGrid {
    cols: usize,
    rows: usize,
    cells: Vec<Vec<Cell>>,
}

/// 一个屏幕单元
#[derive(Clone, Debug)]
enum Cell {
    /// 这一格是某个宽字素的右半边，输出成空格占位
    Hold,
    Glyph(Seg),
}

impl StarGrid {
    fn new(cols: usize, rows: usize) -> Self {
        let cells = (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| Cell::Glyph(star_seg(x, y, 0, STAR_SCALE, STAR_SPARSITY)))
                    .collect()
            })
            .collect();
        Self { cols, rows, cells }
    }

    /// 写一个字符（只用于框线这类单宽字符）
    fn put(&mut self, x: usize, y: usize, text: &str, style: Style) {
        self.put_text(x, y, &[Seg::new(text, style)]);
    }

    /// 从 `x` 开始盖一串片段（右侧超出屏幕的部分丢弃）
    fn put_text(&mut self, x: usize, y: usize, segs: &[Seg]) {
        let Some(row) = self.cells.get_mut(y) else {
            return;
        };
        let mut cursor = x;
        for seg in segs {
            for grapheme in seg.text.graphemes(true) {
                // 超出屏幕就停：按显示宽度判断，中文一个字占两列
                if cursor >= self.cols {
                    return;
                }
                let width = TextLayout::measure(grapheme);
                if width == 0 {
                    continue;
                }
                row[cursor] = Cell::Glyph(Seg::new(grapheme, seg.style));
                for extra in 1..width {
                    if cursor + extra < self.cols {
                        row[cursor + extra] = Cell::Hold;
                    }
                }
                cursor += width;
            }
        }
    }

    /// 收成「屏幕行 → 片段」：按列序拼，同款式相邻格合并（少打 ANSI 序列）
    fn into_rows(self) -> Vec<Vec<Seg>> {
        let mut rows = Vec::with_capacity(self.rows);
        for row in self.cells {
            let mut merged: Vec<Seg> = Vec::new();
            let mut pending = String::new();
            let mut pending_style: Option<Style> = None;
            for cell in row {
                let seg = match cell {
                    // 续格在屏幕上是「宽字素的右半边」，不占新的一列
                    Cell::Hold => Seg::raw(""),
                    Cell::Glyph(seg) => seg,
                };
                match &pending_style {
                    Some(style) if *style == seg.style => pending.push_str(&seg.text),
                    Some(_) => {
                        if let Some(style) = pending_style.take() {
                            merged.push(Seg::new(std::mem::take(&mut pending), style));
                        }
                        pending_style = Some(seg.style);
                        pending.push_str(&seg.text);
                    }
                    None => {
                        pending_style = Some(seg.style);
                        pending.push_str(&seg.text);
                    }
                }
            }
            if let Some(style) = pending_style {
                merged.push(Seg::new(pending, style));
            }
            rows.push(merged);
        }
        rows
    }
}

// ─────────────────── 文本工具 ───────────────────

/// 超宽时保留尾部（输入框里光标在末尾，尾部才是用户刚敲的）
fn tail_clamp(value: &str, width: usize) -> String {
    if TextLayout::measure(value) <= width {
        return value.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let ellipsis = "…";
    let room = width.saturating_sub(TextLayout::measure(ellipsis));
    let mut tail: Vec<&str> = Vec::new();
    let mut used = 0;
    for grapheme in value.graphemes(true).rev() {
        let w = TextLayout::measure(grapheme);
        if used + w > room {
            break;
        }
        used += w;
        tail.push(grapheme);
    }
    tail.reverse();
    format!("{ellipsis}{}", tail.concat())
}

/// 按显示宽度折行（中英混排都能断）
///
/// 用 [`TextLayout::measure`] 而非 `String::len()`：中文一个字占 3 字节但只有 2 列，
/// 按字节折行会让中文说明行提前断掉。
fn wrap_text(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0usize;

    for word in value.split_whitespace() {
        // 词间空格等到真正落笔时再补：这里若换行，行尾就不留空格
        let gap = usize::from(line_width > 0);
        if line_width + gap + TextLayout::measure(word) > width {
            if line_width > 0 {
                lines.push(std::mem::take(&mut line));
                line_width = 0;
            }
        } else if gap > 0 {
            line.push(' ');
            line_width += gap;
        }

        // 逐个字素累加：显示宽度不够就先收束当前行再续
        for grapheme in word.graphemes(true) {
            let grapheme_width = TextLayout::measure(grapheme);
            if line_width > 0 && line_width + grapheme_width > width {
                lines.push(std::mem::take(&mut line));
                line_width = 0;
            }
            line.push_str(grapheme);
            line_width += grapheme_width;
        }
    }

    if line_width > 0 {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

// ─────────────────── 配色（与 welcome.rs 同一套） ───────────────────

/// 框线：深底到柔和紫的中间调，够淡但看得见
fn style_border() -> Style {
    lerp_color(YUNXI_INK, YUNXI_PURPLE, 0.45)
}

/// 标题：银白加粗
fn style_title() -> Style {
    lerp_color(YUNXI_INK, YUNXI_SILVER, 0.95).add_modifier(Modifier::BOLD)
}

/// 进度、说明这类辅助文字
fn style_dim() -> Style {
    lerp_color(YUNXI_INK, YUNXI_LAVENDER, 0.6)
}

/// 更淡的装饰（分隔符）
fn style_faint() -> Style {
    lerp_color(YUNXI_INK, YUNXI_LAVENDER, 0.35)
}

/// 「为什么问这个」的正文说明
fn style_hint() -> Style {
    lerp_color(YUNXI_INK, YUNXI_LAVENDER, 0.75)
}

/// 小标题 / 强调字（进度数字、框顶品牌名）
fn style_accent() -> Style {
    lerp_color(YUNXI_INK, YUNXI_PURPLE, 0.95)
}

/// 用户输入的真实内容
fn style_value() -> Style {
    lerp_color(YUNXI_INK, YUNXI_WHITE, 1.0)
}

/// 输入光标
fn style_cursor() -> Style {
    lerp_color(YUNXI_INK, YUNXI_SILVER, 1.0)
}

/// 占位提示
fn style_placeholder() -> Style {
    lerp_color(YUNXI_INK, YUNXI_LAVENDER, 0.4).add_modifier(Modifier::ITALIC)
}

/// 未选中的选项
fn style_choice() -> Style {
    lerp_color(YUNXI_INK, YUNXI_LAVENDER, 0.85)
}

/// 选中的选项（银白 + 加粗，背景由调用方补）
fn style_selected() -> Style {
    lerp_color(YUNXI_INK, YUNXI_WHITE, 1.0).add_modifier(Modifier::BOLD)
}

/// 按键提示里的键名
fn style_key() -> Style {
    lerp_color(YUNXI_INK, YUNXI_PURPLE, 0.95).add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    const COLS: usize = 80;
    const ROWS: usize = 24;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn press(wizard: &mut OnboardingWizard, code: KeyCode) -> Option<OnboardingOutcome> {
        wizard.handle_key(key(code))
    }

    fn type_text(wizard: &mut OnboardingWizard, text: &str) {
        for ch in text.chars() {
            press(wizard, KeyCode::Char(ch));
        }
    }

    /// 把一屏拼成纯文本（每行一个 `String`，便于断言界面内容）
    fn screen(wizard: &OnboardingWizard) -> Vec<String> {
        wizard
            .render(COLS, ROWS)
            .iter()
            .map(|row| row.iter().map(|seg| seg.text.as_str()).collect::<String>())
            .collect()
    }

    fn screen_text(wizard: &OnboardingWizard) -> String {
        screen(wizard).join("\n")
    }

    /// 与宿主（T3）要准备的步骤同构的演示步骤
    fn demo_steps() -> Vec<OnboardingStep> {
        vec![
            OnboardingStep::masked(
                OnboardingStepId::ApiKey,
                "先连上模型",
                "没有凭证后面全是空谈，当场验证一次最省事。",
                "sk-...",
            ),
            OnboardingStep::new(
                OnboardingStepId::ApiBaseUrl,
                "接口地址",
                "自建网关或代理时改这里，留空就用官方地址。",
                StepKind::Text {
                    masked: false,
                    placeholder: "https://api.deepseek.com".to_string(),
                },
            ),
            OnboardingStep::new(
                OnboardingStepId::ApiModel,
                "模型名",
                "留空就用默认模型，之后可以用 /status 改。",
                StepKind::Text {
                    masked: false,
                    placeholder: "deepseek-chat".to_string(),
                },
            ),
            OnboardingStep::choice(
                OnboardingStepId::Profile,
                "挑一个人格",
                "人格决定我说话的语气和在意的东西。",
                vec![
                    ChoiceOption::new(
                        "yunxi_companion_strong",
                        "云熙 · 强陪伴",
                        "话多一点，主动关心你",
                    ),
                    ChoiceOption::new("yunxi_focus", "云熙 · 专注", "少寒暄，直接干活"),
                    ChoiceOption::new("yunxi_quiet", "云熙 · 安静", "只在你叫我的时候出现"),
                ],
            ),
            OnboardingStep::text(
                OnboardingStepId::UserCard,
                "关于你",
                "这几句会写进长期记忆，之后不用反复自我介绍。",
            ),
            OnboardingStep::yes_no(
                OnboardingStepId::Memory,
                "记忆库",
                "关掉也能用，但我就记不住你的习惯了。",
                true,
            ),
            OnboardingStep::yes_no(
                OnboardingStepId::Knowledge,
                "知识库",
                "现在不接也行，之后随时能从 /status 进来。",
                false,
            ),
        ]
    }

    /// 用一套「最小五步」把向导走完
    fn walk_through_api_only() -> Vec<OnboardingStep> {
        vec![
            OnboardingStep::masked(
                OnboardingStepId::ApiKey,
                "先连上模型",
                "先给凭证。",
                "sk-...",
            ),
            OnboardingStep::text(OnboardingStepId::ApiBaseUrl, "接口地址", "留空用官方。"),
            OnboardingStep::text(OnboardingStepId::ApiModel, "模型名", "留空用默认。"),
        ]
    }

    #[test]
    fn five_step_walkthrough_returns_completed_with_every_field() {
        let mut wizard = OnboardingWizard::new(demo_steps());

        // 1 / 7：API key
        assert_eq!(wizard.progress(), (1, 7));
        type_text(&mut wizard, "sk-live-123");
        assert_eq!(wizard.buffer(), "sk-live-123");
        assert!(press(&mut wizard, KeyCode::Enter).is_none());

        // 2 / 7：base url
        assert_eq!(wizard.progress(), (2, 7));
        type_text(&mut wizard, "https://api.example.com/v1");
        assert!(press(&mut wizard, KeyCode::Enter).is_none());

        // 3 / 7：模型名
        assert_eq!(wizard.progress(), (3, 7));
        type_text(&mut wizard, "deepseek-chat");
        assert!(press(&mut wizard, KeyCode::Enter).is_none());

        // 4 / 7：人格（用 ↓ 选到第二项）
        assert_eq!(wizard.progress(), (4, 7));
        press(&mut wizard, KeyCode::Down);
        assert!(press(&mut wizard, KeyCode::Enter).is_none());

        // 5 / 7：用户卡片，三小问
        assert_eq!(wizard.progress(), (5, 7));
        type_text(&mut wizard, "小云");
        assert!(press(&mut wizard, KeyCode::Enter).is_none());
        assert_eq!(wizard.progress(), (5, 7), "用户卡片内部不跳步");
        assert_eq!(wizard.answers().user_name, "小云");
        type_text(&mut wizard, "在写 Rust");
        assert!(press(&mut wizard, KeyCode::Enter).is_none());
        type_text(&mut wizard, "回答简短一点");
        assert!(press(&mut wizard, KeyCode::Enter).is_none());

        // 6 / 7：记忆库
        assert_eq!(wizard.progress(), (6, 7));
        assert!(wizard.selected_yes(), "默认应为是");
        assert!(press(&mut wizard, KeyCode::Enter).is_none());

        // 7 / 7：知识库
        assert_eq!(wizard.progress(), (7, 7));
        assert!(wizard.is_last());
        assert!(!wizard.selected_yes(), "默认应为否");
        let outcome = press(&mut wizard, KeyCode::Enter).expect("最后一步应返回结果");

        let OnboardingOutcome::Completed(answers) = outcome else {
            panic!("应返回 Completed：{outcome:?}");
        };
        assert_eq!(answers.api_key, "sk-live-123");
        assert_eq!(answers.base_url, "https://api.example.com/v1");
        assert_eq!(answers.model, "deepseek-chat");
        assert_eq!(answers.profile, "yunxi_focus");
        assert_eq!(answers.user_name, "小云");
        assert_eq!(answers.user_identity, "在写 Rust");
        assert_eq!(answers.user_preference, "回答简短一点");
        assert!(answers.memory_enabled);
        assert!(!answers.knowledge_enabled);
    }

    #[test]
    fn esc_returns_skipped_with_the_step_the_user_stood_on() {
        let mut wizard = OnboardingWizard::new(demo_steps());
        type_text(&mut wizard, "sk-abc");
        assert!(press(&mut wizard, KeyCode::Enter).is_none());
        assert_eq!(wizard.current_id(), Some(&OnboardingStepId::ApiBaseUrl));

        let outcome = press(&mut wizard, KeyCode::Esc).expect("Esc 应返回结果");
        assert_eq!(
            outcome,
            OnboardingOutcome::Skipped {
                at_step: OnboardingStepId::ApiBaseUrl
            }
        );
    }

    #[test]
    fn esc_on_a_later_step_reports_that_step() {
        let mut wizard = OnboardingWizard::new(vec![
            OnboardingStep::text(OnboardingStepId::UserName, "称呼", "叫我什么。"),
            OnboardingStep::yes_no(OnboardingStepId::Memory, "记忆库", "要不要记。", true),
        ]);
        type_text(&mut wizard, "小云");
        press(&mut wizard, KeyCode::Enter);
        assert_eq!(wizard.current_id(), Some(&OnboardingStepId::Memory));
        assert_eq!(
            press(&mut wizard, KeyCode::Esc),
            Some(OnboardingOutcome::Skipped {
                at_step: OnboardingStepId::Memory
            })
        );
    }

    #[test]
    fn masked_input_echoes_dots_and_never_leaks_the_secret() {
        let mut wizard = OnboardingWizard::new(walk_through_api_only());
        type_text(&mut wizard, "sk-super-secret");
        assert_eq!(wizard.buffer(), "sk-super-secret", "内部必须保留真实值");

        let rendered = screen_text(&wizard);
        assert!(!rendered.contains("sk-super-secret"), "回显不能出现明文");
        assert!(!rendered.contains("super"), "回显不能出现明文片段");
        assert!(
            rendered.contains(&MASK_GLYPH.repeat("sk-super-secret".len())),
            "应该回显等长的圆点：\n{rendered}"
        );
    }

    #[test]
    fn unmasked_input_echoes_what_the_user_typed() {
        let mut wizard = OnboardingWizard::new(walk_through_api_only());
        type_text(&mut wizard, "sk-abc");
        press(&mut wizard, KeyCode::Enter);
        type_text(&mut wizard, "https://api.example.com");
        assert!(screen_text(&wizard).contains("https://api.example.com"));
    }

    #[test]
    fn arrow_keys_cycle_choice_options() {
        let steps = vec![OnboardingStep::choice(
            OnboardingStepId::Profile,
            "挑一个人格",
            "人格决定语气。",
            vec![
                ChoiceOption::new("a", "甲", "第一个"),
                ChoiceOption::new("b", "乙", "第二个"),
                ChoiceOption::new("c", "丙", "第三个"),
            ],
        )];
        let mut wizard = OnboardingWizard::new(steps);
        assert_eq!(wizard.selected_index(), 0);

        press(&mut wizard, KeyCode::Down);
        assert_eq!(wizard.selected_index(), 1);
        assert!(
            screen_text(&wizard).contains("第二个"),
            "detail 应显示在下方"
        );

        press(&mut wizard, KeyCode::Tab);
        assert_eq!(wizard.selected_index(), 2);
        // 首尾相接
        press(&mut wizard, KeyCode::Down);
        assert_eq!(wizard.selected_index(), 0);
        press(&mut wizard, KeyCode::Up);
        assert_eq!(wizard.selected_index(), 2);
        press(&mut wizard, KeyCode::BackTab);
        assert_eq!(wizard.selected_index(), 1);

        // 单选步骤（只有一步）Enter 直接完成
        let outcome = press(&mut wizard, KeyCode::Enter).expect("单步向导 Enter 应完成");
        let OnboardingOutcome::Completed(answers) = outcome else {
            panic!("应返回 Completed");
        };
        assert_eq!(answers.profile, "b");
    }

    #[test]
    fn yes_no_switches_with_left_right_and_y_n() {
        let steps = vec![OnboardingStep::yes_no(
            OnboardingStepId::Memory,
            "记忆库",
            "要不要记。",
            true,
        )];
        let mut wizard = OnboardingWizard::new(steps);
        assert!(wizard.selected_yes(), "默认是");

        press(&mut wizard, KeyCode::Left);
        assert!(!wizard.selected_yes(), "← 应切到否");
        press(&mut wizard, KeyCode::Right);
        assert!(wizard.selected_yes(), "→ 应切到是");

        // y / n 直接选定并前进（单步向导 = 完成）
        let outcome = press(&mut wizard, KeyCode::Char('n')).expect("n 应提交");
        let OnboardingOutcome::Completed(answers) = outcome else {
            panic!("应返回 Completed");
        };
        assert!(!answers.memory_enabled, "n 应写入 false");
    }

    #[test]
    fn y_and_n_do_not_land_in_text_buffers() {
        let steps = vec![OnboardingStep::text(
            OnboardingStepId::UserName,
            "称呼",
            "叫我什么。",
        )];
        let mut wizard = OnboardingWizard::new(steps);
        type_text(&mut wizard, "yy");
        assert_eq!(wizard.buffer(), "yy", "文本步骤里 y 就是普通字符");
    }

    #[test]
    fn backspace_edits_text_by_grapheme() {
        let steps = vec![OnboardingStep::text(
            OnboardingStepId::UserName,
            "称呼",
            "叫我什么。",
        )];
        let mut wizard = OnboardingWizard::new(steps);
        type_text(&mut wizard, "小云a");
        assert_eq!(wizard.buffer(), "小云a");
        press(&mut wizard, KeyCode::Backspace);
        assert_eq!(wizard.buffer(), "小云");
        press(&mut wizard, KeyCode::Backspace);
        // 中文按字素簇删除：一次退一整字，不留半个
        assert_eq!(wizard.buffer(), "小");
        press(&mut wizard, KeyCode::Backspace);
        assert_eq!(wizard.buffer(), "");
        // 空缓冲上再退格不应崩
        press(&mut wizard, KeyCode::Backspace);
        assert_eq!(wizard.buffer(), "");
        assert_eq!(wizard.answers().user_name, "");
    }

    #[test]
    fn empty_text_step_does_not_advance_and_esc_skips() {
        let mut wizard = OnboardingWizard::new(walk_through_api_only());
        assert!(press(&mut wizard, KeyCode::Enter).is_none(), "空输入不前进");
        assert_eq!(wizard.progress(), (1, 3));
        assert_eq!(
            press(&mut wizard, KeyCode::Esc),
            Some(OnboardingOutcome::Skipped {
                at_step: OnboardingStepId::ApiKey
            })
        );
    }

    #[test]
    fn typed_text_survives_a_round_trip_between_steps() {
        let mut wizard = OnboardingWizard::new(vec![
            OnboardingStep::text(OnboardingStepId::ApiBaseUrl, "接口地址", "留空用官方。"),
            OnboardingStep::text(OnboardingStepId::ApiModel, "模型名", "留空用默认。"),
        ]);
        type_text(&mut wizard, "https://a.example");
        press(&mut wizard, KeyCode::Enter);
        assert_eq!(wizard.buffer(), "", "新步骤应该是空缓冲");
        type_text(&mut wizard, "m1");
        assert_eq!(wizard.answers().base_url, "https://a.example");
    }

    #[test]
    fn release_events_and_control_chords_are_ignored() {
        let steps = vec![OnboardingStep::text(
            OnboardingStepId::UserName,
            "称呼",
            "叫我什么。",
        )];
        let mut wizard = OnboardingWizard::new(steps);

        let release = KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };
        assert!(wizard.handle_key(release).is_none());
        assert_eq!(wizard.buffer(), "", "Release 不应写入");

        let ctrl_c = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        assert!(wizard.handle_key(ctrl_c).is_none());
        assert_eq!(wizard.buffer(), "", "Ctrl+C 不应进输入框");
    }

    #[test]
    fn render_returns_exactly_rows_by_cols() {
        let mut wizard = OnboardingWizard::new(demo_steps());
        for (cols, rows) in [(80, 24), (100, 40), (80, 24), (60, 20), (40, 12)] {
            let output = wizard.render(cols, rows);
            assert_eq!(output.len(), rows, "{cols}x{rows} 行数不对");
            for (y, row) in output.iter().enumerate() {
                let width: usize = row.iter().map(|seg| TextLayout::measure(&seg.text)).sum();
                assert_eq!(width, cols, "{cols}x{rows} 第 {y} 行宽度不是 {cols}");
            }
        }
        // 走到下一步再量一次（内容换了，尺寸契约不能变）
        press(&mut wizard, KeyCode::Enter);
        for (cols, rows) in [(80, 24), (33, 9)] {
            let output = wizard.render(cols, rows);
            assert_eq!(output.len(), rows);
            for row in &output {
                let width: usize = row.iter().map(|seg| TextLayout::measure(&seg.text)).sum();
                assert_eq!(width, cols);
            }
        }
    }

    #[test]
    fn every_screen_shows_progress_title_hint_and_footer() {
        let mut wizard = OnboardingWizard::new(demo_steps());
        let total = demo_steps().len();

        for step in 1..=total {
            let text = screen_text(&wizard);
            assert!(
                text.contains(&format!("第 {step} 步")) && text.contains(&format!("共 {total} 步")),
                "第 {step} 步缺少进度：\n{text}"
            );
            assert!(
                text.contains(&wizard.current_step().expect("step").title),
                "第 {step} 步缺少标题：\n{text}"
            );
            assert!(
                !wizard.current_step().expect("step").hint.is_empty()
                    && text.contains(&wizard.current_step().expect("step").hint),
                "第 {step} 步缺少说明：\n{text}"
            );
            assert!(text.contains("Esc"), "第 {step} 步缺少按键提示：\n{text}");
            if step == total {
                assert!(text.contains("Enter 完成"), "最后一步应显示完成：\n{text}");
            } else {
                assert!(
                    text.contains("Enter 下一步"),
                    "第 {step} 步应显示下一步：\n{text}"
                );
            }

            // 推进到下一步（用户卡片有多个小问，循环按 Enter 直到进度变化）
            let (before, _) = wizard.progress();
            for _ in 0..8 {
                press(&mut wizard, KeyCode::Enter);
                if wizard.progress() != (before, total) {
                    break;
                }
                type_text(&mut wizard, "x");
            }
        }
    }

    #[test]
    fn narrow_terminal_does_not_overflow_or_panic() {
        let mut wizard = OnboardingWizard::new(demo_steps());
        for (cols, rows) in [(1, 1), (20, 5), (33, 8), (80, 3), (120, 2)] {
            let output = wizard.render(cols, rows);
            assert_eq!(output.len(), rows, "{cols}x{rows}");
            for row in &output {
                let width: usize = row.iter().map(|seg| TextLayout::measure(&seg.text)).sum();
                assert_eq!(width, cols, "{cols}x{rows} 行宽溢出");
            }
        }
    }

    #[test]
    fn chinese_hint_is_centered_by_display_width_not_bytes() {
        // 回归：用 `String::len()` 算宽度会让中文行往左偏（welcome.rs 修过同类 bug）
        let wizard = OnboardingWizard::new(vec![OnboardingStep::text(
            OnboardingStepId::UserName,
            "称呼",
            "叫我什么。",
        )]);
        let lines = screen(&wizard);
        let box_row = lines
            .iter()
            .find(|line| line.contains('╭'))
            .expect("应有圆角框");
        // 列号必须按**显示宽度**量：行首有星星，`find` 给的是字节偏移
        let box_left = TextLayout::measure(&box_row[..box_row.find('╭').expect("左圆角")]);
        let right_byte = box_row.rfind('╮').expect("右圆角");
        let box_width =
            TextLayout::measure(&box_row[box_row.find('╭').expect("左圆角")..right_byte + 3]);
        assert_eq!(box_left, (COLS - box_width) / 2, "框应水平居中");
        assert_eq!(
            box_width,
            (COLS * 4 / 5).min(FRAME_MAX_WIDTH),
            "框宽应是屏幕的 80%"
        );

        // 框内正文不能顶到框线
        let title_row = lines
            .iter()
            .find(|line| line.contains("称呼"))
            .expect("应有标题行");
        let title_col = TextLayout::measure(&title_row[..title_row.find("称呼").expect("标题列")]);
        assert!(title_col > box_left + 1, "标题应缩进在框内：\n{title_row}");
        assert!(
            title_col < box_left + box_width,
            "标题不应越过右框线：\n{title_row}"
        );
    }

    #[test]
    fn user_card_walks_three_prompts_inside_one_step() {
        let mut wizard = OnboardingWizard::new(vec![OnboardingStep::text(
            OnboardingStepId::UserCard,
            "关于你",
            "写进长期记忆。",
        )]);
        assert_eq!(wizard.progress(), (1, 1));
        assert!(screen_text(&wizard).contains("称呼"), "第一小问是称呼");

        type_text(&mut wizard, "小云");
        press(&mut wizard, KeyCode::Enter);
        assert_eq!(wizard.progress(), (1, 1), "进度不应变化");
        assert!(screen_text(&wizard).contains("做什么"), "第二小问是身份");
        assert!(screen_text(&wizard).contains("1/3") || screen_text(&wizard).contains("2/3"));

        type_text(&mut wizard, "工程师");
        press(&mut wizard, KeyCode::Enter);
        type_text(&mut wizard, "喜欢简短");
        let outcome = press(&mut wizard, KeyCode::Enter).expect("三小问走完应完成");
        let OnboardingOutcome::Completed(answers) = outcome else {
            panic!("应返回 Completed");
        };
        assert_eq!(answers.user_name, "小云");
        assert_eq!(answers.user_identity, "工程师");
        assert_eq!(answers.user_preference, "喜欢简短");
    }

    #[test]
    fn empty_step_list_renders_blank_and_skips() {
        let mut wizard = OnboardingWizard::default();
        assert_eq!(wizard.progress(), (0, 0));
        assert!(wizard.is_last());
        assert_eq!(wizard.render(COLS, ROWS).len(), ROWS);
        assert_eq!(
            press(&mut wizard, KeyCode::Esc),
            Some(OnboardingOutcome::Skipped {
                at_step: OnboardingStepId::Custom(String::new())
            })
        );
    }

    #[test]
    fn long_input_scrolls_to_keep_the_tail_visible() {
        let mut wizard = OnboardingWizard::new(walk_through_api_only());
        let long = "sk-".to_string() + &"9".repeat(300);
        type_text(&mut wizard, &long);
        assert_eq!(wizard.buffer(), long, "缓冲保留完整值");
        let text = screen_text(&wizard);
        assert!(text.contains("…"), "超长输入应有省略号：\n{text}");
        // API key 是遮掩字段，渲染出来是圆点而不是原文 —— 所以断言应该看
        // **圆点的数量**是否够多（尾部保留），而不是找 '9'。
        let dots = text.chars().filter(|c| *c == '•').count();
        assert!(
            dots >= 10,
            "遮掩输入应保留尾部圆点，实际 {dots} 个：\n{text}"
        );
    }

    #[test]
    fn choice_detail_updates_with_the_highlighted_option() {
        let steps = vec![OnboardingStep::choice(
            OnboardingStepId::Profile,
            "挑一个人格",
            "人格决定语气。",
            vec![
                ChoiceOption::new("a", "甲", "甲的一句话说"),
                ChoiceOption::new("b", "乙", "乙的一句话说"),
            ],
        )];
        let mut wizard = OnboardingWizard::new(steps);
        assert!(screen_text(&wizard).contains("甲的一句话说"));
        press(&mut wizard, KeyCode::Down);
        let text = screen_text(&wizard);
        assert!(text.contains("乙的一句话说"));
        assert!(!text.contains("甲的一句话说"), "detail 只显示当前项");
    }
}
