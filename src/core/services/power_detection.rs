use std::{fs, sync::Arc, time::Duration};
use tokio::sync::Mutex;

use crate::core::manager::Manager;
use crate::log::log_message;

pub async fn detect_initial_power_state(manager: &Arc<Mutex<Manager>>) -> bool {
    let mgr = manager.lock().await;
    if !mgr.state.is_laptop() {
        log_message("Desktop detected, skipping power source check");
        return true;
    }
    drop(mgr);

    let on_ac = is_on_ac_power().await;

    {
        let mut mgr = manager.lock().await;
        mgr.state.set_on_battery(!on_ac);
    }

    log_message(&format!("Initial power deteciton: {}", if on_ac { "AC" } else { "Battery" }));
    on_ac
}

async fn is_on_ac_power() -> bool {
    // Scan /sys/class/power_supply
    if let Ok(entries) = fs::read_dir("/sys/class/power_supply/") {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();

            if let Ok(supply_type) = fs::read_to_string(path.join("type")) {
                if supply_type.trim() == "Mains" {
                    if let Ok(status) = fs::read_to_string(path.join("online")) {
                        if status.trim() == "1" {
                            return true;
                        }
                    }
                }
            }

            // Optional: fallback on legacy AC names
            let legacy_ac_names = ["AC", "ADP", "ACAD", "AC0", "ADP0"];
            if legacy_ac_names.iter().any(|n| name.starts_with(n)) {
                if let Ok(status) = fs::read_to_string(path.join("online")) {
                    if status.trim() == "1" {
                        return true;
                    }
                }
            }
        }
    }

    false
}

pub async fn spawn_power_source_monitor(manager: Arc<Mutex<Manager>>) {
    let on_ac = detect_initial_power_state(&manager).await;
    let mut last_on_ac = on_ac;

    let mut ticker = tokio::time::interval(Duration::from_secs(5));
    loop {
        ticker.tick().await;

        let mgr = manager.lock().await;
        if !mgr.state.is_laptop() {
            continue;
        }
        drop(mgr); // release lock

        let on_ac = is_on_ac_power().await;
        if on_ac != last_on_ac {
            last_on_ac = on_ac;
            log_message(&format!("Power source changed: {}", if on_ac { "AC" } else { "Battery" }));

            let mut mgr = manager.lock().await;
            mgr.state.set_on_battery(!on_ac);
            mgr.reset_instant_actions();
            mgr.trigger_instant_actions().await;
        }
    }
}
