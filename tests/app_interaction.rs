use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use crossbeam_channel::unbounded;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use gest::app::{App, RunMode};
use gest::cache::CacheState;
use gest::model::{TestId, TestStatus};
use gest::repo::PackageInfo;
use gest::runner::{RunKind, RunnerCommand, RunnerEvent};
use gest::ui::draw;

fn build_app(test: &TestId) -> App {
    let cache = CacheState {
        failing: vec![test.clone()],
        selected: Vec::new(),
        package_cache: None,
    };
    App::new(
        std::path::PathBuf::from("."),
        vec![PackageInfo {
            import_path: test.package.clone(),
            dir: std::path::PathBuf::from("."),
        }],
        cache,
        RunMode::All,
        false,
        false,
    )
}

fn key_event(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn render_app(terminal: &mut Terminal<TestBackend>, app: &App) {
    terminal.draw(|frame| draw(frame, app)).expect("render should succeed");
}

#[test]
fn rerun_marks_running_closes_detail_and_emits_command() {
    let test = TestId {
        package: "example".to_string(),
        name: "TestFoo".to_string(),
    };
    let mut app = build_app(&test);
    app.detail_open = true;

    let (runner_tx, runner_rx) = unbounded();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal init");

    app.handle_runner_event(RunnerEvent::RunStarted {
        run_id: 1,
        kind: RunKind::All,
        packages: 1,
    });
    render_app(&mut terminal, &app);

    app.handle_runner_event(RunnerEvent::RunFinished {
        run_id: 1,
        kind: RunKind::All,
    });
    render_app(&mut terminal, &app);

    app.handle_input(key_event(KeyCode::Char('r'), KeyModifiers::empty()), &runner_tx);
    render_app(&mut terminal, &app);

    assert!(!app.detail_open, "detail pane should close on rerun");
    assert_eq!(
        app.registry
            .case(&test)
            .expect("test should exist")
            .status,
        TestStatus::Running
    );

    let command = runner_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("expected rerun command");
    match command {
        RunnerCommand::Run(spec) => {
            assert_eq!(spec.kind, RunKind::Single);
        }
        _ => panic!("expected run command"),
    }
}

#[test]
fn rerun_while_running_updates_run_id_and_ignores_stale_finish() {
    let test = TestId {
        package: "example".to_string(),
        name: "TestFoo".to_string(),
    };
    let mut app = build_app(&test);

    let (runner_tx, runner_rx) = unbounded();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal init");

    app.handle_runner_event(RunnerEvent::RunStarted {
        run_id: 1,
        kind: RunKind::All,
        packages: 1,
    });
    render_app(&mut terminal, &app);

    app.handle_input(key_event(KeyCode::Char('r'), KeyModifiers::empty()), &runner_tx);
    render_app(&mut terminal, &app);
    let _ = runner_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("expected rerun command");

    app.handle_runner_event(RunnerEvent::RunStarted {
        run_id: 2,
        kind: RunKind::Single,
        packages: 1,
    });
    render_app(&mut terminal, &app);
    assert_eq!(app.run_state.run_id, Some(2));

    app.handle_runner_event(RunnerEvent::RunFinished {
        run_id: 1,
        kind: RunKind::All,
    });
    render_app(&mut terminal, &app);
    assert!(app.run_state.running, "stale finish should not stop current run");
    assert_eq!(app.run_state.run_id, Some(2));

    app.handle_runner_event(RunnerEvent::RunFinished {
        run_id: 2,
        kind: RunKind::Single,
    });
    render_app(&mut terminal, &app);
    assert!(!app.run_state.running, "current run should finish cleanly");
}
