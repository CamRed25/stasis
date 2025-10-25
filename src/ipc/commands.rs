use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

use crate::{
    core::manager::{helpers::run_action, Manager},
    log::log_message,
};

pub async fn trigger_action_by_name(manager: Arc<Mutex<Manager>>, name: &str) -> Result<String, String> {
    let normalized = name.replace('_', "-").to_lowercase();
    let mut mgr = manager.lock().await;

    if normalized == "pre-suspend" || normalized == "presuspend" {
        mgr.trigger_pre_suspend(true).await;
        return Ok("pre_suspend".to_string());
    }

    let block = if !mgr.state.ac_actions.is_empty() || !mgr.state.battery_actions.is_empty() {
        match mgr.state.on_battery() {
            Some(true) => &mgr.state.battery_actions,
            Some(false) => &mgr.state.ac_actions,
            None => &mgr.state.default_actions,
        }
    } else {
        &mgr.state.default_actions
    };

    let action_opt = block.iter().find(|a| {
        let kind_name = format!("{:?}", a.kind).to_lowercase().replace('_', "-");
        kind_name == normalized || a.name.to_lowercase() == normalized
    });

    let action = match action_opt {
        Some(a) => a.clone(),
        None => {
            let mut available: Vec<String> = block.iter().map(|a| a.name.clone()).collect();
            if mgr.state.pre_suspend_command.is_some() {
                available.push("pre_suspend".to_string());
            }
            available.sort();
            return Err(format!(
                "Action '{}' not found. Available actions: {}",
                name,
                available.join(", ")
            ));
        }
    };

    log_message(&format!("Action triggered: '{}'", action.name));
    let is_lock = matches!(action.kind, crate::config::model::IdleAction::LockScreen);

    if is_lock {
        // Mark lock state and notify watcher
        mgr.state.lock_state.is_locked = true;
        mgr.state.lock_state.post_advanced = false;
        mgr.state.lock_state.command = Some(action.command.clone());
        mgr.state.lock_notify.notify_one();

        // Run the lock command
        run_action(&mut mgr, &action).await;

        // Mark as advanced past lock
        mgr.advance_past_lock().await;

        // ---- Mirror reset() behavior exactly for timers ----
        let now = Instant::now();
        if let Some(cfg) = &mgr.state.cfg {
            let debounce = Duration::from_secs(cfg.debounce_seconds as u64);
            mgr.state.last_activity_display = now;
            mgr.state.debounce = Some(now + debounce);

            // Clear last_triggered for all actions
            {
                let actions = &mut mgr.state.default_actions;
                for a in actions.iter_mut() {
                    a.last_triggered = None;
                }
            }
            {
                let actions = &mut mgr.state.ac_actions;
                for a in actions.iter_mut() {
                    a.last_triggered = None;
                }
            }
            {
                let actions = &mut mgr.state.battery_actions;
                for a in actions.iter_mut() {
                    a.last_triggered = None;
                }
            }

            // Determine active block name first
            let active_block = if !mgr.state.ac_actions.is_empty() || !mgr.state.battery_actions.is_empty() {
                match mgr.state.on_battery() {
                    Some(true) => "battery",
                    Some(false) => "ac",
                    None => "default",
                }
            } else {
                "default"
            };

            // Now isolate block mutation
            {
                let actions = match active_block {
                    "ac" => &mut mgr.state.ac_actions,
                    "battery" => &mut mgr.state.battery_actions,
                    _ => &mut mgr.state.default_actions,
                };

                // Recalculate action index
                let mut next_index = actions
                    .iter()
                    .position(|a| a.last_triggered.is_none())
                    .unwrap_or_else(|| actions.len().saturating_sub(1));

                // If lock action exists, skip past it so next timer continues properly
                if let Some(lock_index) =
                    actions.iter().position(|a| matches!(a.kind, crate::config::model::IdleAction::LockScreen))
                {
                    if next_index <= lock_index {
                        next_index = lock_index.saturating_add(1);

                        let debounce_end = now + debounce;
                        if next_index < actions.len() {
                            actions[next_index].last_triggered = Some(debounce_end);
                        }

                        mgr.state.lock_state.post_advanced = true;
                    }
                }

                mgr.state.action_index = next_index;
            }
        }

        // Wake idle loop to recalculate timers
        mgr.state.notify.notify_one();
    } else {
        run_action(&mut mgr, &action).await;
    }

    Ok(action.name)
}

pub async fn list_available_actions(manager: Arc<Mutex<Manager>>) -> Vec<String> {
    let mgr = manager.lock().await;
    let mut actions = mgr
        .state
        .default_actions
        .iter()
        .map(|a| a.name.clone())
        .collect::<Vec<_>>();

    if mgr.state.pre_suspend_command.is_some() {
        actions.push("pre_suspend".to_string());
    }

    actions.sort();
    actions
}
