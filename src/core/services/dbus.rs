use std::{collections::HashMap, sync::Arc};
use futures::StreamExt;
use tokio::sync::Mutex;
use zbus::{Connection, fdo::Result as ZbusResult, Proxy};
use zvariant::OwnedValue;

use crate::core::events::handlers::{handle_event, Event};
use crate::core::manager::Manager;
use crate::log::log_message;

pub async fn listen_for_suspend_events(idle_manager: Arc<Mutex<Manager>>) -> ZbusResult<()> {
    let connection = Connection::system().await?;
    let proxy = Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager"
    ).await?;
    
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
        
        let manager_arc = Arc::clone(&idle_manager);
        if going_to_sleep {
            log_message("System is preparing to suspend...");
            handle_event(&manager_arc, Event::Suspend).await; 
        } else {
            log_message("System resumed from sleep");
            handle_event(&manager_arc, Event::Wake).await;
        }
    }
    
    Ok(())
}

pub async fn listen_for_lid_events(idle_manager: Arc<Mutex<Manager>>) -> ZbusResult<()> {
    let connection = Connection::system().await?;

    let seat_proxy = Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1/seat/seat0",
        "org.freedesktop.login1.Seat"
    ).await?;

    log_message("Listening for D-Bus lid events on seat0...");

    // Listen for generic PropertiesChanged signal
    let mut stream = seat_proxy.receive_signal("PropertiesChanged").await?;

    while let Some(signal) = stream.next().await {
        let (iface, changed, _): (String, HashMap<String, OwnedValue>, Vec<String>) =
            match signal.body().deserialize() {
                Ok(val) => val,
                Err(e) => {
                    log_message(&format!("Failed to parse lid signal: {e:?}"));
                    continue;
                }
            };

        if iface == "org.freedesktop.login1.Seat" {
            if let Some(val) = changed.get("LidClosed") {
                if let Ok(lid_closed) = bool::try_from(val.clone()) {
                    let manager_arc = Arc::clone(&idle_manager);
                    if lid_closed {
                        log_message("Lid closed");
                        handle_event(&manager_arc, Event::LidClosed).await;
                    } else {
                        log_message("Lid opened");
                        handle_event(&manager_arc, Event::LidOpened).await;
                    }
                }
            }
        }
    }

    Ok(())
}

// Optional: Combined listener that handles both suspend and lid events
pub async fn listen_for_power_events(idle_manager: Arc<Mutex<Manager>>) -> ZbusResult<()> {
    let suspend_manager = Arc::clone(&idle_manager);
    let lid_manager = Arc::clone(&idle_manager);
    
    // Spawn both listeners concurrently
    let suspend_handle = tokio::spawn(async move {
        if let Err(e) = listen_for_suspend_events(suspend_manager).await {
            log_message(&format!("Suspend listener error: {e:?}"));
        }
    });
    
    let lid_handle = tokio::spawn(async move {
        if let Err(e) = listen_for_lid_events(lid_manager).await {
            log_message(&format!("Lid listener error: {e:?}"));
        }
    });
    
    // Wait for both (they should run indefinitely)
    let _ = tokio::try_join!(suspend_handle, lid_handle);
    
    Ok(())
}
