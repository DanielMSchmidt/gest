use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::TestId;
use crate::runner::RunnerError;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CacheState {
    pub failing: Vec<TestId>,
    pub selected: Vec<TestId>,
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
