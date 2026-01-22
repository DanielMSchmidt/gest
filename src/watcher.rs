use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::{after, Receiver, Sender};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::events::WatchEvent;

pub fn start_watcher(root: PathBuf, event_tx: Sender<WatchEvent>) -> notify::Result<()> {
    let (raw_tx, raw_rx) = crossbeam_channel::unbounded();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = raw_tx.send(res);
        },
        notify::Config::default(),
    )?;
    watcher.watch(&root, RecursiveMode::Recursive)?;

    std::thread::spawn(move || {
        let _watcher = watcher;
        watch_loop(raw_rx, event_tx);
    });
    Ok(())
}

fn watch_loop(raw_rx: Receiver<notify::Result<Event>>, event_tx: Sender<WatchEvent>) {
    let mut pending = HashSet::new();
    let debounce = Duration::from_millis(250);
    loop {
        crossbeam_channel::select! {
            recv(raw_rx) -> msg => {
                match msg {
                    Ok(Ok(event)) => {
                        for path in event.paths {
                            pending.insert(path);
                        }
                    }
                    Ok(Err(err)) => {
                        let _ = event_tx.send(WatchEvent::Error(err.to_string()));
                    }
                    Err(_) => break,
                }
            }
            recv(after(debounce)) -> _ => {
                if !pending.is_empty() {
                    let paths: Vec<PathBuf> = pending.drain().collect();
                    let _ = event_tx.send(WatchEvent::FilesChanged(paths));
                }
            }
        }
    }
}
