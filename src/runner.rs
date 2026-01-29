use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    pub no_test_cache_override: Option<bool>,
    pub timeout: Option<Duration>,
}

#[derive(Debug)]
pub enum RunnerCommand {
    Run(RunSpec),
    Cancel { run_id: Option<u64> },
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
    pub no_test_cache: bool,
    pub test_command: Option<Vec<String>>,
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

struct ActiveRun {
    run_id: u64,
    cancel: AtomicBool,
    completed: AtomicBool,
    cancel_reason: Mutex<Option<String>>,
    children: Mutex<Vec<Arc<Mutex<Child>>>>,
}

impl ActiveRun {
    fn new(run_id: u64) -> Self {
        Self {
            run_id,
            cancel: AtomicBool::new(false),
            completed: AtomicBool::new(false),
            cancel_reason: Mutex::new(None),
            children: Mutex::new(Vec::new()),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn cancel(&self, reason: impl Into<String>) -> bool {
        if self.completed.load(Ordering::SeqCst) {
            return false;
        }
        if self.cancel.swap(true, Ordering::SeqCst) {
            return false;
        }
        let mut reason_guard = self.cancel_reason.lock().unwrap();
        if reason_guard.is_none() {
            *reason_guard = Some(reason.into());
        }
        drop(reason_guard);
        let children = {
            let guard = self.children.lock().unwrap();
            guard.clone()
        };
        for child in children {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        }
        true
    }

    fn cancel_message(&self) -> Option<String> {
        self.cancel_reason.lock().unwrap().clone()
    }

    fn finish(&self) {
        self.completed.store(true, Ordering::SeqCst);
    }
}

struct ActiveRunHandle {
    run: Arc<ActiveRun>,
    thread: std::thread::JoinHandle<()>,
}

fn runner_loop(rx: Receiver<RunnerCommand>, config: RunnerConfig, event_tx: Sender<RunnerEvent>) {
    let next_run_id = Arc::new(AtomicU64::new(1));
    let mut active_run: Option<ActiveRunHandle> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            RunnerCommand::Run(spec) => {
                if let Some(active) = active_run.take() {
                    active.run.cancel("run cancelled by new request");
                    let _ = active.thread.join();
                }
                let run_id = next_run_id.fetch_add(1, Ordering::SeqCst);
                let run = Arc::new(ActiveRun::new(run_id));
                let run_clone = run.clone();
                let config = config.clone();
                let spec = spec.clone();
                let event_tx = event_tx.clone();
                let thread = std::thread::spawn(move || {
                    run_spec(run_id, &config, &spec, &event_tx, run_clone);
                });
                active_run = Some(ActiveRunHandle { run, thread });
            }
            RunnerCommand::Cancel { run_id } => {
                if let Some(active) = active_run.as_ref() {
                    if run_id.map_or(true, |id| id == active.run.run_id) {
                        active.run.cancel("run cancelled");
                    }
                }
            }
            RunnerCommand::Shutdown => break,
        }
    }
    if let Some(active) = active_run {
        active.run.cancel("runner shutdown");
        let _ = active.thread.join();
    }
}

fn run_spec(
    run_id: u64,
    config: &RunnerConfig,
    spec: &RunSpec,
    event_tx: &Sender<RunnerEvent>,
    active_run: Arc<ActiveRun>,
) {
    let no_test_cache = spec
        .no_test_cache_override
        .unwrap_or(config.no_test_cache);
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
        active_run.finish();
        return;
    }

    let (job_tx, job_rx) = crossbeam_channel::unbounded::<PackageRun>();
    let mut handles = Vec::new();
    let worker_count = config.pkg_concurrency.max(1);

    if let Some(timeout) = spec.timeout {
        let run = active_run.clone();
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            let message = format!("run timed out after {:?}", timeout);
            run.cancel(message);
        });
    }

    for _ in 0..worker_count {
        let job_rx = job_rx.clone();
        let event_tx = event_tx.clone();
        let root = config.root.clone();
        let go_test_p = config.go_test_p;
        let no_cache = no_test_cache;
        let test_command = config.test_command.clone();
        let run = active_run.clone();
        handles.push(std::thread::spawn(move || {
            while let Ok(job) = job_rx.recv() {
                if run.is_cancelled() {
                    break;
                }
                run_package(
                    run_id,
                    &root,
                    go_test_p,
                    no_cache,
                    job,
                    test_command.as_ref(),
                    &event_tx,
                    &run,
                );
            }
        }));
    }

    for job in &spec.packages {
        if active_run.is_cancelled() {
            break;
        }
        let _ = job_tx.send(job.clone());
    }
    drop(job_tx);

    for handle in handles {
        let _ = handle.join();
    }

    if active_run.is_cancelled() {
        let message = active_run
            .cancel_message()
            .unwrap_or_else(|| "run cancelled".to_string());
        let _ = event_tx.send(RunnerEvent::RunError { run_id, message });
    }
    let _ = event_tx.send(RunnerEvent::RunFinished {
        run_id,
        kind: spec.kind,
    });
    active_run.finish();
}

fn run_package(
    run_id: u64,
    root: &std::path::Path,
    go_test_p: usize,
    no_test_cache: bool,
    job: PackageRun,
    test_command: Option<&Vec<String>>,
    event_tx: &Sender<RunnerEvent>,
    active_run: &Arc<ActiveRun>,
) {
    if active_run.is_cancelled() {
        return;
    }
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

    if job.packages.is_empty() && test_command.is_none() {
        let _ = event_tx.send(RunnerEvent::RunError {
            run_id,
            message: "no packages provided for go test".to_string(),
        });
        return;
    }

    let mut cmd = build_command(
        root,
        go_test_p,
        no_test_cache,
        &job,
        test_command,
    );

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = event_tx.send(RunnerEvent::RunError {
                run_id,
                message: format!("failed to spawn go test: {}", err),
            });
            return;
        }
    };
    let child_handle = Arc::new(Mutex::new(child));
    {
        let mut guard = active_run.children.lock().unwrap();
        guard.push(child_handle.clone());
    }

    let stderr = {
        let mut guard = child_handle.lock().unwrap();
        guard.stderr.take()
    };
    if let Some(stderr) = stderr {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for _ in reader.lines() {
                // Ignore stderr lines to avoid blocking.
            }
        });
    }

    let stdout = {
        let mut guard = child_handle.lock().unwrap();
        guard.stdout.take()
    };
    if let Some(stdout) = stdout {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if active_run.is_cancelled() {
                break;
            }
            if let Some(mut event) = parse_go_test_line(&line) {
                if event.package.is_empty() {
                    event.package = package_label.clone();
                }
                let _ = event_tx.send(RunnerEvent::TestEvent { run_id, event });
            }
        }
    }

    let status = {
        let mut guard = child_handle.lock().unwrap();
        guard.wait().ok()
    };
    let success = status.map(|status| status.success()).unwrap_or(false);
    let _ = event_tx.send(RunnerEvent::PackageFinished {
        run_id,
        package: package_label,
        success,
    });
    let mut guard = active_run.children.lock().unwrap();
    guard.retain(|item| !Arc::ptr_eq(item, &child_handle));
}

fn build_command(
    root: &std::path::Path,
    go_test_p: usize,
    no_test_cache: bool,
    job: &PackageRun,
    test_command: Option<&Vec<String>>,
) -> Command {
    let mut cmd = if let Some(command) = test_command {
        let mut cmd = Command::new(
            command
                .first()
                .map(String::as_str)
                .unwrap_or("sleep"),
        );
        if command.len() > 1 {
            cmd.args(&command[1..]);
        }
        cmd
    } else {
        let mut cmd = Command::new("go");
        cmd.arg("test")
            .arg("-json")
            .arg(format!("-p={}", go_test_p));

        if no_test_cache {
            cmd.arg("-count=1");
        }

        if let Some(tests) = &job.tests {
            if !tests.is_empty() {
                let pattern = build_run_regex(tests);
                cmd.arg("-run").arg(pattern);
            }
        }

        cmd.args(&job.packages);
        cmd
    };

    cmd.current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    cmd
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
