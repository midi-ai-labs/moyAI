use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Starting { deadline: Instant },
    Active,
    Draining,
}

pub(super) struct Lifetime {
    phase: Phase,
}

impl Lifetime {
    pub(super) fn new(now: Instant, startup_grace: Duration) -> Self {
        Self {
            phase: Phase::Starting {
                deadline: now + startup_grace,
            },
        }
    }

    pub(super) fn admit(&mut self, now: Instant) -> Result<(), String> {
        if matches!(self.phase, Phase::Starting { deadline } if now >= deadline) {
            self.phase = Phase::Draining;
        }
        match self.phase {
            Phase::Starting { .. } | Phase::Active => {
                self.phase = Phase::Active;
                Ok(())
            }
            Phase::Draining => Err("Sandbox service is stopping; no family was admitted".into()),
        }
    }

    // The caller holds the same registry lock used by family admission. An
    // accepted family and the transition to Draining cannot cross each other.
    pub(super) fn observe_empty(&mut self, now: Instant, empty: bool) -> bool {
        if empty
            && (matches!(self.phase, Phase::Active)
                || matches!(self.phase, Phase::Starting { deadline } if now >= deadline))
        {
            self.phase = Phase::Draining;
        }
        matches!(self.phase, Phase::Draining)
    }

    pub(super) fn stop(&mut self) {
        self.phase = Phase::Draining;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_family_may_arrive_during_startup_but_not_after_grace() {
        let now = Instant::now();
        let mut startup = Lifetime::new(now, Duration::from_secs(10));
        assert!(!startup.observe_empty(now + Duration::from_secs(9), true));
        assert!(startup.admit(now + Duration::from_secs(9)).is_ok());
        assert!(!startup.observe_empty(now + Duration::from_secs(11), false));

        let mut expired = Lifetime::new(now, Duration::from_secs(10));
        assert!(expired.admit(now + Duration::from_secs(10)).is_err());
        assert!(expired.observe_empty(now + Duration::from_secs(10), true));
    }

    #[test]
    fn admission_winning_the_idle_race_keeps_the_service_available() {
        let now = Instant::now();
        let mut lifetime = Lifetime::new(now, Duration::from_secs(10));
        lifetime.admit(now).unwrap();
        // The new family is inserted under the admission lock before idle is observed.
        lifetime.admit(now).unwrap();
        assert!(!lifetime.observe_empty(now, false));
        assert!(lifetime.observe_empty(now, true));
        assert!(lifetime.admit(now).is_err());
    }

    #[test]
    fn idle_winning_the_race_does_not_report_a_new_family_as_ready() {
        let now = Instant::now();
        let mut lifetime = Lifetime::new(now, Duration::from_secs(10));
        lifetime.admit(now).unwrap();
        assert!(lifetime.observe_empty(now, true));
        assert!(lifetime.admit(now).is_err());
        assert!(lifetime.admit(now + Duration::from_secs(1)).is_err());
    }

    #[test]
    fn explicit_stop_rejects_admission_even_with_existing_families() {
        let now = Instant::now();
        let mut lifetime = Lifetime::new(now, Duration::from_secs(10));
        lifetime.admit(now).unwrap();
        lifetime.stop();
        assert!(lifetime.observe_empty(now, false));
        assert!(lifetime.admit(now).is_err());
    }
}
