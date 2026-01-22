use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runner::RunnerError;

#[derive(Clone, Debug)]
pub struct PackageInfo {
    pub import_path: String,
    pub dir: PathBuf,
}

pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join("go.mod").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

pub fn cache_dir(root: &Path) -> PathBuf {
    root.join(".gest")
}

pub fn cache_file(root: &Path) -> PathBuf {
    cache_dir(root).join("state.json")
}

pub fn ensure_cache_dir(root: &Path) -> Result<PathBuf, RunnerError> {
    let dir = cache_dir(root);
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|err| RunnerError::Io(err.to_string()))?;
    }
    Ok(dir)
}

pub fn list_packages(root: &Path, pattern: Option<&regex::Regex>) -> Result<Vec<PackageInfo>, RunnerError> {
    let output = Command::new("go")
        .arg("list")
        .arg("-f")
        .arg("{{.ImportPath}}|{{.Dir}}")
        .arg("./...")
        .current_dir(root)
        .output()
        .map_err(|err| RunnerError::Io(err.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RunnerError::GoList(stderr.trim().to_string()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut packages = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(2, '|');
        let import_path = parts.next().unwrap_or("").trim().to_string();
        let dir = parts.next().unwrap_or("").trim();
        if import_path.is_empty() || dir.is_empty() {
            continue;
        }
        if let Some(regex) = pattern {
            if !regex.is_match(&import_path) {
                continue;
            }
        }
        packages.push(PackageInfo {
            import_path,
            dir: PathBuf::from(dir),
        });
    }
    packages.sort_by(|a, b| b.dir.as_os_str().len().cmp(&a.dir.as_os_str().len()));
    Ok(packages)
}

pub fn package_for_path<'a>(packages: &'a [PackageInfo], path: &Path) -> Option<&'a PackageInfo> {
    let path = path.canonicalize().ok()?;
    packages.iter().find(|package| path.starts_with(&package.dir))
}
