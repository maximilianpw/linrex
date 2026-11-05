mod app;
mod picker;
mod theme;
mod view;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};

use crate::config::Config;

pub fn run(config: Config) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = app::App::new(config);
    let mut frame_count = 0usize;
    let result = (|| -> Result<()> {
        while !app.should_quit {
            app.tick();
            terminal.draw(|frame| view::draw(frame, &mut app, frame_count))?;
            frame_count = frame_count.wrapping_add(1);
            if event::poll(Duration::from_millis(120))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                app.on_key(key);
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}
