use crate::session::RunEvent;

use super::state::AppState;

pub fn reduce_run_event(state: &mut AppState, event: &RunEvent) {
    state.apply_run_event(event);
}
