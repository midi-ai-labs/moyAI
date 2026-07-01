use std::sync::{Arc, Mutex};

use crate::config::AccessMode;

#[derive(Clone, Debug)]
pub struct LiveConfigOverrides {
    state: Arc<Mutex<LiveConfigOverrideState>>,
}

#[derive(Debug)]
struct LiveConfigOverrideState {
    access_mode: AccessMode,
}

impl LiveConfigOverrides {
    pub fn new(access_mode: AccessMode) -> Self {
        Self {
            state: Arc::new(Mutex::new(LiveConfigOverrideState { access_mode })),
        }
    }

    pub fn access_mode(&self) -> AccessMode {
        self.state
            .lock()
            .expect("live config override mutex poisoned")
            .access_mode
    }

    pub fn set_access_mode(&self, access_mode: AccessMode) {
        self.state
            .lock()
            .expect("live config override mutex poisoned")
            .access_mode = access_mode;
    }
}
