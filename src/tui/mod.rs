mod app;
mod picker;
mod theme;
mod view;

use std::io::stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::execute;

use crate::config::Config;

pub fn run(config: Config) -> Result<()> {
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableMouseCapture)?;
    let restore_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture);
        restore_hook(info);
    }));
    let mut app = app::App::new(config);
    let mut frame_count = 0usize;
    let result = (|| -> Result<()> {
        while !app.should_quit {
            app.tick();
            terminal.draw(|frame| view::draw(frame, &mut app, frame_count))?;
            frame_count = frame_count.wrapping_add(1);
            if event::poll(Duration::from_millis(50))? {
                loop {
                    match event::read()? {
                        Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                        Event::Mouse(mouse) => app.on_mouse(mouse),
                        _ => {}
                    }
                    if app.should_quit || !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
        }
        Ok(())
    })();
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
