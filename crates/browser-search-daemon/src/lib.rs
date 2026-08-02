pub mod api;
pub mod bridge;
pub mod config;
pub mod error;
pub mod jobs;
pub mod model;

use std::sync::Arc;

use bridge::BridgeHub;
use config::Config;
use jobs::JobScheduler;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub bridge: Arc<BridgeHub>,
    pub scheduler: Arc<JobScheduler>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let config = Arc::new(config);
        let bridge = Arc::new(BridgeHub::new(config.clone()));
        let scheduler = Arc::new(JobScheduler::new(config.clone(), bridge.clone()));
        Self {
            config,
            bridge,
            scheduler,
        }
    }
}
