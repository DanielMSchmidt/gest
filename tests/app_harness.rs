use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use gest::app::{App, RunMode};
use gest::cache::CacheState;
use gest::model::{TestId, TestStatus};
use gest::repo::PackageInfo;
use gest::runner::{RunKind, RunnerCommand, RunnerEvent};

fn build_app() -> App {
    let packages = vec![PackageInfo {
        import_path: "example".to_string(),
        dir: std::path::PathBuf::from("."),
    }];
    App::new(
        std::path::PathBuf::from("."),
        packages,
        CacheState::default(),
        RunMode::All,
        false,
        false,
    )
}

#[test]
fn rerun_marks_running_and_emits_command() {
    let mut app = build_app();
    let test = TestId {
        package: "example".to_string(),
        name: "TestFoo".to_string(),
    };
    app.registry.ensure_test(&test);
    app.list_state.select(Some(0));

    let (runner_tx, runner_rx) = crossbeam_channel::unbounded();
    let event = Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    app.handle_input(event, &runner_tx);

    let command = runner_rx.recv().expect("runner command sent");
    match command {
        RunnerCommand::Run(spec) => {
            assert_eq!(spec.kind, RunKind::Single);
            assert_eq!(spec.packages.len(), 1);
            assert_eq!(
                spec.packages[0].packages,
                vec!["example".to_string()]
            );
            assert_eq!(
                spec.packages[0].tests,
                Some(vec!["TestFoo".to_string()])
            );
        }
        other => panic!("unexpected command: {:?}", other),
    }

    let case = app.registry.case(&test).expect("test case exists");
    assert_eq!(case.status, TestStatus::Running);
}

#[test]
fn ignores_stale_runner_events() {
    let mut app = build_app();
    app.handle_runner_event(RunnerEvent::RunStarted {
        run_id: 1,
        kind: RunKind::All,
        packages: 1,
    });
    app.handle_runner_event(RunnerEvent::RunStarted {
        run_id: 2,
        kind: RunKind::All,
        packages: 1,
    });

    app.handle_runner_event(RunnerEvent::RunFinished {
        run_id: 1,
        kind: RunKind::All,
    });

    assert_eq!(app.run_state.run_id, Some(2));
    assert!(app.run_state.running);

    app.handle_runner_event(RunnerEvent::PackageFinished {
        run_id: 1,
        package: "example".to_string(),
        success: true,
    });
    assert_eq!(app.run_state.packages_done, 0);
}
