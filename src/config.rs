use log::{info, warn};

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub notification: Notification,
    pub timer: Timer,
    pub sleep: Option<Sleep>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Sleep {
    /// Time of day to start sleep reminders, e.g. "23:00"
    pub start_time: String,
    /// Escalating actions after `start_time`
    #[serde(default)]
    pub escalations: Vec<SleepEscalation>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SleepEscalation {
    /// Seconds after `start_time` to trigger this escalation
    #[serde(default)]
    pub after: u64,
    /// Notification summary (optional — if absent, no notification)
    pub summary: Option<String>,
    /// Notification body
    #[serde(default)]
    pub body: String,
    /// Shell command to run (optional)
    pub command: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct Notification {
    pub urgency: Urgency,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Urgency {
    #[default]
    Low,
    Normal,
    Critical,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct Timer {
    pub ignore_idle_inhibitors: bool,
    /// Seconds without input before the Wayland compositor reports idle.
    /// Used to detect break-in-progress during nudges.
    pub idle_detection_threshold: u32,
    /// Seconds of active time between break slots.
    pub base_interval: u64,
    /// Multiplier for idle thresholds when idle inhibitors are active.
    /// E.g. 5 means a 120s threshold becomes 600s when media is playing.
    #[serde(default = "default_idle_inhibitor_multiplier")]
    pub idle_inhibitor_multiplier: u64,
    /// Break tier definitions, ordered from most frequent to least frequent.
    pub breaks: Vec<BreakTier>,
}

const fn default_idle_inhibitor_multiplier() -> u64 {
    5_u64
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BreakTier {
    /// This break type occurs every N slots (e.g. every=1 means every slot,
    /// every=3 means every 3rd slot). Higher-tier breaks replace lower-tier
    /// ones at their slot.
    pub every: u64,
    /// How long the nudge notification shows (seconds).
    pub nudge_duration: u64,
    /// How long without input counts as "break taken" (seconds).
    /// Also the countdown duration when a break starts.
    pub idle_threshold: u64,
    /// After this many consecutive skips, upgrade to the next tier.
    /// If this is the highest tier, the last message just repeats.
    #[serde(default = "default_max_skips")]
    pub max_skips: u32,
    /// Escalating messages within this tier. Index by skip count (clamped).
    #[serde(default)]
    pub messages: Vec<BreakMessage>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BreakMessage {
    pub summary: String,
    pub body: String,
}

const fn default_max_skips() -> u32 {
    2_u32
}


impl Default for Notification {
    fn default() -> Self {
        Self {
            urgency: Urgency::Low,
        }
    }
}


impl Default for Timer {
    fn default() -> Self {
        Self {
            ignore_idle_inhibitors: false,
            idle_detection_threshold: 10_u32,
            base_interval: 1200_u64, // 20 minutes
            idle_inhibitor_multiplier: default_idle_inhibitor_multiplier(),
            breaks: vec![
                BreakTier {
                    every: 1_u64,
                    nudge_duration: 60_u64,
                    idle_threshold: 120_u64,
                    max_skips: 2_u32,
                    messages: vec![
                        BreakMessage {
                            summary: "Quick stretch 🙆".to_owned(),
                            body: "Rest your eyes and stretch".to_owned(),
                        },
                        BreakMessage {
                            summary: "Break reminder 👀".to_owned(),
                            body: "You skipped the last one — take a break".to_owned(),
                        },
                    ],
                },
                BreakTier {
                    every: 3_u64,
                    nudge_duration: 60_u64,
                    idle_threshold: 300_u64,
                    max_skips: 1_u32,
                    messages: vec![
                        BreakMessage {
                            summary: "Time to move ☕".to_owned(),
                            body: "Get up and walk around".to_owned(),
                        },
                        BreakMessage {
                            summary: "Movement break overdue 🚶".to_owned(),
                            body: "Your body needs you to get up and move".to_owned(),
                        },
                    ],
                },
            ],
        }
    }
}

impl Sleep {
    /// Parse `start_time` "HH:MM" into seconds since midnight.
    pub fn start_time_secs(&self) -> u64 {
        let parts: Vec<&str> = self.start_time.split(':').collect();
        assert!(parts.len() == 2, "Invalid start_time format: '{}' (expected HH:MM)", self.start_time);
        let hours: u64 = parts[0].parse().expect("Invalid hour in start_time");
        let minutes: u64 = parts[1].parse().expect("Invalid minute in start_time");
        hours * 3600 + minutes * 60
    }
}

impl BreakTier {
    /// Get the message for the given skip count, clamping to the last message.
    pub fn message(&self, skip_count: u32) -> (&str, &str) {
        if self.messages.is_empty() {
            return ("Break Time!", "Take a break");
        }
        let idx = (skip_count as usize).min(self.messages.len() - 1);
        (self.messages[idx].summary.as_str(), self.messages[idx].body.as_str())
    }
}

/// Legacy config format (v2.x original) for automatic migration.
#[derive(serde::Deserialize)]
struct LegacyTimer {
    ignore_idle_inhibitors: Option<bool>,
    idle_timeout: Option<u32>,
    short_break_timeout: Option<u64>,
    long_break_timeout: Option<u64>,
    short_break_duration: Option<u64>,
    long_break_duration: Option<u64>,
}

const LEGACY_KEYS: &[&str] = &[
    "idle_timeout",
    "short_break_timeout",
    "long_break_timeout",
    "short_break_duration",
    "long_break_duration",
];

/// Check if a TOML string contains legacy config keys and migrate them.
fn migrate_legacy_config(content: &str) -> Option<Config> {
    let table: toml::Table = toml::from_str(content).ok()?;
    let timer_table = table.get("timer")?.as_table()?;

    let has_legacy = LEGACY_KEYS.iter().any(|k| timer_table.contains_key(*k));
    let has_new = timer_table.contains_key("base_interval") || timer_table.contains_key("breaks");

    if !has_legacy || has_new {
        return None;
    }

    warn!("Detected legacy config format — migrating automatically");
    warn!("Please update your config file to the new format.");
    warn!("See: https://github.com/zefr0x/ianny#config");

    let legacy: LegacyTimer =
        toml::from_str(&toml::to_string(timer_table).unwrap_or_default()).ok()?;

    let short_timeout = legacy.short_break_timeout.unwrap_or(1200_u64);
    let long_timeout = legacy.long_break_timeout.unwrap_or(3840_u64);
    let short_duration = legacy.short_break_duration.unwrap_or(120_u64);
    let long_duration = legacy.long_break_duration.unwrap_or(240_u64);
    let idle_timeout = legacy.idle_timeout.unwrap_or(240_u32);

    // Calculate how many short intervals fit in a long interval
    let every_long = if short_timeout > 0 {
        (long_timeout / short_timeout).max(1)
    } else {
        3_u64
    };

    let notification: Notification = table
        .get("notification")
        .and_then(|n| toml::from_str(&toml::to_string(n).unwrap_or_default()).ok())
        .unwrap_or_default();

    Some(Config {
        notification,
        timer: Timer {
            ignore_idle_inhibitors: legacy.ignore_idle_inhibitors.unwrap_or(false),
            idle_detection_threshold: idle_timeout.min(30_u32),
            base_interval: short_timeout,
            idle_inhibitor_multiplier: default_idle_inhibitor_multiplier(),
            breaks: vec![
                BreakTier {
                    every: 1_u64,
                    nudge_duration: short_duration.min(60_u64),
                    idle_threshold: u64::from(idle_timeout).min(short_duration),
                    max_skips: 2_u32,
                    messages: vec![],
                },
                BreakTier {
                    every: every_long,
                    nudge_duration: long_duration.min(60_u64),
                    idle_threshold: u64::from(idle_timeout).max(long_duration),
                    max_skips: 1_u32,
                    messages: vec![],
                },
            ],
        },
        sleep: None,
    })
}

impl Config {
    pub fn load() -> Self {
        let config_file = Self::get_config_file();

        let content = std::fs::read_to_string(&config_file).unwrap_or_else(|_| String::new());

        if !content.is_empty() {
            info!("Read config from: {}", &config_file.to_string_lossy());
        }

        if let Some(config) = migrate_legacy_config(&content) {
            return config;
        }

        toml::from_str(&content).expect("Failed to parse config file")
    }

    fn get_config_file() -> std::path::PathBuf {
        xdg::BaseDirectories::with_prefix(crate::APP_ID)
            .get_config_file("config.toml")
            .expect("Can't find XDG base config directory")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_legacy_config_migration() {
        let legacy_toml = r#"
[timer]
ignore_idle_inhibitors = true
idle_timeout = 240
short_break_timeout = 1200
long_break_timeout = 3600
short_break_duration = 120
long_break_duration = 240

[notification]
show_progress_bar = false
minimum_update_delay = 1
"#;

        let config = migrate_legacy_config(legacy_toml).expect("Should detect legacy config");

        assert!(config.timer.ignore_idle_inhibitors);
        assert_eq!(config.timer.base_interval, 1200);
        assert_eq!(config.timer.breaks.len(), 2);
        assert_eq!(config.timer.breaks[0].every, 1);
        assert_eq!(config.timer.breaks[1].every, 3); // 3600/1200
    }

    #[test]
    fn test_new_config_not_migrated() {
        let new_toml = r#"
[timer]
ignore_idle_inhibitors = true
idle_detection_threshold = 10
base_interval = 1200

[[timer.breaks]]
every = 1
nudge_duration = 60
idle_threshold = 120
"#;

        assert!(migrate_legacy_config(new_toml).is_none());
    }

    #[test]
    fn test_empty_config_not_migrated() {
        assert!(migrate_legacy_config("").is_none());
    }

    #[test]
    fn test_break_tier_message_clamping() {
        let tier = BreakTier {
            every: 1_u64,
            nudge_duration: 60_u64,
            idle_threshold: 120_u64,
            max_skips: 2_u32,
            messages: vec![
                BreakMessage {
                    summary: "First".to_owned(),
                    body: "First body".to_owned(),
                },
                BreakMessage {
                    summary: "Second".to_owned(),
                    body: "Second body".to_owned(),
                },
            ],
        };

        assert_eq!(tier.message(0).0, "First");
        assert_eq!(tier.message(1).0, "Second");
        assert_eq!(tier.message(5).0, "Second"); // clamped
    }

    #[test]
    fn test_break_tier_empty_messages() {
        let tier = BreakTier {
            every: 1_u64,
            nudge_duration: 60_u64,
            idle_threshold: 120_u64,
            max_skips: 2_u32,
            messages: vec![],
        };

        assert_eq!(tier.message(0), ("Break Time!", "Take a break"));
    }
}
