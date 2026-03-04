use log::info;

use crate::config::Config;

/// Actions the timer engine asks the caller to perform.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Show a nudge notification. Caller should display it, then call `nudge_result()`.
    ShowNudge {
        tier_index: usize,
        summary: String,
        body: String,
        nudge_duration: u64,
    },
    /// User went idle during nudge — show a break countdown.
    /// Caller should display it, then call `break_result()`.
    ShowBreakCountdown {
        tier_index: usize,
        summary: String,
        body: String,
        duration: u64,
    },
    /// Passive break: inhibitors active, user is watching/listening.
    /// Show notification with "Skip" action for the break duration.
    /// If it expires → break taken. If user clicks Skip → call `passive_break_skipped()`.
    ShowPassiveBreak {
        tier_index: usize,
        summary: String,
        body: String,
        duration: u64,
    },
    /// Show a brief feedback notification (not blocking).
    ShowFeedback {
        summary: String,
        body: String,
    },
    /// No action needed.
    None,
}

/// Pure timer engine. No I/O, no sleep — caller drives it.
pub struct TimerEngine {
    /// Accumulated active time since last break slot (seconds).
    active_time: u64,
    /// Current slot index (increments each `base_interval`). Determines cycle position.
    slot_index: u64,
    /// Number of consecutive break skips. Resets when a break is taken or
    /// sufficient idle time is detected.
    skip_count: u32,
    /// Whether we're waiting for a `nudge_result` call.
    pending_nudge_tier: Option<usize>,
}

impl TimerEngine {
    pub const fn new() -> Self {
        Self {
            active_time: 0_u64,
            slot_index: 0_u64,
            skip_count: 0_u32,
            pending_nudge_tier: None,
        }
    }

    /// Determine which tier a slot belongs to, considering skip-based upgrades.
    /// Returns the tier index (into config.timer.breaks).
    fn resolve_tier(&self, config: &Config) -> usize {
        let breaks = &config.timer.breaks;
        if breaks.is_empty() {
            return 0;
        }

        // Find the natural tier: highest tier where slot_index is a multiple of `every`
        let slot = self.slot_index;
        let mut natural_tier = 0_usize;
        for (i, tier) in breaks.iter().enumerate() {
            if tier.every > 0 && slot.is_multiple_of(tier.every) {
                natural_tier = i;
            }
        }

        // Check if skip count warrants an upgrade
        let mut effective_tier = natural_tier;
        if self.skip_count > 0 {
            // Walk up from the natural tier: if we've exceeded max_skips, upgrade
            for i in natural_tier..breaks.len() {
                if self.skip_count >= breaks[i].max_skips && i + 1 < breaks.len() {
                    effective_tier = i + 1;
                } else {
                    break;
                }
            }
        }

        effective_tier
    }

    /// Advance active time. Returns an action if a break slot is reached.
    /// `inhibitors_active`: true if idle inhibitors are currently active
    /// (media playing, etc.) — triggers the passive break flow instead of nudge.
    pub fn tick(&mut self, elapsed: u64, config: &Config, inhibitors_active: bool) -> Action {
        self.active_time = self.active_time.saturating_add(elapsed);

        if self.active_time >= config.timer.base_interval {
            self.active_time = 0;
            self.slot_index = self.slot_index.wrapping_add(1);

            let tier_idx = self.resolve_tier(config);
            let tier = &config.timer.breaks[tier_idx];
            let (summary, body) = tier.message(self.skip_count);

            info!(
                "Slot {} → tier {} (skip_count={}, inhibitors={}): {}",
                self.slot_index, tier_idx, self.skip_count, inhibitors_active, summary
            );

            if inhibitors_active {
                self.pending_nudge_tier = None;
                Action::ShowPassiveBreak {
                    tier_index: tier_idx,
                    summary: summary.to_owned(),
                    body: body.to_owned(),
                    duration: tier.idle_threshold,
                }
            } else {
                self.pending_nudge_tier = Some(tier_idx);
                Action::ShowNudge {
                    tier_index: tier_idx,
                    summary: summary.to_owned(),
                    body: body.to_owned(),
                    nudge_duration: tier.nudge_duration,
                }
            }
        } else {
            Action::None
        }
    }

    /// Report the result of a nudge. `went_idle`: user went idle during the nudge.
    pub fn nudge_result(&mut self, went_idle: bool, config: &Config) -> Action {
        let Some(tier_idx) = self.pending_nudge_tier.take() else {
            return Action::None;
        };

        if went_idle {
            let tier = &config.timer.breaks[tier_idx];
            info!("Break started: tier {tier_idx}, countdown {}s", tier.idle_threshold);

            Action::ShowBreakCountdown {
                tier_index: tier_idx,
                summary: "Enjoy your break ☕".to_owned(),
                body: format_duration_sentence(tier.idle_threshold),
                duration: tier.idle_threshold,
            }
        } else {
            self.skip_count = self.skip_count.saturating_add(1);
            info!("Break skipped: tier {tier_idx}, skip_count now {}", self.skip_count);
            Action::None
        }
    }

    /// Report that a break countdown completed fully (user stayed idle).
    pub fn break_completed(&mut self, _tier_index: usize) {
        info!("Break completed — resetting skip count");
        self.skip_count = 0;
    }

    /// Passive break expired without being skipped — count as taken.
    pub fn passive_break_completed(&mut self, tier_index: usize) {
        info!("Passive break completed (tier {tier_index}) — resetting skip count");
        self.skip_count = 0;
    }

    /// User clicked Skip on a passive break notification.
    pub fn passive_break_skipped(&mut self, tier_index: usize) {
        self.skip_count = self.skip_count.saturating_add(1);
        info!(
            "Passive break skipped (tier {tier_index}) — skip_count now {}",
            self.skip_count
        );
    }

    /// Report that a break was interrupted (user resumed during countdown).
    #[expect(clippy::unused_self, clippy::needless_pass_by_ref_mut, reason = "may mutate state in the future")]
    pub fn break_interrupted(&mut self, _tier_index: usize) -> Action {
        info!("Break interrupted — not counting as taken");
        // Don't reset skip_count — the break wasn't completed
        Action::ShowFeedback {
            summary: "Break interrupted".to_owned(),
            body: "No worries — we'll remind you again".to_owned(),
        }
    }

    /// Handle an idle→resumed cycle (not during a nudge/break).
    /// `idle_duration` is the measured seconds the user was actually idle.
    /// `inhibitors_active`: if true, thresholds are multiplied by
    /// `idle_inhibitor_multiplier` — a short pause while watching a video
    /// shouldn't count as a real break.
    pub fn idle_resumed(&mut self, idle_duration: u64, config: &Config, inhibitors_active: bool) {
        let multiplier = if inhibitors_active {
            config.timer.idle_inhibitor_multiplier
        } else {
            1
        };

        // Find the highest tier whose (possibly multiplied) idle_threshold is met
        let mut highest_met: Option<usize> = None;
        for (i, tier) in config.timer.breaks.iter().enumerate() {
            let effective_threshold = tier.idle_threshold.saturating_mul(multiplier);
            if idle_duration >= effective_threshold {
                highest_met = Some(i);
            }
        }

        if let Some(tier_idx) = highest_met {
            if inhibitors_active {
                info!(
                    "Natural idle {idle_duration}s met tier {tier_idx} threshold (×{multiplier} for inhibitors) — resetting"
                );
            } else {
                info!(
                    "Natural idle {idle_duration}s met tier {tier_idx} threshold — resetting"
                );
            }
            self.skip_count = 0;
            self.active_time = 0;
        } else if inhibitors_active {
            info!("Natural idle {idle_duration}s — no tier threshold met (×{multiplier} for inhibitors)");
        } else {
            info!("Natural idle {idle_duration}s — no tier threshold met");
        }
    }

    /// System was suspended for a long time — full reset.
    pub fn suspend_detected(&mut self) {
        info!("System suspend detected — full reset");
        self.active_time = 0;
        self.skip_count = 0;
    }

    // Accessors for testing
    #[cfg(test)]
    pub fn active_time(&self) -> u64 {
        self.active_time
    }

    #[cfg(test)]
    pub fn slot_index(&self) -> u64 {
        self.slot_index
    }

    #[cfg(test)]
    pub fn skip_count(&self) -> u32 {
        self.skip_count
    }
}

pub fn format_duration(seconds: u64) -> String {
    let minutes = seconds / 60;
    let secs = seconds % 60;
    match (minutes, secs) {
        (0_u64, s) => format!("{s} seconds"),
        (m, 0_u64) => format!("{m} minutes"),
        (m, s) => format!("{m} minutes and {s} seconds"),
    }
}

fn format_duration_sentence(seconds: u64) -> String {
    format!("Relax for the next {}", format_duration(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn test_config() -> Config {
        Config {
            notification: Notification::default(),
            sleep: None,
            timer: Timer {
                ignore_idle_inhibitors: false,
                idle_inhibitor_multiplier: 5_u64,
                idle_detection_threshold: 10_u32,
                base_interval: 100_u64,
                breaks: vec![
                    BreakTier {
                        every: 1_u64,
                        nudge_duration: 10_u64,
                        idle_threshold: 20_u64,
                        max_skips: 2_u32,
                        messages: vec![
                            BreakMessage {
                                summary: "Short".to_owned(),
                                body: "Short break".to_owned(),
                            },
                            BreakMessage {
                                summary: "Short (escalated)".to_owned(),
                                body: "You skipped".to_owned(),
                            },
                        ],
                    },
                    BreakTier {
                        every: 3_u64,
                        nudge_duration: 10_u64,
                        idle_threshold: 60_u64,
                        max_skips: 1_u32,
                        messages: vec![
                            BreakMessage {
                                summary: "Long".to_owned(),
                                body: "Long break".to_owned(),
                            },
                            BreakMessage {
                                summary: "Long (escalated)".to_owned(),
                                body: "You really need to move".to_owned(),
                            },
                        ],
                    },
                ],
            },
        }
    }

    #[test]
    fn test_no_break_before_interval() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        let action = engine.tick(50, &config, false);
        assert_eq!(action, Action::None);
        assert_eq!(engine.active_time(), 50);
    }

    #[test]
    fn test_first_slot_is_short_break() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        let action = engine.tick(100, &config, false);
        assert!(matches!(action, Action::ShowNudge { tier_index: 0, .. }));
        assert_eq!(engine.slot_index(), 1); // slot 1: every=1 matches, every=3 doesn't
    }

    #[test]
    fn test_third_slot_is_long_break() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        // Slots 1, 2 — short breaks, take them
        for _ in 0..2_i32 {
            let _ = engine.tick(100, &config, false);
            engine.nudge_result(true, &config);
            engine.break_completed(0);
        }

        // Slot 3 — should be long break (3 % 3 == 0)
        let action = engine.tick(100, &config, false);
        assert!(matches!(
            action,
            Action::ShowNudge { tier_index: 1, ref summary, .. } if summary == "Long"
        ));
    }

    #[test]
    fn test_skip_escalates_message() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        // Slot 1 — skip
        let _ = engine.tick(100, &config, false);
        engine.nudge_result(false, &config);
        assert_eq!(engine.skip_count(), 1);

        // Slot 2 — should show escalated short message
        let action = engine.tick(100, &config, false);
        assert!(matches!(
            action,
            Action::ShowNudge { tier_index: 0, ref summary, .. } if summary == "Short (escalated)"
        ));
    }

    #[test]
    fn test_skip_upgrades_to_next_tier() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        // Skip slots 1 and 2 (max_skips=2 for short break)
        let _ = engine.tick(100, &config, false);
        engine.nudge_result(false, &config);
        let _ = engine.tick(100, &config, false);
        engine.nudge_result(false, &config);
        assert_eq!(engine.skip_count(), 2);

        // Slot 3 is naturally a long break (3%3==0), so it stays long
        // But even if it weren't, skip_count >= max_skips would upgrade it
        let action = engine.tick(100, &config, false);
        assert!(matches!(
            action,
            Action::ShowNudge { tier_index: 1, .. }
        ));
    }

    #[test]
    fn test_skip_upgrades_at_non_natural_slot() {
        // Config where max_skips=1 for short breaks — skipping once upgrades
        let config = Config {
            timer: Timer {
                base_interval: 100_u64,
                breaks: vec![
                    BreakTier {
                        every: 1_u64,
                        nudge_duration: 10_u64,
                        idle_threshold: 20_u64,
                        max_skips: 1_u32,
                        messages: vec![BreakMessage {
                            summary: "Short".to_owned(),
                            body: "".to_owned(),
                        }],
                    },
                    BreakTier {
                        every: 3_u64,
                        nudge_duration: 10_u64,
                        idle_threshold: 60_u64,
                        max_skips: 1_u32,
                        messages: vec![BreakMessage {
                            summary: "Long".to_owned(),
                            body: "".to_owned(),
                        }],
                    },
                ],
                ..Timer::default()
            },
            ..Config::default()
        };

        let mut engine = TimerEngine::new();

        // Skip slot 1
        let _ = engine.tick(100, &config, false);
        engine.nudge_result(false, &config);

        // Slot 2: skip_count=1 >= max_skips=1 → upgraded to long
        let action = engine.tick(100, &config, false);
        assert!(matches!(
            action,
            Action::ShowNudge { tier_index: 1, ref summary, .. } if summary == "Long"
        ));
    }

    #[test]
    fn test_break_taken_resets_skip_count() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        // Skip once
        let _ = engine.tick(100, &config, false);
        engine.nudge_result(false, &config);
        assert_eq!(engine.skip_count(), 1);

        // Take the break
        let _ = engine.tick(100, &config, false);
        let action = engine.nudge_result(true, &config);
        assert!(matches!(action, Action::ShowBreakCountdown { .. }));
        engine.break_completed(0);
        assert_eq!(engine.skip_count(), 0);
    }

    #[test]
    fn test_break_interrupted_keeps_skip_count() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        // Take a break but interrupt it
        let _ = engine.tick(100, &config, false);
        let _ = engine.nudge_result(true, &config);
        let action = engine.break_interrupted(0);
        assert!(matches!(action, Action::ShowFeedback { .. }));
        // skip_count should NOT have been reset
        assert_eq!(engine.skip_count(), 0); // it was 0 before, still 0
        // But importantly, break_completed was NOT called
    }

    #[test]
    fn test_nudge_accepted_shows_countdown() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        let _ = engine.tick(100, &config, false);
        let action = engine.nudge_result(true, &config);
        assert!(matches!(
            action,
            Action::ShowBreakCountdown { tier_index: 0, duration: 20, .. }
        ));
    }

    #[test]
    fn test_idle_resumed_resets_when_threshold_met() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        // Skip a break, accumulate time
        let _ = engine.tick(100, &config, false);
        engine.nudge_result(false, &config);
        let _ = engine.tick(50, &config, false);
        assert_eq!(engine.skip_count(), 1);

        // Idle for 60s — meets long break threshold
        engine.idle_resumed(60, &config, false);
        assert_eq!(engine.skip_count(), 0);
        assert_eq!(engine.active_time(), 0);
    }

    #[test]
    fn test_idle_resumed_no_reset_when_threshold_not_met() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        let _ = engine.tick(100, &config, false);
        engine.nudge_result(false, &config);
        let _ = engine.tick(50, &config, false);

        // Idle for 10s — doesn't meet any threshold
        engine.idle_resumed(10, &config, false);
        assert_eq!(engine.skip_count(), 1); // preserved
        assert_eq!(engine.active_time(), 50); // preserved
    }

    #[test]
    fn test_suspend_resets_everything() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        let _ = engine.tick(100, &config, false);
        engine.nudge_result(false, &config);
        let _ = engine.tick(50, &config, false);

        engine.suspend_detected();
        assert_eq!(engine.skip_count(), 0);
        assert_eq!(engine.active_time(), 0);
    }

    #[test]
    fn test_multiple_ticks_accumulate() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        for _ in 0..9_i32 {
            assert_eq!(engine.tick(10, &config, false), Action::None);
        }
        let action = engine.tick(10, &config, false);
        assert!(matches!(action, Action::ShowNudge { .. }));
    }

    #[test]
    fn test_cycle_repeats() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        // Run through 6 slots, taking every break
        let mut tiers = Vec::new();
        for _ in 0..6_i32 {
            let action = engine.tick(100, &config, false);
            if let Action::ShowNudge { tier_index, .. } = action {
                tiers.push(tier_index);
                engine.nudge_result(true, &config);
                engine.break_completed(tier_index);
            }
        }

        // Slots: 1(short), 2(short), 3(long), 4(short), 5(short), 6(long)
        assert_eq!(tiers, vec![0, 0, 1, 0, 0, 1]);
    }

    #[test]
    fn test_inhibitors_active_shows_passive_break() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        let action = engine.tick(100, &config, true); // inhibitors active
        assert!(matches!(
            action,
            Action::ShowPassiveBreak { tier_index: 0, .. }
        ));
    }

    #[test]
    fn test_passive_break_completed_resets_skips() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        // Skip once
        let _ = engine.tick(100, &config, false);
        engine.nudge_result(false, &config);
        assert_eq!(engine.skip_count(), 1);

        // Passive break expires (taken)
        let _ = engine.tick(100, &config, true);
        engine.passive_break_completed(0);
        assert_eq!(engine.skip_count(), 0);
    }

    #[test]
    fn test_passive_break_skipped_increments_skips() {
        let config = test_config();
        let mut engine = TimerEngine::new();

        let _ = engine.tick(100, &config, true);
        engine.passive_break_skipped(0);
        assert_eq!(engine.skip_count(), 1);
    }

    #[test]
    fn test_idle_resumed_with_inhibitors_requires_multiplied_threshold() {
        let config = test_config();
        // config: tier 0 idle_threshold=20, tier 1 idle_threshold=60
        // multiplier=5 → effective thresholds: 100, 300
        let mut engine = TimerEngine::new();

        let _ = engine.tick(100, &config, false);
        engine.nudge_result(false, &config);
        let _ = engine.tick(50, &config, false);
        assert_eq!(engine.skip_count(), 1);

        // 60s idle with inhibitors — doesn't meet 20*5=100
        engine.idle_resumed(60, &config, true);
        assert_eq!(engine.skip_count(), 1); // not reset
        assert_eq!(engine.active_time(), 50); // not reset

        // 100s idle with inhibitors — meets tier 0 threshold (20*5=100)
        engine.idle_resumed(100, &config, true);
        assert_eq!(engine.skip_count(), 0);
        assert_eq!(engine.active_time(), 0);
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(30), "30 seconds");
        assert_eq!(format_duration(60), "1 minutes");
        assert_eq!(format_duration(120), "2 minutes");
        assert_eq!(format_duration(90), "1 minutes and 30 seconds");
        assert_eq!(format_duration(0), "0 seconds");
    }
}
