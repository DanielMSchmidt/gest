use gest::go::parse_go_test_line;
use gest::model::{TestId, TestRegistry, TestStatus};

#[test]
fn parses_fixture_and_tracks_leaf_tests() {
    let data = include_str!("fixtures/subtests.jsonl");
    let mut registry = TestRegistry::default();
    for line in data.lines() {
        if let Some(event) = parse_go_test_line(line) {
            registry.apply_event(&event);
        }
    }

    let leaf = registry.leaf_tests();
    assert_eq!(leaf.len(), 1);
    assert_eq!(leaf[0].name, "TestFoo/Sub");

    let id = TestId {
        package: "example".to_string(),
        name: "TestFoo/Sub".to_string(),
    };
    let case = registry.case(&id).unwrap();
    assert_eq!(case.status, TestStatus::Failed);
    assert!(case.panic);
}

#[test]
fn ignores_package_harness_output() {
    let data = include_str!("fixtures/tfdiags_output.jsonl");
    let mut registry = TestRegistry::default();
    for line in data.lines() {
        if let Some(event) = parse_go_test_line(line) {
            registry.apply_event(&event);
        }
    }

    let id = TestId {
        package: "github.com/hashicorp/terraform/internal/tfdiags".to_string(),
        name: "TestDiagnosticComparer".to_string(),
    };
    let case = registry.case(&id).unwrap();
    assert!(!case.output.contains("PASS"));
    assert!(!case.output.contains("ok"));
}

#[test]
fn captures_panic_output_and_failure() {
    let data = include_str!("fixtures/tfdiags_panic.jsonl");
    let mut registry = TestRegistry::default();
    for line in data.lines() {
        if let Some(event) = parse_go_test_line(line) {
            registry.apply_event(&event);
        }
    }

    let id = TestId {
        package: "github.com/hashicorp/terraform/internal/tfdiags".to_string(),
        name: "TestDiagnosticsToHCL".to_string(),
    };
    let case = registry.case(&id).unwrap();
    assert!(case.panic);
    assert_eq!(case.status, TestStatus::Failed);
    assert!(case.output.contains("panic: this went wrong"));
    assert!(!case.output.contains("FAIL\tgithub.com/hashicorp/terraform/internal/tfdiags"));
}
