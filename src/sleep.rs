use std::time::Instant;

use log::info;

use crate::config::{Persistence, Sleep};

/// Actions the sleep engine asks the caller to perform.
#[derive(Debug, PartialEq, Eq)]
pub enum SleepAction {
    /// Show a notification and/or run a command.
    Escalate {
        index: usize,
        summary: Option<String>,
        body: String,
        command: Option<String>,
        persistence: PersistenceHint,
    },
    /// No action needed.
    None,
}

/// Hint for the caller about how to display the notification.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum PersistenceHint {
    /// Low urgency, 20s timeout
    Gentle,
    /// Normal urgency, 60s timeout
    Firm,
    /// Critical urgency, resident (no timeout)
    Persistent,
}

/// Tracks sleep reminder escalation state.
pub struct SleepEngine {
    /// Index of the next escalation to fire.
    next_escalation: usize,
    /// Whether we've been in the bedtime window.
    was_active: bool,
    /// For repeating escalations: when the last repeat was shown.
    last_repeat: Option<Instant>,
}

impl SleepEngine {
    pub const fn new() -> Self {
        Self {
            next_escalation: 0,
            was_active: false,
            last_repeat: None,
        }
    }

    /// Whether we are currently in bedtime mode.
    pub fn is_bedtime(&self, now_secs: u64, config: &Sleep) -> bool {
        config.is_bedtime(now_secs)
    }

    /// Check if any sleep escalation should fire.
    /// `now_secs`: current time as seconds since midnight.
    pub fn check(&mut self, now_secs: u64, config: &Sleep) -> SleepAction {
        let active = config.is_bedtime(now_secs);

        if !active {
            // Outside bedtime window — reset for today
            if self.was_active {
                info!("Sleep: resetting escalations (bedtime ended)");
                self.next_escalation = 0;
                self.was_active = false;
                self.last_repeat = None;
            }
            return SleepAction::None;
        }

        self.was_active = true;

        // Calculate seconds elapsed since start_time
        let start = config.start_time_secs();
        let elapsed = if now_secs >= start {
            now_secs - start
        } else {
            // Past midnight: time since start = (midnight - start) + now
            (24 * 3600 - start) + now_secs
        };

        // Check if the current (already-fired) escalation wants to repeat
        if self.next_escalation > 0 {
            let prev = &config.escalations[self.next_escalation - 1];
            if let Some(repeat_secs) = prev.repeat_every {
                let should_repeat = match self.last_repeat {
                    Some(last) => last.elapsed().as_secs() >= repeat_secs,
                    None => true, // first repeat after initial fire
                };
                // Only repeat if we've exhausted new escalations or the next one
                // hasn't triggered yet
                let next_not_ready = self.next_escalation >= config.escalations.len()
                    || elapsed < config.escalations[self.next_escalation].after;

                if should_repeat && next_not_ready {
                    self.last_repeat = Some(Instant::now());
                    info!(
                        "Sleep: repeating escalation {} (every {}s)",
                        self.next_escalation - 1,
                        repeat_secs
                    );
                    return SleepAction::Escalate {
                        index: self.next_escalation - 1,
                        summary: prev.summary.clone(),
                        body: prev.body.clone(),
                        command: None, // don't re-run commands on repeat
                        persistence: persistence_hint(&prev.persistence),
                    };
                }
            }
        }

        if self.next_escalation >= config.escalations.len() {
            return SleepAction::None; // all escalations fired, no repeat
        }

        let escalation = &config.escalations[self.next_escalation];

        if elapsed >= escalation.after {
            let idx = self.next_escalation;
            self.next_escalation += 1;
            self.last_repeat = None; // reset repeat timer for new escalation

            info!(
                "Sleep escalation {idx}: after={}s, elapsed={elapsed}s, \
                 persistence={:?}, summary={:?}, command={:?}",
                escalation.after, escalation.persistence,
                escalation.summary, escalation.command
            );

            SleepAction::Escalate {
                index: idx,
                summary: escalation.summary.clone(),
                body: escalation.body.clone(),
                command: escalation.command.clone(),
                persistence: persistence_hint(&escalation.persistence),
            }
        } else {
            SleepAction::None
        }
    }

    #[cfg(test)]
    pub fn next_escalation(&self) -> usize {
        self.next_escalation
    }
}

fn persistence_hint(p: &Persistence) -> PersistenceHint {
    match p {
        Persistence::Gentle => PersistenceHint::Gentle,
        Persistence::Firm => PersistenceHint::Firm,
        Persistence::Persistent => PersistenceHint::Persistent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Sleep, SleepEscalation};

    fn test_sleep_config() -> Sleep {
        Sleep {
            start_time: "23:00".to_owned(),
            end_time: "06:00".to_owned(),
            escalations: vec![
                SleepEscalation {
                    after: 0,
                    summary: Some("Bedtime".to_owned()),
                    body: "Time to sleep".to_owned(),
                    command: None,
                    persistence: Default::default(),
                    repeat_every: None,
                },
                SleepEscalation {
                    after: 1800,
                    summary: Some("Still up?".to_owned()),
                    body: "Go to bed".to_owned(),
                    command: Some("playerctl pause".to_owned()),
                    persistence: Default::default(),
                    repeat_every: None,
                },
                SleepEscalation {
                    after: 3600,
                    summary: None,
                    body: String::new(),
                    command: Some("echo grayscale".to_owned()),
                    persistence: Default::default(),
                    repeat_every: None,
                },
            ],
        }
    }

    #[test]
    fn test_no_action_before_start_time() {
        let config = test_sleep_config();
        let mut engine = SleepEngine::new();

        // 10:00 AM — well before 23:00
        let action = engine.check(10 * 3600, &config);
        assert_eq!(action, SleepAction::None);
    }

    #[test]
    fn test_first_escalation_at_start_time() {
        let config = test_sleep_config();
        let mut engine = SleepEngine::new();

        // Exactly 23:00
        let action = engine.check(23 * 3600, &config);
        assert!(matches!(
            action,
            SleepAction::Escalate { index: 0, ref summary, .. } if summary.as_deref() == Some("Bedtime")
        ));
    }

    #[test]
    fn test_second_escalation_after_delay() {
        let config = test_sleep_config();
        let mut engine = SleepEngine::new();

        // Fire first
        let _ = engine.check(23 * 3600, &config);

        // 23:15 — not yet 30 min
        let action = engine.check(23 * 3600 + 900, &config);
        assert_eq!(action, SleepAction::None);

        // 23:30 — 30 min elapsed
        let action = engine.check(23 * 3600 + 1800, &config);
        assert!(matches!(
            action,
            SleepAction::Escalate { index: 1, ref command, .. } if command.as_deref() == Some("playerctl pause")
        ));
    }

    #[test]
    fn test_escalation_past_midnight() {
        let config = test_sleep_config();
        let mut engine = SleepEngine::new();

        // Fire first two at 23:00 and 23:30
        let _ = engine.check(23 * 3600, &config);
        let _ = engine.check(23 * 3600 + 1800, &config);

        // 00:00 — 1 hour after 23:00, third escalation
        let action = engine.check(0, &config);
        assert!(matches!(
            action,
            SleepAction::Escalate { index: 2, ref command, .. } if command.as_deref() == Some("echo grayscale")
        ));
    }

    #[test]
    fn test_no_more_after_all_fired() {
        let config = test_sleep_config();
        let mut engine = SleepEngine::new();

        // Fire all three
        let _ = engine.check(23 * 3600, &config);
        let _ = engine.check(23 * 3600 + 1800, &config);
        let _ = engine.check(0, &config); // past midnight

        // Nothing left
        let action = engine.check(1 * 3600, &config);
        assert_eq!(action, SleepAction::None);
    }

    #[test]
    fn test_resets_next_day() {
        let config = test_sleep_config();
        let mut engine = SleepEngine::new();

        // Fire first escalation at 23:00
        let _ = engine.check(23 * 3600, &config);
        assert_eq!(engine.next_escalation(), 1);

        // Next day, 10:00 AM — outside bedtime window, resets
        let _ = engine.check(10 * 3600, &config);
        assert_eq!(engine.next_escalation(), 0);
    }

    #[test]
    fn test_start_time_parsing() {
        let config = test_sleep_config();
        assert_eq!(config.start_time_secs(), 23 * 3600);

        let early = Sleep {
            start_time: "06:30".to_owned(),
            end_time: "07:00".to_owned(),
            escalations: vec![],
        };
        assert_eq!(early.start_time_secs(), 6 * 3600 + 30 * 60);
    }

    #[test]
    fn test_is_bedtime() {
        let config = test_sleep_config(); // 23:00–06:00

        // Before bedtime
        assert!(!config.is_bedtime(22 * 3600));
        assert!(!config.is_bedtime(10 * 3600));

        // During bedtime (before midnight)
        assert!(config.is_bedtime(23 * 3600));
        assert!(config.is_bedtime(23 * 3600 + 1800));

        // During bedtime (after midnight)
        assert!(config.is_bedtime(0));
        assert!(config.is_bedtime(3 * 3600));
        assert!(config.is_bedtime(5 * 3600 + 3599));

        // After bedtime ends
        assert!(!config.is_bedtime(6 * 3600));
        assert!(!config.is_bedtime(12 * 3600));
    }
}
