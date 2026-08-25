use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ControlDoubleTap {
    last_release: Option<Instant>,
    was_down: bool,
    window: Duration,
}

impl Default for ControlDoubleTap {
    fn default() -> Self {
        Self::new(Duration::from_millis(360))
    }
}

impl ControlDoubleTap {
    pub fn new(window: Duration) -> Self {
        Self {
            last_release: None,
            was_down: false,
            window,
        }
    }

    pub fn key_down(&mut self) -> bool {
        if self.was_down {
            return false;
        }
        self.was_down = true;
        false
    }

    pub fn key_up(&mut self, now: Instant) -> bool {
        if !self.was_down {
            return false;
        }
        self.was_down = false;
        let triggered = self
            .last_release
            .map(|last| now.duration_since(last) <= self.window)
            .unwrap_or(false);
        self.last_release = if triggered { None } else { Some(now) };
        triggered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_double_tap_triggers_once() {
        let start = Instant::now();
        let mut detector = ControlDoubleTap::new(Duration::from_millis(360));
        assert!(!detector.key_down());
        assert!(!detector.key_up(start));
        assert!(!detector.key_down());
        assert!(detector.key_up(start + Duration::from_millis(140)));
        assert!(!detector.key_up(start + Duration::from_millis(180)));
    }

    #[test]
    fn slow_double_tap_does_not_trigger() {
        let start = Instant::now();
        let mut detector = ControlDoubleTap::new(Duration::from_millis(120));
        detector.key_down();
        assert!(!detector.key_up(start));
        detector.key_down();
        assert!(!detector.key_up(start + Duration::from_millis(300)));
    }
}
