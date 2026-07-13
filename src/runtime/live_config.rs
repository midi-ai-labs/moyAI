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

    pub fn apply_to(&self, config: &mut crate::config::ResolvedConfig) {
        config.permissions.access_mode = self.access_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_access_mode_is_the_authority_when_materializing_a_run_config() {
        let mut config = crate::config::ResolvedConfig::default();
        config.permissions.access_mode = AccessMode::Default;
        let live = LiveConfigOverrides::new(AccessMode::FullAccess);

        live.apply_to(&mut config);

        assert_eq!(config.permissions.access_mode, AccessMode::FullAccess);
    }
}
