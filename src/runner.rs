use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use thiserror::Error;

use crate::go::{parse_go_test_line, GoTestEvent};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RunKind {
    All,
    Failing,
    Selected,
    Single,
}

#[derive(Debug, Clone)]
pub struct PackageRun {
    pub packages: Vec<String>,
    pub tests: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct RunSpec {
    pub kind: RunKind,
    pub packages: Vec<PackageRun>,
}

#[derive(Debug)]
pub enum RunnerCommand {
    Run(RunSpec),
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum RunnerEvent {
    RunStarted {
        run_id: u64,
        kind: RunKind,
        packages: usize,
    },
    PackageStarted {
        run_id: u64,
        package: String,
    },
    TestEvent {
        run_id: u64,
        event: GoTestEvent,
    },
    PackageFinished {
        run_id: u64,
        package: String,
        success: bool,
    },
    RunFinished {
        run_id: u64,
        kind: RunKind,
    },
    RunError {
        run_id: u64,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub root: std::path::PathBuf,
    pub pkg_concurrency: usize,
    pub go_test_p: usize,
}

#[derive(Error, Debug)]
pub enum RunnerError {
    #[error("io error: {0}")]
    Io(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("go list failed: {0}")]
    GoList(String),
    #[error("go test failed: {0}")]
    GoTest(String),
}

pub fn start_runner(
    config: RunnerConfig,
    event_tx: Sender<RunnerEvent>,
) -> Sender<RunnerCommand> {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || runner_loop(cmd_rx, config, event_tx));
    cmd_tx
}

fn runner_loop(rx: Receiver<RunnerCommand>, config: RunnerConfig, event_tx: Sender<RunnerEvent>) {
    let next_run_id = Arc::new(AtomicU64::new(1));
    while let Ok(cmd) = rx.recv() {
        match cmd {
            RunnerCommand::Run(spec) => {
                let run_id = next_run_id.fetch_add(1, Ordering::SeqCst);
                run_spec(run_id, &config, &spec, &event_tx);
            }
            RunnerCommand::Shutdown => break,
        }
    }
}

fn run_spec(run_id: u64, config: &RunnerConfig, spec: &RunSpec, event_tx: &Sender<RunnerEvent>) {
    let _ = event_tx.send(RunnerEvent::RunStarted {
        run_id,
        kind: spec.kind,
        packages: spec.packages.len(),
    });

    if spec.packages.is_empty() {
        let _ = event_tx.send(RunnerEvent::RunFinished {
            run_id,
            kind: spec.kind,
        });
        return;
    }

    let (job_tx, job_rx) = crossbeam_channel::unbounded::<PackageRun>();
    let mut handles = Vec::new();
    let worker_count = config.pkg_concurrency.max(1);

    for _ in 0..worker_count {
        let job_rx = job_rx.clone();
        let event_tx = event_tx.clone();
        let root = config.root.clone();
        let go_test_p = config.go_test_p;
        handles.push(std::thread::spawn(move || {
            while let Ok(job) = job_rx.recv() {
                run_package(run_id, &root, go_test_p, job, &event_tx);
            }
        }));
    }

    for job in &spec.packages {
        let _ = job_tx.send(job.clone());
    }
    drop(job_tx);

    for handle in handles {
        let _ = handle.join();
    }

    let _ = event_tx.send(RunnerEvent::RunFinished {
        run_id,
        kind: spec.kind,
    });
}

fn run_package(
    run_id: u64,
    root: &std::path::Path,
    go_test_p: usize,
    job: PackageRun,
    event_tx: &Sender<RunnerEvent>,
) {
    let package_label = if job.packages.len() == 1 {
        job.packages
            .first()
            .cloned()
            .unwrap_or_else(|| "(unknown)".to_string())
    } else {
        format!("{} packages", job.packages.len())
    };
    let _ = event_tx.send(RunnerEvent::PackageStarted {
        run_id,
        package: package_label.clone(),
    });

    let mut cmd = Command::new("go");
    cmd.arg("test")
        .arg("-json")
        .arg("-count=1")
        .arg(format!("-p={}", go_test_p));

    if let Some(tests) = &job.tests {
        if !tests.is_empty() {
            let pattern = build_run_regex(tests);
            cmd.arg("-run").arg(pattern);
        }
    }

    if job.packages.is_empty() {
        let _ = event_tx.send(RunnerEvent::RunError {
            run_id,
            message: "no packages provided for go test".to_string(),
        });
        return;
    }

    cmd.args(&job.packages)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = event_tx.send(RunnerEvent::RunError {
                run_id,
                message: format!("failed to spawn go test: {}", err),
            });
            return;
        }
    };

    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for _ in reader.lines() {
                // Ignore stderr lines to avoid blocking.
            }
        });
    }

    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(mut event) = parse_go_test_line(&line) {
                if event.package.is_empty() {
                    event.package = package_label.clone();
                }
                let _ = event_tx.send(RunnerEvent::TestEvent { run_id, event });
            }
        }
    }

    let status = child.wait().ok();
    let success = status.map(|status| status.success()).unwrap_or(false);
    let _ = event_tx.send(RunnerEvent::PackageFinished {
        run_id,
        package: package_label,
        success,
    });
}

fn build_run_regex(tests: &[String]) -> String {
    let mut parts = Vec::new();
    for test in tests {
        parts.push(regex::escape(test));
    }
    format!("^({})$", parts.join("|"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_run_regex() {
        let tests = vec!["TestFoo/Sub".to_string(), "TestBar".to_string()];
        let regex = build_run_regex(&tests);
        assert!(regex.contains("TestFoo/Sub"));
        assert!(regex.contains("TestBar"));
    }
}
