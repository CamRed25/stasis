use std::sync::Arc;
use futures::StreamExt;
use tokio::sync::Mutex;
use zbus::{Connection, fdo::Result as ZbusResult, Proxy};
use crate::core::timer::IdleTimer;
use crate::core::actions::run_command_silent;
use crate::log::log_message;

/// Listens for system suspend and resume events via logind D-Bus signals.
pub async fn listen_for_suspend_events(idle_timer: Arc<Mutex<IdleTimer>>) -> ZbusResult<()> {
    // Connect to the system bus
    let connection = Connection::system().await?;

    // Create proxy to org.freedesktop.login1.Manager
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.login1",        // destination
        "/org/freedesktop/login1",       // path
        "org.freedesktop.login1.Manager" // interface
    ).await?;

    // Subscribe to PrepareForSleep signals
    let mut stream = proxy.receive_signal("PrepareForSleep").await?;

    log_message("Listening for D-Bus suspend events...");

    while let Some(signal) = stream.next().await {
        let going_to_sleep: bool = match signal.body().deserialize() {
            Ok(val) => val,
            Err(e) => {
                log_message(&format!("Failed to parse D-Bus suspend signal: {e:?}"));
                continue;
            }
        };

        let mut timer = idle_timer.lock().await;

        if going_to_sleep {
            log_message("System is preparing to suspend...");
            timer.trigger_pre_suspend(false, true).await;
        } else {
            log_message("System resumed from sleep");

            // If a lockscreen is running, advance timers past it
            if timer.is_lock_running().await {
                log_message("Lock detected on resume — advancing timers past lock");
                timer.advance_past_lock().await;
            } else {
                log_message("No lock detected on resume — executing resume actions");

                // Fire resume commands for all previously triggered actions
                for opt_action in timer.triggered_actions.iter() {
                    if let Some(action) = opt_action {
                        if let Some(resume_cmd) = &action.resume_command {
                            let cmd_clone = resume_cmd.clone();
                            tokio::spawn(async move {
                                let _ = run_command_silent(&cmd_clone).await;
                            });
                        }
                    }
                }
            }

            // After handling resume, reset idle flags and active kinds
            timer.is_idle_flags.fill(false);
            timer.active_kinds.clear();
            timer.triggered_actions.iter_mut().for_each(|a| *a = None);
            timer.last_activity = std::time::Instant::now();
        }
    }

    Ok(())
}
