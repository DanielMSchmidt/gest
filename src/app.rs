use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::widgets::ListState;

use crate::cache::CacheState;
use crate::events::WatchEvent;
use crate::model::{TestId, TestRegistry, TestStatus};
use crate::repo::{package_for_path, PackageInfo};
use crate::runner::{PackageRun, RunKind, RunSpec, RunnerCommand, RunnerEvent};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RunMode {
    All,
    Failing,
    Selected,
    Selecting,
}

#[derive(Debug, Default, Clone)]
pub struct RunState {
    pub run_id: Option<u64>,
    pub kind: Option<RunKind>,
    pub packages_total: usize,
    pub packages_done: usize,
    pub running: bool,
    pub run_started_at: Option<Instant>,
}

#[derive(Debug, Default, Clone)]
pub struct SelectionState {
    pub query: String,
    pub filtered: Vec<TestId>,
}

pub struct App {
    pub mode: RunMode,
    pub registry: TestRegistry,
    pub failing_set: HashSet<TestId>,
    pub selected_set: HashSet<TestId>,
    pub list_state: ListState,
    pub detail_open: bool,
    pub selection: SelectionState,
    pub run_state: RunState,
    pub packages: Vec<PackageInfo>,
    pub package_filter_active: bool,
    pub repo_root: std::path::PathBuf,
    pub watch_enabled: bool,
    pub last_error: Option<String>,
}

impl App {
    pub fn new(
        repo_root: std::path::PathBuf,
        packages: Vec<PackageInfo>,
        cache: CacheState,
        mode: RunMode,
        package_filter_active: bool,
        watch_enabled: bool,
    ) -> Self {
        let mut registry = TestRegistry::default();
        let failing_set: HashSet<TestId> = cache.failing.into_iter().collect();
        let selected_set: HashSet<TestId> = cache.selected.into_iter().collect();

        for test in failing_set.iter().chain(selected_set.iter()) {
            registry.ensure_test(test);
        }

        let mut app = Self {
            mode,
            registry,
            failing_set,
            selected_set,
            list_state: ListState::default(),
            detail_open: false,
            selection: SelectionState::default(),
            run_state: RunState::default(),
            packages,
            package_filter_active,
            repo_root,
            watch_enabled,
            last_error: None,
        };

        app.refresh_lists();
        app
    }

    pub fn cache_state(&self) -> CacheState {
        CacheState {
            failing: self.failing_set.iter().cloned().collect(),
            selected: self.selected_set.iter().cloned().collect(),
            package_cache: None,
        }
    }

    pub fn visible_tests(&self) -> Vec<TestId> {
        match self.mode {
            RunMode::All => self.sorted_all_tests(),
            RunMode::Failing => self.sorted_from_set(&self.failing_set),
            RunMode::Selected => self.sorted_from_set(&self.selected_set),
            RunMode::Selecting => self.selection.filtered.clone(),
        }
    }

    pub fn current_test(&self) -> Option<TestId> {
        let list = self.visible_tests();
        let index = self.list_state.selected()?;
        list.get(index).cloned()
    }

    pub fn test_progress(&self) -> (usize, usize) {
        let start = match self.run_state.run_started_at {
            Some(start) => start,
            None => return (0, 0),
        };
        let tests = self.registry.leaf_tests();
        let mut total = 0;
        let done = tests
            .iter()
            .filter_map(|id| self.registry.case(id))
            .filter(|case| case.last_update.map(|ts| ts >= start).unwrap_or(false))
            .filter(|case| {
                total += 1;
                matches!(case.status, TestStatus::Passed | TestStatus::Failed)
            })
            .count();
        (done, total)
    }

    pub fn handle_runner_event(&mut self, event: RunnerEvent) {
        self.handle_runner_events(std::iter::once(event));
    }

    pub fn handle_runner_events<I>(&mut self, events: I)
    where
        I: IntoIterator<Item = RunnerEvent>,
    {
        let mut refresh_selection = false;
        let mut refresh_failing = false;

        for event in events {
            match event {
                RunnerEvent::RunStarted {
                    run_id,
                    kind,
                    packages,
                } => {
                    self.run_state = RunState {
                        run_id: Some(run_id),
                        kind: Some(kind),
                        packages_total: packages,
                        packages_done: 0,
                        running: true,
                        run_started_at: Some(Instant::now()),
                    };
                }
                RunnerEvent::PackageFinished { run_id, .. } => {
                    if !self.is_current_run(run_id) {
                        continue;
                    }
                    self.run_state.packages_done = self.run_state.packages_done.saturating_add(1);
                }
                RunnerEvent::RunFinished { run_id, kind } => {
                    if !self.is_current_run(run_id) {
                        continue;
                    }
                    self.run_state.running = false;
                    if kind == RunKind::All {
                        refresh_failing = true;
                    }
                }
                RunnerEvent::TestEvent { run_id, event } => {
                    if !self.is_current_run(run_id) {
                        continue;
                    }
                    self.registry.apply_event(&event);
                    if self.mode == RunMode::Selecting {
                        refresh_selection = true;
                    }
                }
                RunnerEvent::RunError { run_id, message } => {
                    if !self.is_current_run(run_id) {
                        continue;
                    }
                    self.last_error = Some(message);
                }
                RunnerEvent::PackageStarted { run_id, .. } => {
                    if !self.is_current_run(run_id) {
                        continue;
                    }
                }
            }
        }

        if refresh_failing {
            self.update_failing_set();
        }
        if refresh_selection {
            self.refresh_selection_filter();
        }
        self.refresh_lists();
    }

    pub fn handle_watch_event(
        &mut self,
        event: WatchEvent,
        runner_tx: &crossbeam_channel::Sender<RunnerCommand>,
    ) {
        match event {
            WatchEvent::FilesChanged(paths) => {
                if !self.watch_enabled {
                    return;
                }
                if self.mode == RunMode::All {
                    let mut packages = HashSet::new();
                    for path in paths {
                        if !is_go_file(&path) {
                            continue;
                        }
                        if let Some(package) = package_for_path(&self.packages, &path) {
                            packages.insert(package.import_path.clone());
                        }
                    }
                    if !packages.is_empty() {
                        self.cancel_current_run(runner_tx);
                        let spec = RunSpec {
                            kind: RunKind::All,
                            packages: vec![PackageRun {
                                packages: packages.into_iter().collect(),
                                tests: None,
                            }],
                            no_test_cache_override: None,
                            timeout: None,
                        };
                        let _ = runner_tx.send(RunnerCommand::Run(spec));
                    }
                } else if self.mode == RunMode::Failing {
                    self.run_failing(runner_tx);
                } else if self.mode == RunMode::Selected || self.mode == RunMode::Selecting {
                    self.run_selected(runner_tx);
                }
            }
            WatchEvent::Error(message) => {
                self.last_error = Some(message);
            }
        }
    }

    pub fn handle_input(
        &mut self,
        event: Event,
        runner_tx: &crossbeam_channel::Sender<RunnerCommand>,
    ) -> bool {
        match event {
            Event::Key(key) => self.handle_key(key, runner_tx),
            _ => false,
        }
    }

    pub fn run_all(&mut self, runner_tx: &crossbeam_channel::Sender<RunnerCommand>) {
        self.cancel_current_run(runner_tx);
        let packages: Vec<String> = if self.package_filter_active {
            self.packages
                .iter()
                .map(|package| package.import_path.clone())
                .collect()
        } else {
            vec!["./...".to_string()]
        };
        if packages.is_empty() {
            return;
        }
        let spec = RunSpec {
            kind: RunKind::All,
            packages: vec![PackageRun {
                packages,
                tests: None,
            }],
            no_test_cache_override: None,
            timeout: None,
        };
        let _ = runner_tx.send(RunnerCommand::Run(spec));
    }

    pub fn run_failing(&self, runner_tx: &crossbeam_channel::Sender<RunnerCommand>) {
        self.cancel_current_run(runner_tx);
        let spec = self.spec_for_tests(RunKind::Failing, &self.failing_set, None);
        if let Some(spec) = spec {
            let _ = runner_tx.send(RunnerCommand::Run(spec));
        }
    }

    pub fn run_selected(&self, runner_tx: &crossbeam_channel::Sender<RunnerCommand>) {
        self.cancel_current_run(runner_tx);
        let spec = self.spec_for_tests(RunKind::Selected, &self.selected_set, None);
        if let Some(spec) = spec {
            let _ = runner_tx.send(RunnerCommand::Run(spec));
        }
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        runner_tx: &crossbeam_channel::Sender<RunnerCommand>,
    ) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        match self.mode {
            RunMode::Selecting => self.handle_select_key(key, runner_tx),
            _ => self.handle_list_key(key, runner_tx),
        }
    }

    fn handle_list_key(
        &mut self,
        key: KeyEvent,
        runner_tx: &crossbeam_channel::Sender<RunnerCommand>,
    ) -> bool {
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('a') => {
                self.mode = RunMode::All;
                self.run_all(runner_tx);
            }
            KeyCode::Char('o') => {
                self.mode = RunMode::Failing;
                self.run_failing(runner_tx);
            }
            KeyCode::Char('p') => {
                self.mode = RunMode::Selecting;
                self.selection.query.clear();
                self.refresh_selection_filter();
            }
            KeyCode::Up => self.select_previous(),
            KeyCode::Down => self.select_next(),
            KeyCode::Enter => self.detail_open = !self.detail_open,
            KeyCode::Right => self.detail_open = true,
            KeyCode::Left => self.detail_open = false,
            KeyCode::Char('r') | KeyCode::Char('R') => {
                let no_test_cache = key.modifiers.contains(KeyModifiers::SHIFT)
                    || matches!(key.code, KeyCode::Char('R'));
                if let Some(test) = self.current_test() {
                    self.detail_open = false;
                    self.mark_running(&test);
                    let mut tests = HashSet::new();
                    tests.insert(test);
                    let override_flag = if no_test_cache { Some(true) } else { None };
                    let spec = self.spec_for_tests(RunKind::Single, &tests, override_flag);
                    if let Some(spec) = spec {
                        self.cancel_current_run(runner_tx);
                        let _ = runner_tx.send(RunnerCommand::Run(spec));
                    }
                }
            }
            KeyCode::Char('x') => {
                if let Some(test) = self.current_test() {
                    match self.mode {
                        RunMode::Failing => {
                            self.failing_set.remove(&test);
                        }
                        RunMode::Selected => {
                            self.selected_set.remove(&test);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        self.refresh_lists();
        false
    }

    fn handle_select_key(
        &mut self,
        key: KeyEvent,
        runner_tx: &crossbeam_channel::Sender<RunnerCommand>,
    ) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('p') => {
                self.mode = RunMode::Selected;
                self.refresh_lists();
                self.run_selected(runner_tx);
            }
            KeyCode::Char('a') => {
                self.mode = RunMode::All;
                self.run_all(runner_tx);
            }
            KeyCode::Char('o') => {
                self.mode = RunMode::Failing;
                self.run_failing(runner_tx);
            }
            KeyCode::Up => self.select_previous(),
            KeyCode::Down => self.select_next(),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(test) = self.current_test() {
                    if self.selected_set.contains(&test) {
                        self.selected_set.remove(&test);
                    } else {
                        self.selected_set.insert(test);
                    }
                }
            }
            KeyCode::Backspace => {
                self.selection.query.pop();
                self.refresh_selection_filter();
            }
            KeyCode::Char(ch) => {
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    self.selection.query.push(ch);
                    self.refresh_selection_filter();
                }
            }
            _ => {}
        }
        self.refresh_lists();
        false
    }

    fn select_previous(&mut self) {
        let list = self.visible_tests();
        if list.is_empty() {
            self.list_state.select(None);
            return;
        }
        let index = self.list_state.selected().unwrap_or(0);
        let new_index = if index == 0 {
            list.len() - 1
        } else {
            index - 1
        };
        self.list_state.select(Some(new_index));
    }

    fn select_next(&mut self) {
        let list = self.visible_tests();
        if list.is_empty() {
            self.list_state.select(None);
            return;
        }
        let index = self.list_state.selected().unwrap_or(0);
        let new_index = if index + 1 >= list.len() {
            0
        } else {
            index + 1
        };
        self.list_state.select(Some(new_index));
    }

    fn refresh_lists(&mut self) {
        let list = self.visible_tests();
        self.ensure_selection_index(&list);
    }

    fn ensure_selection_index(&mut self, list: &[TestId]) {
        let current = self.list_state.selected().unwrap_or(0);
        if list.is_empty() {
            self.list_state.select(None);
            return;
        }
        if current >= list.len() || self.list_state.selected().is_none() {
            self.list_state.select(Some(0));
        }
    }

    fn update_failing_set(&mut self) {
        let failing = self.registry.failed_tests();
        self.failing_set = failing.into_iter().collect();
    }

    fn sorted_all_tests(&self) -> Vec<TestId> {
        let mut tests = self.registry.leaf_tests();
        tests.sort_by(|a, b| {
            let status_a = self.status_rank(a);
            let status_b = self.status_rank(b);
            status_a.cmp(&status_b).then_with(|| {
                self.registry
                    .order_index(a)
                    .cmp(&self.registry.order_index(b))
            })
        });
        tests
    }

    fn sorted_from_set(&self, set: &HashSet<TestId>) -> Vec<TestId> {
        let mut tests: Vec<TestId> = set
            .iter()
            .filter(|id| !self.registry.is_parent(id))
            .cloned()
            .collect();
        tests.sort_by_key(|id| self.registry.order_index(id));
        tests
    }

    fn status_rank(&self, id: &TestId) -> usize {
        let status = self
            .registry
            .case(id)
            .map(|case| case.status)
            .unwrap_or(TestStatus::Unknown);
        match status {
            TestStatus::Failed => 0,
            TestStatus::Running => 1,
            TestStatus::Passed => 2,
            TestStatus::Unknown => 3,
        }
    }

    fn spec_for_tests(
        &self,
        kind: RunKind,
        tests: &HashSet<TestId>,
        no_test_cache_override: Option<bool>,
    ) -> Option<RunSpec> {
        if tests.is_empty() {
            return None;
        }
        let mut packages: HashMap<String, Vec<String>> = HashMap::new();
        for test in tests {
            packages
                .entry(test.package.clone())
                .or_default()
                .push(test.name.clone());
        }
        Some(RunSpec {
            kind,
            packages: packages
                .into_iter()
                .map(|(package, tests)| PackageRun {
                    packages: vec![package],
                    tests: Some(tests),
                })
                .collect(),
            no_test_cache_override,
            timeout: None,
        })
    }

    fn refresh_selection_filter(&mut self) {
        let query = self.selection.query.clone();
        let all_tests = self.registry.leaf_tests();
        let filtered = if query.is_empty() {
            all_tests
        } else {
            let matcher = SkimMatcherV2::default();
            let mut scored: Vec<(i64, TestId)> = Vec::new();
            for test in all_tests {
                let haystack = format!("{} {}", test.name, test.package);
                if let Some(score) = matcher.fuzzy_match(&haystack, &query) {
                    scored.push((score, test));
                }
            }
            scored.sort_by(|a, b| {
                b.0.cmp(&a.0).then_with(|| {
                    self.registry
                        .order_index(&a.1)
                        .cmp(&self.registry.order_index(&b.1))
                })
            });
            scored.into_iter().map(|(_, test)| test).collect()
        };

        self.selection.filtered = filtered;
        self.list_state.select(Some(0));
    }

    fn mark_running(&mut self, id: &TestId) {
        self.registry.ensure_test(id);
        if let Some(case) = self.registry.case_mut(id) {
            case.status = TestStatus::Running;
            case.output.clear();
            case.panic = false;
            case.last_update = Some(Instant::now());
        }
    }

    fn cancel_current_run(&self, runner_tx: &crossbeam_channel::Sender<RunnerCommand>) {
        if self.run_state.running {
            let _ = runner_tx.send(RunnerCommand::Cancel {
                run_id: self.run_state.run_id,
            });
        }
    }

    fn is_current_run(&self, run_id: u64) -> bool {
        self.run_state
            .run_id
            .map(|current| current == run_id)
            .unwrap_or(true)
    }
}

fn is_go_file(path: &std::path::Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("go") => true,
        Some("mod") if path.file_name().and_then(|name| name.to_str()) == Some("go.mod") => true,
        Some("sum") if path.file_name().and_then(|name| name.to_str()) == Some("go.sum") => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheState;
    use crate::go::{GoTestAction, GoTestEvent};
    use crate::repo::PackageInfo;

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

    #[test]
    fn updates_failing_set_from_registry() {
        let mut app = sample_app();
        let event = GoTestEvent {
            action: GoTestAction::Fail,
            package: "example".to_string(),
            test: Some("TestFoo".to_string()),
            output: None,
            elapsed: None,
        };
        app.registry.apply_event(&event);
        app.update_failing_set();
        assert!(app.failing_set.contains(&TestId {
            package: "example".to_string(),
            name: "TestFoo".to_string()
        }));
    }

    #[test]
    fn fuzzy_filter_matches_query() {
        let mut app = sample_app();
        let event = GoTestEvent {
            action: GoTestAction::Run,
            package: "example".to_string(),
            test: Some("TestAlpha".to_string()),
            output: None,
            elapsed: None,
        };
        app.registry.apply_event(&event);
        app.selection.query = "Tal".to_string();
        app.refresh_selection_filter();
        assert_eq!(app.selection.filtered.len(), 1);
        assert_eq!(app.selection.filtered[0].name, "TestAlpha");
    }

    #[test]
    fn sorts_failures_first_in_all_mode() {
        let mut app = sample_app();
        let fail = GoTestEvent {
            action: GoTestAction::Fail,
            package: "example".to_string(),
            test: Some("TestFail".to_string()),
            output: None,
            elapsed: None,
        };
        let pass = GoTestEvent {
            action: GoTestAction::Pass,
            package: "example".to_string(),
            test: Some("TestPass".to_string()),
            output: None,
            elapsed: None,
        };
        app.registry.apply_event(&pass);
        app.registry.apply_event(&fail);
        let tests = app.sorted_all_tests();
        assert_eq!(tests.first().unwrap().name, "TestFail");
    }

    #[test]
    fn groups_spec_by_package() {
        let app = sample_app();
        let mut tests = HashSet::new();
        tests.insert(TestId {
            package: "example".to_string(),
            name: "TestFoo".to_string(),
        });
        tests.insert(TestId {
            package: "example".to_string(),
            name: "TestBar".to_string(),
        });
        let spec = app.spec_for_tests(RunKind::Selected, &tests, None).unwrap();
        assert_eq!(spec.packages.len(), 1);
        assert_eq!(spec.packages[0].tests.as_ref().unwrap().len(), 2);
        assert_eq!(spec.packages[0].packages.len(), 1);
    }

    #[test]
    fn mark_running_clears_output() {
        let mut app = sample_app();
        let id = TestId {
            package: "example".to_string(),
            name: "TestFoo".to_string(),
        };
        app.registry.ensure_test(&id);
        if let Some(case) = app.registry.case_mut(&id) {
            case.output = "old output".to_string();
            case.status = TestStatus::Failed;
            case.panic = true;
        }
        app.mark_running(&id);
        let case = app.registry.case(&id).unwrap();
        assert_eq!(case.status, TestStatus::Running);
        assert!(case.output.is_empty());
        assert!(!case.panic);
    }
}
