use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoTestAction {
    Run,
    Pass,
    Fail,
    Skip,
    Output,
    Other,
}

#[derive(Debug, Clone)]
pub struct GoTestEvent {
    pub action: GoTestAction,
    pub package: String,
    pub test: Option<String>,
    pub output: Option<String>,
    pub elapsed: Option<f64>,
}

#[derive(Deserialize)]
struct RawGoTestEvent {
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "Package")]
    package: Option<String>,
    #[serde(rename = "Test")]
    test: Option<String>,
    #[serde(rename = "Output")]
    output: Option<String>,
    #[serde(rename = "Elapsed")]
    elapsed: Option<f64>,
}

pub fn parse_go_test_line(line: &str) -> Option<GoTestEvent> {
    let raw: RawGoTestEvent = serde_json::from_str(line).ok()?;
    let action = match raw.action.as_str() {
        "run" => GoTestAction::Run,
        "pass" => GoTestAction::Pass,
        "fail" => GoTestAction::Fail,
        "skip" => GoTestAction::Skip,
        "output" => GoTestAction::Output,
        _ => GoTestAction::Other,
    };
    Some(GoTestEvent {
        action,
        package: raw.package.unwrap_or_default(),
        test: raw.test,
        output: raw.output,
        elapsed: raw.elapsed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_run_event() {
        let line = r#"{"Action":"run","Package":"example","Test":"TestFoo"}"#;
        let event = parse_go_test_line(line).unwrap();
        assert_eq!(event.action, GoTestAction::Run);
        assert_eq!(event.package, "example");
        assert_eq!(event.test.as_deref(), Some("TestFoo"));
    }

    #[test]
    fn parses_output_event() {
        let line = r#"{"Action":"output","Package":"example","Test":"TestFoo","Output":"panic: boom\n"}"#;
        let event = parse_go_test_line(line).unwrap();
        assert_eq!(event.action, GoTestAction::Output);
        assert_eq!(event.output.as_deref(), Some("panic: boom\n"));
    }
}
