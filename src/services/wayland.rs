use eyre::Result;
use std::sync::Arc;
use std::time::Duration;

use crate::core::timer::IdleTimer;
use crate::log::{log_error_message, log_message};

use tokio::sync::Notify;
use tokio::time::sleep;

use wayland_client::{
    protocol::{wl_registry, wl_seat::WlSeat},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notifier_v1::ExtIdleNotifierV1,
    ext_idle_notification_v1::{ExtIdleNotificationV1, Event as IdleEvent},
};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
    zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};

/// Wayland integration for Stasis idle management
pub struct WaylandIdleData {
    pub idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>,
    pub idle_notifier: Option<ExtIdleNotifierV1>,
    pub seat: Option<WlSeat>,
    pub notification: Option<ExtIdleNotificationV1>,
    pub inhibit_manager: Option<ZwpIdleInhibitManagerV1>,
    pub respect_inhibitors: bool,
    pub shutdown: Arc<Notify>,
}

impl WaylandIdleData {
    pub fn new(idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>, respect_inhibitors: bool) -> Self {
        Self {
            idle_timer,
            idle_notifier: None,
            seat: None,
            notification: None,
            inhibit_manager: None,
            respect_inhibitors,
            shutdown: Arc::new(Notify::new()),
        }
    }
}

/// Bind registry globals
impl Dispatch<wl_registry::WlRegistry, ()> for WaylandIdleData {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "ext_idle_notifier_v1" => {
                    state.idle_notifier =
                        Some(registry.bind::<ExtIdleNotifierV1, _, _>(name, 1, qh, ()));
                    log_message("Bound ext_idle_notifier_v1");
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind::<WlSeat, _, _>(name, 1, qh, ()));
                    log_message("Bound wl_seat");
                }
                "zwp_idle_inhibit_manager_v1" => {
                    state.inhibit_manager =
                        Some(registry.bind::<ZwpIdleInhibitManagerV1, _, _>(name, 1, qh, ()));
                    log_message("Bound zwp_idle_inhibit_manager_v1");
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for WaylandIdleData {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Handle compositor idle events (trust compositor when notifier is active)
impl Dispatch<ExtIdleNotificationV1, ()> for WaylandIdleData {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: IdleEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let idle_timer = Arc::clone(&state.idle_timer);

        tokio::spawn(async move {
            let mut timer = idle_timer.lock().await;

            if !timer.is_compositor_managed() {
                // Ignore if compositor-managed mode isn't active
                return;
            }

            match event {
                IdleEvent::Idled => {
                    log_message("Compositor reported idle state");
                    timer.mark_all_idle();
                    timer.trigger_idle().await;
                }
                IdleEvent::Resumed => {
                    log_message("Compositor reported activity");
                    timer.reset();
                }
                _ => {}
            }
        });
    }
}

/// The idle-inhibit protocol is per-client; we can only manage our own inhibitors.
/// These handlers are retained as no-ops for completeness.
impl Dispatch<ZwpIdleInhibitorV1, ()> for WaylandIdleData {
    fn event(
        _: &mut Self,
        _: &ZwpIdleInhibitorV1,
        _: <ZwpIdleInhibitorV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // No-op: We cannot observe inhibitors from other clients.
    }
}

impl Dispatch<ZwpIdleInhibitManagerV1, ()> for WaylandIdleData {
    fn event(
        _: &mut Self,
        _: &ZwpIdleInhibitManagerV1,
        _: <ZwpIdleInhibitManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // No-op: Manager does not broadcast inhibitor creation/removal.
    }
}

impl Dispatch<WlSeat, ()> for WaylandIdleData {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// Initialize Wayland idle detection
pub async fn setup(
    idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>,
    respect_inhibitors: bool,
) -> Result<Arc<tokio::sync::Mutex<WaylandIdleData>>> {
    log_message(&format!(
        "Initializing Wayland idle detection (respect_inhibitors={})",
        respect_inhibitors
    ));

    let conn = Connection::connect_to_env()
        .map_err(|e| eyre::eyre!("Failed to connect to Wayland: {}", e))?;
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let display = conn.display();

    let mut app_data = WaylandIdleData::new(idle_timer.clone(), respect_inhibitors);
    let _registry = display.get_registry(&qh, ());
    event_queue.roundtrip(&mut app_data)?;

    if let (Some(notifier), Some(seat)) = (&app_data.idle_notifier, &app_data.seat) {
        let timeout_ms = {
            let timer = idle_timer.lock().await;
            timer.shortest_timeout().as_millis() as u32
        };
        let notification = notifier.get_idle_notification(timeout_ms, seat, &qh, ());
        app_data.notification = Some(notification);

        let mut timer = idle_timer.lock().await;
        timer.set_compositor_managed(true);
        log_message("Wayland compositor-managed idle detection active");
    } else {
        log_message("Compositor does not support ext_idle_notifier_v1 — using timer fallback");
    }

    let app_data = Arc::new(tokio::sync::Mutex::new(app_data));
    let shutdown = { Arc::clone(&app_data.lock().await.shutdown) };

    // Spawn Wayland event loop
    tokio::spawn({
        let app_data = Arc::clone(&app_data);
        async move {
            loop {
                {
                    let mut locked_data = app_data.lock().await;
                    if let Err(e) = event_queue.dispatch_pending(&mut *locked_data) {
                        log_error_message(&format!("Wayland event error: {}", e));
                    }
                }

                tokio::select! {
                    _ = shutdown.notified() => {
                        log_message("Wayland event loop shutting down");
                        break;
                    }
                    _ = sleep(Duration::from_millis(50)) => {}
                }
            }
        }
    });

    Ok(app_data)
}
