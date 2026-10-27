use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Load {
    #[default]
    Idle,
    Requested,
    Loading,
    /// A request came in while loading; load again once this one finishes.
    LoadingThenAgain,
}

/// When to (re)load one piece of server state, so requests never overlap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refresh {
    load: Load,
    previous_start: Option<Instant>,
}

impl Refresh {
    pub fn request(&mut self) {
        self.load = match self.load {
            Load::Idle | Load::Requested => Load::Requested,
            Load::Loading | Load::LoadingThenAgain => Load::LoadingThenAgain,
        };
    }

    /// Requested, or idle for longer than `every` when polling.
    pub fn is_due(&self, now: Instant, every: Option<Duration>) -> bool {
        match self.load {
            Load::Requested => true,
            Load::Idle => every.is_some_and(|every| {
                self.previous_start
                    .is_none_or(|start| now.duration_since(start) >= every)
            }),
            Load::Loading | Load::LoadingThenAgain => false,
        }
    }

    pub fn start(&mut self, now: Instant) {
        *self = Self {
            load: Load::Loading,
            previous_start: Some(now),
        };
    }

    pub fn finish(&mut self) {
        self.load = match self.load {
            Load::LoadingThenAgain => Load::Requested,
            _ => Load::Idle,
        };
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.load, Load::Loading | Load::LoadingThenAgain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY: Option<Duration> = Some(Duration::from_secs(5));

    #[test]
    fn a_request_is_due_at_once() {
        let now = Instant::now();
        let mut refresh = Refresh::default();
        refresh.start(now);
        refresh.finish();
        assert!(!refresh.is_due(now, EVERY));

        refresh.request();
        assert!(refresh.is_due(now, EVERY));
    }

    #[test]
    fn polling_waits_for_the_interval() {
        let now = Instant::now();
        let mut refresh = Refresh::default();
        assert!(refresh.is_due(now, EVERY), "never loaded");
        assert!(!refresh.is_due(now, None), "not polling");

        refresh.start(now);
        refresh.finish();
        assert!(!refresh.is_due(now + Duration::from_secs(4), EVERY));
        assert!(refresh.is_due(now + Duration::from_secs(5), EVERY));
    }

    #[test]
    fn requests_while_loading_run_after_it_not_alongside() {
        let now = Instant::now();
        let mut refresh = Refresh::default();
        refresh.start(now);
        refresh.request();
        assert!(!refresh.is_due(now, EVERY));
        assert!(refresh.is_loading());

        refresh.finish();
        assert!(refresh.is_due(now, EVERY));
        assert!(!refresh.is_loading());
    }
}
