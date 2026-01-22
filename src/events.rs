use std::path::PathBuf;

use crossterm::event::Event;

use crate::runner::RunnerEvent;

#[derive(Debug)]
pub enum AppEvent {
    Input(Event),
    Runner(RunnerEvent),
    Watch(WatchEvent),
    Tick,
    Shutdown,
}

#[derive(Debug)]
pub enum WatchEvent {
    FilesChanged(Vec<PathBuf>),
    Error(String),
}
