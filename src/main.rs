use std::collections::VecDeque;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{self};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use gest::app::{App, RunMode};
use gest::cache::{cached_packages, load_cache, save_cache, update_package_cache};
use gest::cli::{Cli, ModeArg};
use gest::events::AppEvent;
use gest::repo::{cache_file, ensure_cache_dir, filter_packages, find_repo_root, list_packages};
use gest::runner::{start_runner, RunnerCommand, RunnerConfig};
use gest::ui;
use gest::watcher::start_watcher;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    let repo_root = find_repo_root(&cwd).ok_or("No go.mod found in this directory or parents")?;
    ensure_cache_dir(&repo_root)?;
    let cache_path = cache_file(&repo_root);
    let mut cache = load_cache(&cache_path).unwrap_or_default();
    let package_filter = cli
        .packages
        .as_ref()
        .map(|pattern| regex::Regex::new(pattern))
        .transpose()?;
    let cached = cached_packages(&repo_root, &cache);
    let all_packages = if let Some(packages) = cached {
        packages
    } else {
        let packages = list_packages(&repo_root)?;
        let _ = update_package_cache(&repo_root, &mut cache, &packages);
        packages
    };
    let packages = filter_packages(&all_packages, package_filter.as_ref());

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

    let package_cache = cache.package_cache.clone();
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
            test_command: None,
        },
        runner_event_tx,
    );

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
    let mut last_draw = Instant::now();
    let redraw_interval = Duration::from_millis(50);

    let mut should_exit = false;
    let mut dirty = false;
    let mut draw_now = false;
    let mut pending_runner_events: VecDeque<gest::runner::RunnerEvent> = VecDeque::new();
    let mut last_runner_flush = Instant::now();
    let runner_flush_interval = Duration::from_millis(50);
    let runner_batch_limit = 200usize;
    while !should_exit {
        match app_rx.recv_timeout(Duration::from_millis(30)) {
            Ok(event) => {
                let outcome = handle_app_event(event, &mut app, &runner_tx);
                should_exit = should_exit || outcome.should_exit;
                dirty = dirty || outcome.dirty;
                draw_now = draw_now || outcome.draw_now;
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        let mut processed = 0usize;
        let start = std::time::Instant::now();
        while processed < 200 && start.elapsed() < Duration::from_millis(10) {
            match app_rx.try_recv() {
                Ok(event) => {
                    processed += 1;
                    let outcome = handle_app_event(event, &mut app, &runner_tx);
                    should_exit = should_exit || outcome.should_exit;
                    dirty = dirty || outcome.dirty;
                    draw_now = draw_now || outcome.draw_now;
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

        let mut runner_processed = 0usize;
        let start = std::time::Instant::now();
        while runner_processed < 500 && start.elapsed() < Duration::from_millis(10) {
            match runner_event_rx.try_recv() {
                Ok(event) => {
                    pending_runner_events.push_back(event);
                    runner_processed += 1;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }

        if should_exit {
            break;
        }

        if !pending_runner_events.is_empty()
            && (draw_now
                || last_runner_flush.elapsed() >= runner_flush_interval
                || pending_runner_events.len() >= runner_batch_limit)
        {
            let drain_count = pending_runner_events.len().min(runner_batch_limit);
            let mut batch = Vec::with_capacity(drain_count);
            for _ in 0..drain_count {
                if let Some(event) = pending_runner_events.pop_front() {
                    batch.push(event);
                }
            }
            app.handle_runner_events(batch);
            last_runner_flush = Instant::now();
            dirty = true;
        }

        let should_draw = if draw_now {
            true
        } else {
            dirty && last_draw.elapsed() >= redraw_interval
        };

        if should_draw {
            terminal.draw(|frame| ui::draw(frame, &app))?;
            last_draw = Instant::now();
            dirty = false;
            draw_now = false;
        }
    }

    let _ = runner_tx.send(RunnerCommand::Shutdown);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    let mut final_cache = app.cache_state();
    final_cache.package_cache = package_cache;
    let _ = save_cache(&cache_path, &final_cache);
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

struct AppEventOutcome {
    should_exit: bool,
    draw_now: bool,
    dirty: bool,
}

fn handle_app_event(
    event: AppEvent,
    app: &mut App,
    runner_tx: &crossbeam_channel::Sender<RunnerCommand>,
) -> AppEventOutcome {
    match event {
        AppEvent::Input(event) => AppEventOutcome {
            should_exit: app.handle_input(event, runner_tx),
            draw_now: true,
            dirty: true,
        },
        AppEvent::Watch(event) => {
            app.handle_watch_event(event, runner_tx);
            AppEventOutcome {
                should_exit: false,
                draw_now: false,
                dirty: true,
            }
        }
        AppEvent::Tick => AppEventOutcome {
            should_exit: false,
            draw_now: false,
            dirty: false,
        },
        AppEvent::Shutdown => AppEventOutcome {
            should_exit: true,
            draw_now: false,
            dirty: false,
        },
    }
}
