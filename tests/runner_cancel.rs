use std::time::{Duration, Instant};

use crossbeam_channel::RecvTimeoutError;

use gest::app::{App, RunMode};
use gest::cache::CacheState;
use gest::repo::PackageInfo;
use gest::runner::{start_runner, PackageRun, RunKind, RunSpec, RunnerCommand, RunnerConfig, RunnerEvent};

fn sample_app() -> App {
    App::new(
        std::path::PathBuf::from("."),
        vec![PackageInfo {
            import_path: "example".to_string(),
            dir: std::path::PathBuf::from("."),
        }],
        CacheState::default(),
        RunMode::All,
        false,
        false,
    )
}

fn long_running_command() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "cmd".to_string(),
            "/C".to_string(),
            "ping".to_string(),
            "127.0.0.1".to_string(),
            "-n".to_string(),
            "3".to_string(),
        ]
    }
    #[cfg(not(windows))]
    {
        vec!["sleep".to_string(), "2".to_string()]
    }
}

#[test]
fn cancel_unblocks_runner_and_updates_app_state() {
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let runner_tx = start_runner(
        RunnerConfig {
            root: std::path::PathBuf::from("."),
            pkg_concurrency: 1,
            go_test_p: 1,
            no_test_cache: false,
            test_command: Some(long_running_command()),
        },
        event_tx,
    );

    let mut app = sample_app();
    let spec = RunSpec {
        kind: RunKind::All,
        packages: vec![PackageRun {
            packages: vec!["./...".to_string()],
            tests: None,
        }],
        no_test_cache_override: None,
        timeout: None,
    };
    let _ = runner_tx.send(RunnerCommand::Run(spec));

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut saw_run_error = false;
    let mut saw_run_finished = false;
    while Instant::now() < deadline {
        match event_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => match event {
                RunnerEvent::RunStarted {
                    run_id,
                    kind,
                    packages,
                } => {
                    app.handle_runner_event(RunnerEvent::RunStarted {
                        run_id,
                        kind,
                        packages,
                    });
                    assert!(app.run_state.running);
                    let _ = runner_tx.send(RunnerCommand::Cancel {
                        run_id: Some(run_id),
                    });
                }
                RunnerEvent::RunError { run_id, message } => {
                    saw_run_error = true;
                    app.handle_runner_event(RunnerEvent::RunError { run_id, message });
                }
                RunnerEvent::RunFinished { run_id, kind } => {
                    saw_run_finished = true;
                    app.handle_runner_event(RunnerEvent::RunFinished { run_id, kind });
                }
                other => {
                    app.handle_runner_event(other);
                }
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if saw_run_finished {
            break;
        }
    }

    let _ = runner_tx.send(RunnerCommand::Shutdown);

    assert!(saw_run_error, "expected RunError on cancellation");
    assert!(saw_run_finished, "expected RunFinished after cancellation");
    assert!(!app.run_state.running, "expected app to stop running");
}
