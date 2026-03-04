use std::sync::mpsc;

use log::{error, info, trace};
use wayland_client::{
    Proxy,
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};

use crate::CONFIG;

#[derive(Debug, Eq, PartialEq)]
pub enum Signal {
    /// User input stopped (ignoring idle inhibitors)
    Idled,
    /// User input resumed
    Resumed,
    /// Compositor idle (respects inhibitors) — fires only when no inhibitors active
    InhibitorIdled,
    /// Compositor idle resumed (respects inhibitors)
    InhibitorResumed,
}

/// Tag to distinguish which idle notification sent an event.
#[derive(Debug, Clone, Copy)]
enum IdleNotificationKind {
    /// Input-only (ignores inhibitors)
    Input,
    /// Standard (respects inhibitors)
    Inhibitor,
}

type GlobalName = u32;

pub struct State {
    idle_notifier: Option<(GlobalName, ext_idle_notifier_v1::ExtIdleNotifierV1)>,
    /// Input-only idle notification (ignores inhibitors)
    input_idle_notification: Option<ext_idle_notification_v1::ExtIdleNotificationV1>,
    /// Standard idle notification (respects inhibitors)
    inhibitor_idle_notification: Option<ext_idle_notification_v1::ExtIdleNotificationV1>,
    signal_sender: mpsc::SyncSender<Signal>,
}

impl State {
    pub const fn new(signal_sender: mpsc::SyncSender<Signal>) -> Self {
        Self {
            idle_notifier: None,
            input_idle_notification: None,
            inhibitor_idle_notification: None,
            signal_sender,
        }
    }
}

impl wayland_client::Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &wayland_client::Connection,
        queue_handle: &wayland_client::QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                match interface.as_str() {
                    "wl_seat" => {
                        // TODO: Support newest version of wl_seat.
                        let wl_seat =
                            registry.bind::<wl_seat::WlSeat, _, _>(name, 1, queue_handle, ());

                        trace!("Binded to {}", wl_seat.id());
                    }
                    "ext_idle_notifier_v1" => {
                        let idle_notifier = registry
                            .bind::<ext_idle_notifier_v1::ExtIdleNotifierV1, _, _>(
                                name,
                                version,
                                queue_handle,
                                (),
                            );

                        trace!("Binded to {}", idle_notifier.id());

                        state.idle_notifier = Some((name, idle_notifier));
                    }
                    _ => {}
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some((idle_notifier_name, idle_notifier)) = &state.idle_notifier
                    && name == *idle_notifier_name {
                        idle_notifier.destroy();
                        state.idle_notifier = None;

                        trace!("Destroyed ext_idle_notifier_v1");

                        if let Some(n) = &state.input_idle_notification {
                            n.destroy();
                            state.input_idle_notification = None;
                            trace!("Destroyed input idle notification");
                        }
                        if let Some(n) = &state.inhibitor_idle_notification {
                            n.destroy();
                            state.inhibitor_idle_notification = None;
                            trace!("Destroyed inhibitor idle notification");
                        }
                    }
            }
            _ => {}
        }
    }
}

impl wayland_client::Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _conn: &wayland_client::Connection,
        queue_handle: &wayland_client::QueueHandle<Self>,
    ) {
        // FIX: Support multiseat configuration.
        if let Some((_, idle_notifier)) = &state.idle_notifier {
            let idle_timeout = CONFIG.timer.idle_detection_threshold * 1000; // milliseconds

            // Destroy existing notifications
            if let Some(n) = &state.input_idle_notification {
                n.destroy();
                state.input_idle_notification = None;
                trace!("Destroyed input idle notification");
            }
            if let Some(n) = &state.inhibitor_idle_notification {
                n.destroy();
                state.inhibitor_idle_notification = None;
                trace!("Destroyed inhibitor idle notification");
            }

            // Create input-only idle notification (ignores inhibitors)
            let supports_input = idle_notifier.version()
                >= ext_idle_notifier_v1::REQ_GET_INPUT_IDLE_NOTIFICATION_SINCE;

            if CONFIG.timer.ignore_idle_inhibitors && supports_input {
                let input_notification = idle_notifier.get_input_idle_notification(
                    idle_timeout,
                    seat,
                    queue_handle,
                    IdleNotificationKind::Input,
                );
                trace!("Created input idle notification: {}", input_notification.id());
                state.input_idle_notification = Some(input_notification);

                // Also create inhibitor-respecting notification for detecting inhibitor state
                let inhibitor_notification = idle_notifier.get_idle_notification(
                    idle_timeout,
                    seat,
                    queue_handle,
                    IdleNotificationKind::Inhibitor,
                );
                trace!("Created inhibitor idle notification: {}", inhibitor_notification.id());
                state.inhibitor_idle_notification = Some(inhibitor_notification);
            } else {
                if CONFIG.timer.ignore_idle_inhibitors {
                    error!(
                        "Failed to ignore idle inhibitors, your wayland compositor's idle notifier does not support this feature."
                    );
                }

                // Fall back to standard notification only
                let notification = idle_notifier.get_idle_notification(
                    idle_timeout,
                    seat,
                    queue_handle,
                    IdleNotificationKind::Input,
                );
                trace!("Created standard idle notification: {}", notification.id());
                state.input_idle_notification = Some(notification);
            }
        }
    }
}

impl wayland_client::Dispatch<ext_idle_notifier_v1::ExtIdleNotifierV1, ()> for State {
    fn event(
        _state: &mut Self,
        _idle_notifier: &ext_idle_notifier_v1::ExtIdleNotifierV1,
        _event: ext_idle_notifier_v1::Event,
        &(): &(),
        _conn: &wayland_client::Connection,
        _queue_handle: &wayland_client::QueueHandle<Self>,
    ) {
        // No events
    }
}

impl wayland_client::Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, IdleNotificationKind>
    for State
{
    fn event(
        state: &mut Self,
        _idle_notification: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        data: &IdleNotificationKind,
        _conn: &wayland_client::Connection,
        _queue_handle: &wayland_client::QueueHandle<Self>,
    ) {
        match (data, &event) {
            (IdleNotificationKind::Input, ext_idle_notification_v1::Event::Idled) => {
                info!("Idled (input)");
                match state.signal_sender.try_send(Signal::Idled) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => (),
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        panic!("Timer disconnected, `Idled` signal could not be sent")
                    }
                }
            }
            (IdleNotificationKind::Input, ext_idle_notification_v1::Event::Resumed) => {
                info!("Resumed (input)");
                match state.signal_sender.try_send(Signal::Resumed) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => (),
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        panic!("Timer disconnected, `Resumed` signal could not be sent")
                    }
                }
            }
            (IdleNotificationKind::Inhibitor, ext_idle_notification_v1::Event::Idled) => {
                info!("Idled (inhibitor-aware)");
                match state.signal_sender.try_send(Signal::InhibitorIdled) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => (),
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        panic!("Timer disconnected, `InhibitorIdled` signal could not be sent")
                    }
                }
            }
            (IdleNotificationKind::Inhibitor, ext_idle_notification_v1::Event::Resumed) => {
                info!("Resumed (inhibitor-aware)");
                match state.signal_sender.try_send(Signal::InhibitorResumed) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => (),
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        panic!("Timer disconnected, `InhibitorResumed` signal could not be sent")
                    }
                }
            }
            _ => {}
        }
    }
}
