//! 云熙完整欢迎界面演示
//!
//! 运行: cargo run -p yunxi-agent-tui --example welcome_demo

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
    println!("🌸 云熙欢迎界面演示");
    println!("按任意键进入全屏演示，按 q 退出...\n");

    // 等待按键
    terminal::enable_raw_mode()?;
    let _ = read()?;
    terminal::disable_raw_mode()?;

    // 进入全屏模式
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide, Clear(ClearType::All))?;
    terminal::enable_raw_mode()?;

    let result = run_demo(&mut stdout);

    // 恢复终端
    let _ = execute!(stdout, Show, LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();

    result
}

fn run_demo(stdout: &mut io::Stdout) -> io::Result<()> {
    let mut scene = WelcomeScene::new();
    let mut painted: Vec<String> = Vec::new();

    loop {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let output = scene.render(cols as usize, rows as usize);

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

fn segs_to_string(segs: &[Seg]) -> String {
    segs.iter().map(|s| &s.text).cloned().collect::<String>()
}
