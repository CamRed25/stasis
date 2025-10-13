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
            
            // Execute resume commands for all triggered actions that have one
            let resume_commands: Vec<String> = timer
                .triggered_actions
                .iter()
                .filter_map(|opt_action| {
                    opt_action.as_ref().and_then(|action| action.resume_command.clone())
                })
                .collect();
            
            // Spawn tasks for each resume command
            for cmd in resume_commands {
                tokio::spawn(async move {
                    let _ = run_command_silent(&cmd).await;
                });
            }
        }
    }
    
    Ok(())
}
