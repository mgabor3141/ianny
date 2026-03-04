mod config;
mod sleep;
mod timer;
mod wayland;

use core::time::Duration;
use std::{
    env,
    sync::{LazyLock, mpsc},
    time::Instant,
};

use log::{error, info};
use notify_rust::{Hint, Notification, Timeout, Urgency};
use single_instance::SingleInstance;

use sleep::{SleepAction, SleepEngine};
use timer::{Action, TimerEngine};

const APP_ID: &str = "io.github.zefr0x.ianny";

static CONFIG: LazyLock<config::Config> = LazyLock::new(|| {
    let config = config::Config::load();
    info!("{config:?}");
    config
});

/// Get current wall clock time as seconds since midnight (local time).
fn now_secs_since_midnight() -> u64 {
    let now = time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let (h, m, s) = now.time().as_hms();
    u64::from(h) * 3600 + u64::from(m) * 60 + u64::from(s)
}

/// Check if current time is past the configured sleep `start_time`.
fn is_sleep_time() -> bool {
    let Some(ref sleep) = CONFIG.sleep else {
        return false;
    };
    let now = now_secs_since_midnight();
    let start = sleep.start_time_secs();
    // Active if past start_time OR before 6 AM (still up from last night)
    now >= start || now < 6 * 3600
}

/// Replace break notification text with sleep-relevant messages.
fn override_for_sleep(action: Action) -> Action {
    match action {
        Action::ShowNudge {
            tier_index,
            nudge_duration,
            ..
        } => Action::ShowNudge {
            tier_index,
            summary: "You should be sleeping 😴".to_owned(),
            body: "It's past bedtime — go to bed instead of taking a break".to_owned(),
            nudge_duration,
        },
        Action::ShowPassiveBreak {
            tier_index,
            duration,
            ..
        } => Action::ShowPassiveBreak {
            tier_index,
            summary: "You should be sleeping 😴".to_owned(),
            body: "It's past bedtime — go to bed instead of taking a break".to_owned(),
            duration,
        },
        other => other,
    }
}

fn urgency_hint() -> Hint {
    Hint::Urgency(match CONFIG.notification.urgency {
        config::Urgency::Low => Urgency::Low,
        config::Urgency::Normal => Urgency::Normal,
        config::Urgency::Critical => Urgency::Critical,
    })
}

/// Show a notification with the given timeout. Blocks until duration elapses
/// or `should_stop` returns true (if `break_on_stop` is set).
/// Returns (elapsed, `stopped_early`).
fn show_timed_notification(
    summary: &str,
    body: &str,
    duration: Duration,
    should_stop: &dyn Fn() -> bool,
    break_on_stop: bool,
) -> (Duration, bool) {
    #[expect(clippy::cast_possible_truncation, reason = "Duration fits in u32 ms")]
    let timeout_ms = duration.as_millis() as u32;

    let handle = Notification::new()
        .summary(summary)
        .body(body)
        .appname("Ianny")
        .hint(Hint::SoundName("suspend-error".to_owned()))
        .hint(urgency_hint())
        .timeout(Timeout::Milliseconds(timeout_ms))
        .show()
        .expect("Failed to send notification");

    let start = Instant::now();
    let mut stopped = false;

    while Instant::now().duration_since(start) < duration {
        std::thread::sleep(Duration::from_secs(1));
        if should_stop() {
            stopped = true;
            if break_on_stop {
                break;
            }
        }
    }

    handle.close();
    (Instant::now().duration_since(start), stopped)
}

/// Show a brief non-blocking feedback notification.
fn show_feedback(summary: &str, body: &str) {
    let _handle: Result<_, _> = Notification::new()
        .summary(summary)
        .body(body)
        .appname("Ianny")
        .hint(urgency_hint())
        .timeout(Timeout::Milliseconds(3000_u32))
        .show();
}

/// Show a passive break notification with a "Skip" action.
/// Returns true if user clicked Skip, false if it expired (break taken).
fn show_passive_break(summary: &str, body: &str, duration: Duration) -> bool {
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[expect(clippy::cast_possible_truncation, reason = "Duration fits in u32 ms")]
    let timeout_ms = duration.as_millis() as u32;

    let handle = Notification::new()
        .summary(summary)
        .body(body)
        .appname("Ianny")
        .hint(Hint::SoundName("suspend-error".to_owned()))
        .hint(urgency_hint())
        .action("skip", "Skip break")
        .timeout(Timeout::Milliseconds(timeout_ms))
        .show()
        .expect("Failed to send notification");

    let skipped = Arc::new(AtomicBool::new(false));
    let skipped_clone = Arc::<AtomicBool>::clone(&skipped);

    // wait_for_action blocks until action or close/timeout.
    // The closure receives the action key as &str.
    handle.wait_for_action(move |action: &str| {
        if action == "skip" {
            skipped_clone.store(true, Ordering::Relaxed);
        }
    });

    skipped.load(Ordering::Relaxed)
}

/// Execute an action from the timer engine.
fn execute_action(
    action: Action,
    engine: &mut TimerEngine,
    signal_receiver: &mpsc::Receiver<wayland::Signal>,
) {
    match action {
        Action::ShowNudge {
            tier_index,
            summary,
            body,
            nudge_duration,
        } => {
            info!("Nudge: tier={tier_index}, summary={summary}");

            // Check for idle during nudge
            let idle_detected = core::sync::atomic::AtomicBool::new(false);
            let check_idle = || {
                if signal_receiver.try_recv() == Ok(wayland::Signal::Idled) {
                    idle_detected.store(true, core::sync::atomic::Ordering::Relaxed);
                }
                idle_detected.load(core::sync::atomic::Ordering::Relaxed)
            };

            let (_, went_idle) = show_timed_notification(
                &summary,
                &body,
                Duration::from_secs(nudge_duration),
                &check_idle,
                true, // close early if idle detected
            );

            // Drain signals
            while signal_receiver.try_recv().is_ok() {}

            let follow_up = engine.nudge_result(went_idle, &CONFIG);
            execute_action(follow_up, engine, signal_receiver);
        }
        Action::ShowPassiveBreak {
            tier_index,
            summary,
            body,
            duration,
        } => {
            info!("Passive break: tier={tier_index}, duration={duration}s (inhibitors active)");

            let skipped = show_passive_break(
                &summary,
                &body,
                Duration::from_secs(duration),
            );

            if skipped {
                engine.passive_break_skipped(tier_index);
            } else {
                engine.passive_break_completed(tier_index);
            }
        }
        Action::ShowBreakCountdown {
            tier_index,
            summary,
            body,
            duration,
        } => {
            info!("Break countdown: tier={tier_index}, duration={duration}s");

            // Check for user resuming activity (break interruption)
            let resumed_detected = core::sync::atomic::AtomicBool::new(false);
            let check_resumed = || {
                if signal_receiver.try_recv() == Ok(wayland::Signal::Resumed) {
                    resumed_detected.store(true, core::sync::atomic::Ordering::Relaxed);
                }
                resumed_detected.load(core::sync::atomic::Ordering::Relaxed)
            };

            let (_, was_interrupted) = show_timed_notification(
                &summary,
                &body,
                Duration::from_secs(duration),
                &check_resumed,
                true, // close early if user resumes
            );

            // Drain signals
            while signal_receiver.try_recv().is_ok() {}

            if was_interrupted {
                let feedback = engine.break_interrupted(tier_index);
                execute_action(feedback, engine, signal_receiver);
            } else {
                engine.break_completed(tier_index);
            }
        }
        Action::ShowFeedback { summary, body } => {
            info!("Feedback: {summary}");
            show_feedback(&summary, &body);
        }
        Action::None => {}
    }
}

#[expect(clippy::too_many_lines, reason = "main loop with Wayland event handling")]
fn main() -> ! {
    simple_logger::SimpleLogger::new().init().unwrap();

    let app_instance = SingleInstance::new(APP_ID).unwrap();
    if !app_instance.is_single() {
        error!("{APP_ID} is already running.");
        std::process::exit(1);
    }

    let app_lang = gettextrs::setlocale(
        gettextrs::LocaleCategory::LcAll,
        env::var("LC_ALL").unwrap_or_else(|_| {
            env::var("LC_CTYPE").unwrap_or_else(|_| env::var("LANG").unwrap_or_default())
        }),
    )
    .expect("Failed to set locale, please use a valid system locale and make sure it's enabled");
    gettextrs::textdomain(APP_ID).unwrap();
    gettextrs::bindtextdomain(APP_ID, "/usr/share/locale").unwrap();
    gettextrs::bind_textdomain_codeset(APP_ID, "UTF-8").unwrap();

    info!("Application locale: {}", String::from_utf8_lossy(&app_lang));

    if CONFIG.timer.breaks.is_empty() {
        error!("No break tiers configured");
        std::process::exit(1);
    }

    let (signal_sender, signal_receiver) = mpsc::sync_channel(8);

    // Timer thread
    std::thread::spawn(move || -> ! {
        let tick = core::cmp::min(
            CONFIG.timer.base_interval,
            u64::from(CONFIG.timer.idle_detection_threshold) + 1,
        );

        let mut engine = TimerEngine::new();
        let mut sleep_engine = SleepEngine::new();
        let mut last_time = Instant::now();
        // Track inhibitor state: true when input is idle but compositor
        // idle hasn't fired (meaning an inhibitor is preventing it).
        let mut input_idle = false;
        let mut inhibitor_idle = false;

        loop {
            std::thread::sleep(Duration::from_secs(tick));

            let last_time_copy = last_time;
            last_time = Instant::now();
            let time_diff = Instant::now().duration_since(last_time_copy).as_secs();

            // Detect system suspend
            let max_idle = CONFIG
                .timer
                .breaks
                .iter()
                .map(|b| b.idle_threshold)
                .max()
                .unwrap_or(120);

            if time_diff - tick >= max_idle {
                engine.suspend_detected();
                input_idle = false;
                inhibitor_idle = false;
                last_time = Instant::now();
                continue;
            }

            // Drain all pending signals and update state
            let mut got_input_idled = false;
            let mut got_input_resumed = false;
            loop {
                match signal_receiver.try_recv() {
                    Ok(wayland::Signal::Idled) => {
                        got_input_idled = true;
                        input_idle = true;
                    }
                    Ok(wayland::Signal::Resumed) => {
                        got_input_resumed = true;
                        input_idle = false;
                    }
                    Ok(wayland::Signal::InhibitorIdled) => {
                        inhibitor_idle = true;
                    }
                    Ok(wayland::Signal::InhibitorResumed) => {
                        inhibitor_idle = false;
                    }
                    Err(_) => break,
                }
            }

            // Determine if inhibitors are active:
            // input is idle but compositor idle hasn't fired
            let inhibitors_active = input_idle && !inhibitor_idle;

            // Handle input idle→resumed cycle
            if got_input_idled && !got_input_resumed {
                // Entered idle — wait for resume
                let idle_start = Instant::now();
                info!("Idle detected via Wayland (inhibitors_active={inhibitors_active})");

                loop {
                    match signal_receiver.recv() {
                        Ok(wayland::Signal::Resumed) => {
                            input_idle = false;
                            break;
                        }
                        Ok(wayland::Signal::InhibitorIdled) => {
                            inhibitor_idle = true;
                        }
                        Ok(wayland::Signal::InhibitorResumed) => {
                            inhibitor_idle = false;
                        }
                        _ => {}
                    }
                }

                // Drain remaining signals
                loop {
                    match signal_receiver.try_recv() {
                        Ok(wayland::Signal::InhibitorIdled) => inhibitor_idle = true,
                        Ok(wayland::Signal::InhibitorResumed) => inhibitor_idle = false,
                        Ok(wayland::Signal::Resumed) => input_idle = false,
                        _ => break,
                    }
                }

                let idle_secs = Instant::now().duration_since(idle_start).as_secs();
                // Were inhibitors active during this idle period?
                // If the inhibitor-respecting notification never fired during
                // our input-idle period, an inhibitor was preventing it.
                let inhibitors_were_active = !inhibitor_idle;
                info!("Resumed after {idle_secs}s idle (inhibitors_active={inhibitors_were_active})");
                engine.idle_resumed(idle_secs, &CONFIG, inhibitors_were_active);
                last_time = Instant::now();
                continue;
            }

            // If we got both idled and resumed in the same tick, handle it
            if got_input_idled && got_input_resumed {
                info!("Brief idle detected (within tick) — ignoring");
                last_time = Instant::now();
                continue;
            }

            // Tick the engine
            let action = engine.tick(time_diff, &CONFIG, inhibitors_active);
            if action != Action::None {
                let action = if is_sleep_time() {
                    override_for_sleep(action)
                } else {
                    action
                };
                execute_action(action, &mut engine, &signal_receiver);
                last_time = Instant::now();
            }

            // Check sleep reminders
            if let Some(ref sleep_config) = CONFIG.sleep {
                let now = now_secs_since_midnight();
                let sleep_action = sleep_engine.check(now, sleep_config);
                if let SleepAction::Escalate {
                    summary,
                    body,
                    command,
                    ..
                } = sleep_action
                {
                    if let Some(ref s) = summary {
                        let _handle: Result<_, _> = Notification::new()
                            .summary(s)
                            .body(&body)
                            .appname("Ianny")
                            .hint(urgency_hint())
                            .timeout(Timeout::Milliseconds(30_000))
                            .show();
                    }
                    if let Some(ref cmd) = command {
                        info!("Sleep: running command: {cmd}");
                        match std::process::Command::new("sh")
                            .arg("-c")
                            .arg(cmd)
                            .spawn()
                        {
                            Ok(_) => {}
                            Err(e) => error!("Sleep: command failed: {e}"),
                        }
                    }
                }
            }
        }
    });

    // Wayland event loop
    let conn = wayland_client::Connection::connect_to_env()
        .expect("Not able to detect a wayland compositor");

    let mut event_queue = conn.new_event_queue::<wayland::State>();
    let queue_handle = event_queue.handle();
    let display = conn.display();
    let _registry = display.get_registry(&queue_handle, ());

    let mut state = wayland::State::new(signal_sender);

    event_queue
        .roundtrip(&mut state)
        .expect("Failed to cause a synchronous round trip with the wayland server");

    loop {
        event_queue
            .blocking_dispatch(&mut state)
            .expect("Failed to block waiting for events and dispatch them");
    }
}
