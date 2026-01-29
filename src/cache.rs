use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::TestId;
use crate::repo::PackageInfo;
use crate::runner::RunnerError;

const PACKAGE_CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageCache {
    generated_at: u64,
    go_mod_mtime: Option<u64>,
    go_work_mtime: Option<u64>,
    packages: Vec<PackageInfo>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CacheState {
    pub failing: Vec<TestId>,
    pub selected: Vec<TestId>,
    #[serde(default)]
    pub package_cache: Option<PackageCache>,
}

pub fn load_cache(path: &Path) -> Result<CacheState, RunnerError> {
    if !path.exists() {
        return Ok(CacheState::default());
    }
    let data = fs::read_to_string(path).map_err(|err| RunnerError::Io(err.to_string()))?;
    serde_json::from_str(&data).map_err(|err| RunnerError::Parse(err.to_string()))
}

pub fn save_cache(path: &Path, state: &CacheState) -> Result<(), RunnerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| RunnerError::Io(err.to_string()))?;
    }
    let data = serde_json::to_string_pretty(state).map_err(|err| RunnerError::Parse(err.to_string()))?;
    fs::write(path, data).map_err(|err| RunnerError::Io(err.to_string()))?;
    Ok(())
}

pub fn cached_packages(root: &Path, state: &CacheState) -> Option<Vec<PackageInfo>> {
    let cache = state.package_cache.as_ref()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    if now.saturating_sub(cache.generated_at) > PACKAGE_CACHE_TTL.as_secs() {
        return None;
    }
    let current_go_mod = file_mtime_secs(&root.join("go.mod"));
    let current_go_work = file_mtime_secs(&root.join("go.work"));
    if !mtimes_match(cache.go_mod_mtime, current_go_mod) {
        return None;
    }
    if !mtimes_match(cache.go_work_mtime, current_go_work) {
        return None;
    }
    Some(cache.packages.clone())
}

pub fn update_package_cache(
    root: &Path,
    state: &mut CacheState,
    packages: &[PackageInfo],
) -> Result<(), RunnerError> {
    state.package_cache = Some(PackageCache {
        generated_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| RunnerError::Io(err.to_string()))?
            .as_secs(),
        go_mod_mtime: file_mtime_secs(&root.join("go.mod")),
        go_work_mtime: file_mtime_secs(&root.join("go.work")),
        packages: packages.to_vec(),
    });
    Ok(())
}

fn file_mtime_secs(path: &Path) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    modified.duration_since(UNIX_EPOCH).ok().map(|dur| dur.as_secs())
}

fn mtimes_match(cached: Option<u64>, current: Option<u64>) -> bool {
    match (cached, current) {
        (Some(cached), Some(current)) => cached == current,
        (None, None) => true,
        _ => false,
    }
}
