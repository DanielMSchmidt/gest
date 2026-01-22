use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::go::{GoTestAction, GoTestEvent};

#[derive(Clone, Debug, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct TestId {
    pub package: String,
    pub name: String,
}

impl fmt::Display for TestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}", self.package, self.name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TestStatus {
    Unknown,
    Running,
    Passed,
    Failed,
}

#[derive(Clone, Debug)]
pub struct TestCase {
    pub status: TestStatus,
    pub output: String,
    pub panic: bool,
    pub has_children: bool,
    pub last_update: Option<Instant>,
}

impl Default for TestCase {
    fn default() -> Self {
        Self {
            status: TestStatus::Unknown,
            output: String::new(),
            panic: false,
            has_children: false,
            last_update: None,
        }
    }
}

#[derive(Default, Debug)]
pub struct TestRegistry {
    tests: HashMap<TestId, TestCase>,
    order: Vec<TestId>,
    order_index: HashMap<TestId, usize>,
    parents: HashSet<TestId>,
    package_state: HashMap<String, PackageState>,
}

#[derive(Default, Debug)]
struct PackageState {
    current_test: Option<String>,
}

impl TestRegistry {
    pub fn ensure_test(&mut self, id: &TestId) {
        if !self.tests.contains_key(id) {
            self.insert_test(id.clone(), TestCase::default());
        }
    }

    pub fn apply_event(&mut self, event: &GoTestEvent) {
        let package = event.package.clone();
        if let Some(test_name) = event.test.as_ref() {
            self.mark_parents(&package, test_name);
        }

        match event.action {
            GoTestAction::Run => {
                if let Some(test) = event.test.as_ref() {
                    let id = TestId {
                        package: package.clone(),
                        name: test.clone(),
                    };
                    let case = self.tests.entry(id.clone()).or_default();
                    case.status = TestStatus::Running;
                    case.output.clear();
                    case.panic = false;
                    case.last_update = Some(Instant::now());
                    self.track_order(id);
                    self.package_state
                        .entry(package.clone())
                        .or_default()
                        .current_test = Some(test.clone());
                }
            }
            GoTestAction::Pass | GoTestAction::Fail | GoTestAction::Skip => {
                if let Some(test) = event.test.as_ref() {
                    let id = TestId {
                        package: package.clone(),
                        name: test.clone(),
                    };
                    let case = self.tests.entry(id.clone()).or_default();
                    case.status = match event.action {
                        GoTestAction::Pass => TestStatus::Passed,
                        GoTestAction::Fail => TestStatus::Failed,
                        GoTestAction::Skip => TestStatus::Unknown,
                        _ => case.status,
                    };
                    case.last_update = Some(Instant::now());
                    self.track_order(id);
                    self.package_state
                        .entry(package.clone())
                        .or_default()
                        .current_test = Some(test.clone());
                }
            }
            GoTestAction::Output => {
                if event.test.is_none() {
                    if let Some(output) = event.output.as_ref() {
                        if is_harness_output(output) {
                            return;
                        }
                    }
                }
                let current_test = self
                    .package_state
                    .get(&package)
                    .and_then(|state| state.current_test.clone());
                let target = event.test.clone().or(current_test);
                if let Some(test) = target {
                    let id = TestId {
                        package: package.clone(),
                        name: test,
                    };
                    let case = self.tests.entry(id.clone()).or_default();
                    if let Some(output) = event.output.as_ref() {
                        let sanitized = sanitize_output(output);
                        case.output.push_str(&sanitized);
                        if is_panic_output(output) {
                            case.panic = true;
                        }
                    }
                    case.last_update = Some(Instant::now());
                    self.track_order(id);
                }
                if let Some(test) = event.test.as_ref() {
                    self.package_state
                        .entry(package.clone())
                        .or_default()
                        .current_test = Some(test.clone());
                }
            }
            GoTestAction::Other => {}
        }
    }

    pub fn case(&self, id: &TestId) -> Option<&TestCase> {
        self.tests.get(id)
    }

    pub fn case_mut(&mut self, id: &TestId) -> Option<&mut TestCase> {
        self.tests.get_mut(id)
    }

    pub fn is_parent(&self, id: &TestId) -> bool {
        self.parents.contains(id)
    }

    pub fn leaf_tests(&self) -> Vec<TestId> {
        self.order
            .iter()
            .filter(|id| !self.parents.contains(*id))
            .cloned()
            .collect()
    }

    pub fn failed_tests(&self) -> Vec<TestId> {
        self.order
            .iter()
            .filter(|id| {
                !self.parents.contains(*id)
                    && self
                        .tests
                        .get(*id)
                        .map(|case| case.status == TestStatus::Failed)
                        .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    pub fn order_index(&self, id: &TestId) -> usize {
        self.order_index.get(id).cloned().unwrap_or(usize::MAX)
    }

    fn insert_test(&mut self, id: TestId, case: TestCase) {
        self.track_order(id.clone());
        self.tests.insert(id, case);
    }

    fn track_order(&mut self, id: TestId) {
        if let std::collections::hash_map::Entry::Vacant(entry) = self.order_index.entry(id) {
            let index = self.order.len();
            self.order.push(entry.key().clone());
            entry.insert(index);
        }
    }

    fn mark_parents(&mut self, package: &str, test_name: &str) {
        let parts: Vec<&str> = test_name.split('/').collect();
        if parts.len() <= 1 {
            return;
        }
        for idx in 1..parts.len() {
            let parent_name = parts[..idx].join("/");
            let parent_id = TestId {
                package: package.to_string(),
                name: parent_name,
            };
            self.parents.insert(parent_id.clone());
            let case = self.tests.entry(parent_id.clone()).or_default();
            case.has_children = true;
            self.track_order(parent_id);
        }
    }
}

fn is_panic_output(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("panic:")
        || trimmed.starts_with("fatal error")
        || trimmed.contains("panic:")
}

fn is_harness_output(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "PASS"
        || trimmed == "FAIL"
        || trimmed.starts_with("ok ")
        || trimmed.starts_with("ok\t")
        || trimmed.starts_with("FAIL\t")
        || trimmed.starts_with("?\t")
        || trimmed.starts_with("?  ")
}

fn sanitize_output(output: &str) -> String {
    let mut cleaned = String::with_capacity(output.len());
    let mut in_escape = false;
    for ch in output.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        match ch {
            '\x1b' => in_escape = true,
            '\r' => cleaned.push('\n'),
            '\u{0008}' => {}
            _ if ch.is_control() && ch != '\n' && ch != '\t' => {}
            _ => cleaned.push(ch),
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::go::GoTestAction;

    #[test]
    fn marks_leaf_tests_only() {
        let mut registry = TestRegistry::default();
        let events = vec![
            GoTestEvent {
                action: GoTestAction::Run,
                package: "example".to_string(),
                test: Some("TestFoo".to_string()),
                output: None,
                elapsed: None,
            },
            GoTestEvent {
                action: GoTestAction::Run,
                package: "example".to_string(),
                test: Some("TestFoo/Sub".to_string()),
                output: None,
                elapsed: None,
            },
            GoTestEvent {
                action: GoTestAction::Pass,
                package: "example".to_string(),
                test: Some("TestFoo/Sub".to_string()),
                output: None,
                elapsed: None,
            },
        ];

        for event in events {
            registry.apply_event(&event);
        }

        let leaf = registry.leaf_tests();
        assert_eq!(leaf.len(), 1);
        assert_eq!(leaf[0].name, "TestFoo/Sub");
    }

    #[test]
    fn detects_panic_output() {
        let mut registry = TestRegistry::default();
        let event = GoTestEvent {
            action: GoTestAction::Output,
            package: "example".to_string(),
            test: Some("TestFoo".to_string()),
            output: Some("panic: boom".to_string()),
            elapsed: None,
        };

        registry.apply_event(&event);
        let id = TestId {
            package: "example".to_string(),
            name: "TestFoo".to_string(),
        };
        let case = registry.case(&id).unwrap();
        assert!(case.panic);
    }

    #[test]
    fn assigns_output_to_current_test() {
        let mut registry = TestRegistry::default();
        let run = GoTestEvent {
            action: GoTestAction::Run,
            package: "example".to_string(),
            test: Some("TestFoo".to_string()),
            output: None,
            elapsed: None,
        };
        registry.apply_event(&run);
        let output = GoTestEvent {
            action: GoTestAction::Output,
            package: "example".to_string(),
            test: None,
            output: Some("line one\n".to_string()),
            elapsed: None,
        };
        registry.apply_event(&output);
        let id = TestId {
            package: "example".to_string(),
            name: "TestFoo".to_string(),
        };
        let case = registry.case(&id).unwrap();
        assert!(case.output.contains("line one"));
    }

    #[test]
    fn strips_control_sequences_from_output() {
        let mut registry = TestRegistry::default();
        let event = GoTestEvent {
            action: GoTestAction::Output,
            package: "example".to_string(),
            test: Some("TestFoo".to_string()),
            output: Some("\x1b[31mline one\rline two\n".to_string()),
            elapsed: None,
        };
        registry.apply_event(&event);
        let id = TestId {
            package: "example".to_string(),
            name: "TestFoo".to_string(),
        };
        let case = registry.case(&id).unwrap();
        assert!(!case.output.contains("\x1b"));
        assert!(!case.output.contains("\r"));
        assert!(case.output.contains("line one"));
        assert!(case.output.contains("line two"));
    }

    #[test]
    fn ignores_harness_output_without_test() {
        let mut registry = TestRegistry::default();
        let run = GoTestEvent {
            action: GoTestAction::Run,
            package: "example".to_string(),
            test: Some("TestFoo".to_string()),
            output: None,
            elapsed: None,
        };
        registry.apply_event(&run);
        let output = GoTestEvent {
            action: GoTestAction::Output,
            package: "example".to_string(),
            test: None,
            output: Some("PASS\n".to_string()),
            elapsed: None,
        };
        registry.apply_event(&output);
        let id = TestId {
            package: "example".to_string(),
            name: "TestFoo".to_string(),
        };
        let case = registry.case(&id).unwrap();
        assert!(!case.output.contains("PASS"));
    }
}
