//! 云熙完整欢迎界面演示
//!
//! 运行: cargo run -p yunxi-agent-tui --example welcome_demo
//!       cargo run -p yunxi-agent-tui --example welcome_demo -- --first-run
//!
//! `--first-run` 演示首启路径：同一个动画界面在下方追加“首次启动检查”清单，
//! 运行中按 c 可以来回切换，确认带清单时动画依然完整可见。

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{Event, KeyCode, poll, read},
    execute,
    style::Print,
    terminal::{
        self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use std::io::{self, Write};
use std::time::Duration;
use yunxi_agent_tui::welcome::WelcomeScene;
use yunxi_agent_tui::yunxi_starfield::Seg;

fn main() -> io::Result<()> {
    let first_run = std::env::args().any(|arg| arg == "--first-run");

    println!("🌸 云熙欢迎界面演示");
    if first_run {
        println!("已启用首启检查清单（运行中按 c 切换）");
    } else {
        println!("提示：加 --first-run 可预览带首启检查清单的界面");
    }
    println!("按任意键进入全屏演示，按 q 退出...\n");

    // 等待按键
    terminal::enable_raw_mode()?;
    let _ = read()?;
    terminal::disable_raw_mode()?;

    // 进入全屏模式
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide, Clear(ClearType::All))?;
    terminal::enable_raw_mode()?;

    let result = run_demo(&mut stdout, first_run);

    // 恢复终端
    let _ = execute!(stdout, Show, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();

    result
}

fn run_demo(stdout: &mut io::Stdout, first_run: bool) -> io::Result<()> {
    let mut scene = WelcomeScene::new();
    let mut painted: Vec<String> = Vec::new();
    let mut show_checklist = first_run;

    loop {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let checklist = if show_checklist {
            demo_checklist()
        } else {
            Vec::new()
        };
        let output = scene.render_with_checklist(cols as usize, rows as usize, &checklist);

        // 确保 painted 数组大小匹配
        if painted.len() != output.len() {
            painted = vec!["\u{0}".into(); output.len()];
        }

        // 同步更新
        execute!(stdout, BeginSynchronizedUpdate)?;

        for (y, line) in output.iter().enumerate() {
            let line_str = segs_to_string(line);
            if painted[y] != line_str {
                execute!(
                    stdout,
                    MoveTo(0, y as u16),
                    Clear(ClearType::UntilNewLine),
                    Print(&line_str)
                )?;
                painted[y] = line_str;
            }
        }

        execute!(stdout, MoveTo(0, 0), EndSynchronizedUpdate)?;
        stdout.flush()?;

        // 40ms 一帧
        if poll(Duration::from_millis(40))? {
            match read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') => {
                        show_checklist = !show_checklist;
                        painted.clear();
                        execute!(stdout, Clear(ClearType::All))?;
                    }
                    _ => {}
                },
                Event::Resize(_, _) => {
                    painted.clear();
                    execute!(stdout, Clear(ClearType::All))?;
                }
                _ => {}
            }
        }

        scene.tick();
    }
}

/// 与 `yunxi-agent-linux/src/main.rs::claim_first_run_checklist` 同构的演示数据
fn demo_checklist() -> Vec<String> {
    vec![
        "  工作区                 ✓ /tmp/yunxi".to_string(),
        "  YunXi 状态目录         ✓ /home/yunxi/.local/state/yunxi".to_string(),
        "  Provider / 模型        ✓ deepseek / static".to_string(),
        "  会话与记忆目录         ✓ 已初始化".to_string(),
        "  默认知识库             - 未配置，稍后可接入".to_string(),
        "  完成。直接输入目标即可开始，/help 查看帮助".to_string(),
    ]
}

fn segs_to_string(segs: &[Seg]) -> String {
    segs.iter().map(|s| &s.text).cloned().collect::<String>()
}
