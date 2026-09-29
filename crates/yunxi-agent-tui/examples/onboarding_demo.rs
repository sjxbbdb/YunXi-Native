//! 云熙配置向导演示 - 独立跑一遍五个步骤
//!
//! 运行: cargo run --release --locked -p yunxi-agent-tui --example onboarding_demo
//!
//! 演示的是方案第二节里的五步：先测 API（API key / base url / 模型名）、人格、
//! 关于你（称呼 / 身份 / 偏好）、记忆库、知识库。这个 example 只用到向导自己的
//! 渲染与键盘收集，不碰文件系统、不碰网络 —— 走完之后打印收集到的答案。
//!
//! 按键：Enter 确认 / Esc 跳过（演示会打印 Skipped）/ Ctrl+C 退出。

use std::io::{self, Write};
use std::time::Duration;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{Event, KeyCode, KeyEvent, KeyModifiers, poll, read},
    execute,
    style::Print,
    terminal::{
        self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use yunxi_agent_tui::onboarding::{
    ChoiceOption, OnboardingAnswers, OnboardingOutcome, OnboardingStep, OnboardingStepId,
    OnboardingWizard, StepKind,
};
use yunxi_agent_tui::yunxi_starfield::Seg;

fn main() -> io::Result<()> {
    // 演示也能在非 tty（比如被 pyte 直接喂 stdin）里跑：拿不到尺寸就退回 80x24
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, Hide, Clear(ClearType::All))?;

    let result = run(&mut stdout, cols as usize, rows as usize);

    let _ = execute!(stdout, Show, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    result
}

/// 事件循环：每次按键重画一屏，直到向导给出结果
fn run(stdout: &mut io::Stdout, start_cols: usize, start_rows: usize) -> io::Result<()> {
    let steps = demo_steps();
    let mut wizard = OnboardingWizard::new(steps);
    let (mut cols, mut rows) = (start_cols.max(1), start_rows.max(1));
    let mut dirty = true;

    loop {
        if dirty {
            paint(stdout, &wizard, cols, rows)?;
            dirty = false;
        }

        // 100ms 一轮：既有动画帧刷新（星空会闪），也不至于空转烧 CPU
        if !poll(Duration::from_millis(100))? {
            dirty = true;
            continue;
        }
        match read()? {
            Event::Key(key) => {
                if is_quit(&key) {
                    return Ok(());
                }
                if let Some(outcome) = wizard.handle_key(key) {
                    paint_outcome(stdout, &outcome, cols, rows)?;
                    wait_any_key()?;
                    return Ok(());
                }
                dirty = true;
            }
            Event::Resize(new_cols, new_rows) => {
                cols = (new_cols as usize).max(1);
                rows = (new_rows as usize).max(1);
                let _ = execute!(stdout, Clear(ClearType::All));
                dirty = true;
            }
            _ => {}
        }
    }
}

/// 画一屏向导界面
fn paint(
    stdout: &mut io::Stdout,
    wizard: &OnboardingWizard,
    cols: usize,
    rows: usize,
) -> io::Result<()> {
    let output = wizard.render(cols, rows);
    execute!(stdout, BeginSynchronizedUpdate)?;
    // 先整屏清一遍：行数变少时不会留下上一屏的残影
    execute!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    for (y, line) in output.iter().enumerate().take(rows) {
        execute!(stdout, MoveTo(0, y as u16), Print(segs_to_string(line)))?;
    }
    // 光标停在底部，方便用户看到「界面在等我」
    execute!(
        stdout,
        MoveTo(0, rows.saturating_sub(1) as u16),
        EndSynchronizedUpdate
    )?;
    stdout.flush()
}

/// 走完之后把收集到的答案摊开给人看（同时验证字段真的对上了）
fn paint_outcome(
    stdout: &mut io::Stdout,
    outcome: &OnboardingOutcome,
    cols: usize,
    rows: usize,
) -> io::Result<()> {
    let lines: Vec<String> = match outcome {
        OnboardingOutcome::Completed(answers) => completed_lines(answers),
        OnboardingOutcome::Skipped { at_step } => vec![
            String::new(),
            "  ── 向导已跳过 ──".to_string(),
            format!("  用户在 {at_step:?} 这一步按了 Esc"),
            String::new(),
            "  宿主应当：提示后果（比如没有凭证无法对话），".to_string(),
            "  然后决定是进会话还是退出。".to_string(),
        ],
    };

    execute!(stdout, BeginSynchronizedUpdate)?;
    execute!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    let top = rows.saturating_sub(lines.len()) / 2;
    for (offset, line) in lines.iter().enumerate() {
        let y = top + offset;
        if y >= rows {
            break;
        }
        execute!(
            stdout,
            MoveTo(0, y as u16),
            Clear(ClearType::UntilNewLine),
            Print(truncate_to(line, cols))
        )?;
    }
    execute!(
        stdout,
        MoveTo(0, rows.saturating_sub(1) as u16),
        EndSynchronizedUpdate
    )?;
    stdout.flush()
}

/// 完成页的正文
fn completed_lines(answers: &OnboardingAnswers) -> Vec<String> {
    // API key 只显示头尾：这是演示，也不该把凭证打到屏幕上
    let shown_key = if answers.api_key.chars().count() > 8 {
        let head: String = answers.api_key.chars().take(4).collect();
        let tail: String = answers
            .api_key
            .chars()
            .skip(answers.api_key.chars().count() - 4)
            .collect();
        format!(
            "{head}…{tail}（共 {} 字符）",
            answers.api_key.chars().count()
        )
    } else {
        "（未填）".to_string()
    };
    let blank = |value: &str| {
        if value.trim().is_empty() {
            "（留空，用默认）".to_string()
        } else {
            value.to_string()
        }
    };

    vec![
        String::new(),
        "  ── 向导完成，宿主收到这些答案 ──".to_string(),
        String::new(),
        format!("  api_key        {shown_key}"),
        format!("  base_url       {}", blank(&answers.base_url)),
        format!("  model          {}", blank(&answers.model)),
        format!("  profile        {}", blank(&answers.profile)),
        format!("  user_name      {}", blank(&answers.user_name)),
        format!("  user_identity  {}", blank(&answers.user_identity)),
        format!("  user_preference  {}", blank(&answers.user_preference)),
        format!("  memory_enabled    {}", answers.memory_enabled),
        format!("  knowledge_enabled {}", answers.knowledge_enabled),
        String::new(),
        "  真实宿主接下来会：验证 key → 写 environment → 写记忆 → 写完成标记。".to_string(),
        String::new(),
        "  按任意键退出。".to_string(),
    ]
}

fn wait_any_key() -> io::Result<()> {
    loop {
        if let Event::Key(_) = read()? {
            return Ok(());
        }
    }
}

fn is_quit(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL))
}

/// 方案第二节的五步：API（3 小问）/ 人格 / 关于你（3 小问）/ 记忆库 / 知识库
fn demo_steps() -> Vec<OnboardingStep> {
    vec![
        OnboardingStep::masked(
            OnboardingStepId::ApiKey,
            "先连上模型",
            "没有凭证后面全是空谈。粘贴你的 API key，回车确认。",
            "sk-...",
        ),
        OnboardingStep::new(
            OnboardingStepId::ApiBaseUrl,
            "接口地址",
            "用官方服务就直接回车；自建网关或代理才需要改这里。",
            StepKind::Text {
                masked: false,
                placeholder: "https://api.deepseek.com".to_string(),
            },
        ),
        OnboardingStep::new(
            OnboardingStepId::ApiModel,
            "模型名",
            "留空就用默认模型，之后可以在 /status 里换。",
            StepKind::Text {
                masked: false,
                placeholder: "deepseek-chat".to_string(),
            },
        ),
        OnboardingStep::choice(
            OnboardingStepId::Profile,
            "挑一个人格",
            "人格决定我说话的语气，以及我会主动在意什么。",
            vec![
                ChoiceOption::new(
                    "yunxi_companion_strong",
                    "云熙 · 强陪伴",
                    "话多一点，会主动关心你今天过得怎么样。",
                ),
                ChoiceOption::new(
                    "yunxi_focus",
                    "云熙 · 专注",
                    "少寒暄，直接干活，需要时才开口。",
                ),
                ChoiceOption::new(
                    "yunxi_quiet",
                    "云熙 · 安静",
                    "几乎不主动说话，只在你叫我时出现。",
                ),
            ],
        ),
        OnboardingStep::text(
            OnboardingStepId::UserCard,
            "关于你",
            "这三句会写进长期记忆，以后不用反复自我介绍。",
        ),
        OnboardingStep::yes_no(
            OnboardingStepId::Memory,
            "记忆库",
            "关掉也能正常对话，只是我记不住你的习惯和偏好。",
            true,
        ),
        OnboardingStep::yes_no(
            OnboardingStepId::Knowledge,
            "知识库",
            "现在不接也行，之后随时可以从 /status 里进来。",
            false,
        ),
    ]
}

fn segs_to_string(segs: &[Seg]) -> String {
    segs.iter().map(|seg| seg.text.as_str()).collect()
}

/// 演示自己的截断兜底（只用于完成页，向导界面本身由 `render` 保证宽度）
fn truncate_to(value: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for ch in value.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w > cols {
            break;
        }
        width += w;
        out.push(ch);
    }
    out
}
