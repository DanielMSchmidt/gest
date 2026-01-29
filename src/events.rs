use std::path::PathBuf;

use crossterm::event::Event;

#[derive(Debug)]
pub enum AppEvent {
    Input(Event),
    Watch(WatchEvent),
    Tick,
    Shutdown,
}

#[derive(Debug)]
pub enum WatchEvent {
    FilesChanged(Vec<PathBuf>),
    Error(String),
}
