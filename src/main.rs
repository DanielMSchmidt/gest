use std::time::Duration;

use clap::Parser;
use crossterm::event::{self};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use gest::app::{App, RunMode};
use gest::cache::{load_cache, save_cache};
use gest::cli::{Cli, ModeArg};
use gest::events::AppEvent;
use gest::repo::{cache_file, ensure_cache_dir, find_repo_root, list_packages};
use gest::runner::{start_runner, RunnerCommand, RunnerConfig};
use gest::watcher::start_watcher;
use gest::ui;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    let repo_root = find_repo_root(&cwd)
        .ok_or("No go.mod found in this directory or parents")?;
    ensure_cache_dir(&repo_root)?;
    let cache_path = cache_file(&repo_root);
    let cache = load_cache(&cache_path).unwrap_or_default();
    let package_filter = cli
        .packages
        .as_ref()
        .map(|pattern| regex::Regex::new(pattern))
        .transpose()?;
    let packages = list_packages(&repo_root, package_filter.as_ref())?;

    let mut pkg_concurrency = cli.pkg_concurrency.max(1);
    let mut go_test_p = pkg_concurrency;
    if cli.sequential {
        pkg_concurrency = 1;
        go_test_p = 1;
    }

    let mode = match cli.mode {
        ModeArg::All => RunMode::All,
        ModeArg::Failing => RunMode::Failing,
        ModeArg::Select => RunMode::Selecting,
    };

    let mut app = App::new(
        repo_root.clone(),
        packages,
        cache,
        mode,
        package_filter.is_some(),
        !cli.no_watch,
    );

    let (app_tx, app_rx) = crossbeam_channel::unbounded();
    let shutdown_tx = app_tx.clone();
    ctrlc::set_handler(move || {
        let _ = shutdown_tx.send(AppEvent::Shutdown);
    })?;

    let (runner_event_tx, runner_event_rx) = crossbeam_channel::unbounded();
    let runner_tx = start_runner(
        RunnerConfig {
            root: repo_root.clone(),
            pkg_concurrency,
            go_test_p,
            no_test_cache: cli.no_test_cache,
        },
        runner_event_tx,
    );

    let app_tx_clone = app_tx.clone();
    std::thread::spawn(move || {
        while let Ok(event) = runner_event_rx.recv() {
            let _ = app_tx_clone.send(AppEvent::Runner(event));
        }
    });

    if app.watch_enabled {
        let (watch_event_tx, watch_event_rx) = crossbeam_channel::unbounded();
        if let Err(err) = start_watcher(repo_root.clone(), watch_event_tx) {
            app.last_error = Some(err.to_string());
        } else {
            let app_tx_clone = app_tx.clone();
            std::thread::spawn(move || {
                while let Ok(event) = watch_event_rx.recv() {
                    let _ = app_tx_clone.send(AppEvent::Watch(event));
                }
            });
        }
    }

    start_input_thread(app_tx.clone());
    start_tick_thread(app_tx.clone());

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    app.run_all(&runner_tx);

    terminal.draw(|frame| ui::draw(frame, &app))?;

    let mut should_exit = false;
    while !should_exit {
        let mut had_event = false;
        match app_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => {
                had_event = true;
                should_exit = handle_app_event(event, &mut app, &runner_tx);
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        let mut processed = 0usize;
        let start = std::time::Instant::now();
        while processed < 500 && start.elapsed() < Duration::from_millis(10) {
            match app_rx.try_recv() {
                Ok(event) => {
                    had_event = true;
                    processed += 1;
                    should_exit = handle_app_event(event, &mut app, &runner_tx) || should_exit;
                    if should_exit {
                        break;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    should_exit = true;
                    break;
                }
            }
        }

        if had_event {
            terminal.draw(|frame| ui::draw(frame, &app))?;
        }
    }

    let _ = runner_tx.send(RunnerCommand::Shutdown);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    let _ = save_cache(&cache_path, &app.cache_state());
    Ok(())
}

fn start_input_thread(tx: crossbeam_channel::Sender<AppEvent>) {
    std::thread::spawn(move || loop {
        if event::poll(Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(event) = event::read() {
                let _ = tx.send(AppEvent::Input(event));
            }
        }
    });
}

fn start_tick_thread(tx: crossbeam_channel::Sender<AppEvent>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(200));
        let _ = tx.send(AppEvent::Tick);
    });
}

fn handle_app_event(
    event: AppEvent,
    app: &mut App,
    runner_tx: &crossbeam_channel::Sender<RunnerCommand>,
) -> bool {
    match event {
        AppEvent::Input(event) => app.handle_input(event, runner_tx),
        AppEvent::Runner(event) => {
            app.handle_runner_event(event);
            false
        }
        AppEvent::Watch(event) => {
            app.handle_watch_event(event, runner_tx);
            false
        }
        AppEvent::Tick => false,
        AppEvent::Shutdown => true,
    }
}
