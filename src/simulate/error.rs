//! Public error type for the simulator (work-in-progress).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AssetSimulatorError {
    #[error("executor initialisation failed: {0}")]
    ExecutorInit(String),
    // add concrete variants as the API stabilises …
}
