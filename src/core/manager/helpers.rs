use std::fs;
use std::path::Path;
use tokio::process::Command;


use crate::log::{log_error_message, log_message};

use crate::{
    config::model::IdleActionBlock, 
    core::manager::{
        actions::{is_process_running, prepare_action, run_command_detached, run_command_silent, ActionRequest}, 
        state::ManagerState, Manager,
    }
};

// Brightness
#[derive(Clone, Debug)]
struct BrightnessState {
    value: u32,
    #[allow(dead_code)]
    device: String,
}

pub async fn capture_brightness(state: &mut ManagerState) -> Result<(), std::io::Error> {
    // Try sysfs method first
    if let Some(sys_brightness) = capture_sysfs_brightness() {
        log_message(&format!("Captured brightness via sysfs: {}", sys_brightness.value));

        // Convert safely to u8
        state.previous_brightness = Some(sys_brightness.value.min(u8::MAX as u32) as u8);
        return Ok(());
    }

    // Fallback to brightnessctl
    log_message("Falling back to brightnessctl for brightness capture");
    match Command::new("brightnessctl").arg("get").output().await {
        Ok(out) if out.status.success() => {
            let val = String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<u32>()
                .unwrap_or(0);
            state.previous_brightness = Some(val.min(u8::MAX as u32) as u8);
            log_message(&format!("Captured brightness via brightnessctl: {}", val));
        }
        Ok(out) => {
            log_error_message(&format!("brightnessctl get failed: {:?}", out.status));
        }
        Err(e) => {
            log_error_message(&format!("Failed to execute brightnessctl: {}", e));
        }
    }

    Ok(())
}
pub async fn restore_brightness(state: &mut ManagerState) -> Result<(), std::io::Error> {
    if let Some(level) = state.previous_brightness {
        log_message(&format!("Attempting to restore brightness to {}", level));

        // Try sysfs restore first
        if restore_sysfs_brightness(level as u32).is_ok() {
            log_message("Brightness restored via sysfs");
        } else {
            log_message("Falling back to brightnessctl for brightness restore");
            if let Err(e) = Command::new("brightnessctl")
                .arg("set")
                .arg(level.to_string())
                .output()
                .await
            {
                log_error_message(&format!("Failed to restore brightness: {}", e));
            }
        }

        // Reset stored brightness
        state.previous_brightness = None;
    }
    Ok(())
}
fn capture_sysfs_brightness() -> Option<BrightnessState> {
    let base = Path::new("/sys/class/backlight");
    let device_entry = fs::read_dir(base).ok()?.next()?;
    let device = device_entry.ok()?.file_name().to_string_lossy().to_string();

    let current = fs::read_to_string(base.join(&device).join("brightness")).ok()?;
    Some(BrightnessState {
        value: current.trim().parse().ok()?,
        device,
    })
}
fn restore_sysfs_brightness(value: u32) -> Result<(), std::io::Error> {
    let base = Path::new("/sys/class/backlight");

    // Convert Option to Result with a descriptive error
    let entry = fs::read_dir(base)
        .ok()
        .and_then(|mut it| it.next())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "No backlight device found"))??;

    let device = entry.file_name().to_string_lossy().to_string();
    let path = base.join(device).join("brightness");
    fs::write(&path, value.to_string())?;

    Ok(())
}

pub fn wake_idle_tasks(state: &ManagerState) {
    state.notify.notify_waiters();
}

// Getters and Setters
pub fn update_lock_state(state: &mut ManagerState, locked: bool) {
    state.lock_state.is_locked = locked;
}

pub fn get_compositor_manager(state: &mut ManagerState) -> bool {
    state.compositor_managed
}

pub fn set_compositor_manager(state: &mut ManagerState, value: bool) {
    state.compositor_managed = value;
}

pub fn get_manual_inhibit(state: &mut ManagerState) -> bool {
    state.manually_paused
}

pub async fn set_manual_inhibit(mgr: &mut Manager, inhibit: bool) {
    if inhibit {
        mgr.pause(true).await;
    }
}

pub async fn run_action(mgr: &mut Manager, action: &IdleActionBlock) {
    log_message(&format!(
        "Action triggered: name=\"{}\" kind={:?} timeout={} command=\"{}\"",
        action.name, action.kind, action.timeout, action.command
    ));

    // Brightness capture
    if matches!(action.kind, crate::config::model::IdleAction::Brightness) && mgr.state.previous_brightness.is_none() {
        let _ = capture_brightness(&mut mgr.state).await;
    }

    if matches!(action.kind, crate::config::model::IdleAction::LockScreen) {
        mgr.state.lock_state.is_locked = true;
        mgr.state.lock_notify.notify_one();
        log_message("Lock screen action triggered, notifying lock watcher");
    }

    let requests = prepare_action(action).await;
    for req in requests {
        match req {
            ActionRequest::PreSuspend => {
                let cmd = action.command.clone();
                run_command_for_action(mgr, action, cmd).await;
            }
            ActionRequest::RunCommand(cmd) => {
                run_command_for_action(mgr, action, cmd).await;
            }
            ActionRequest::Skip(_) => {}
        }
    }
}

pub async fn run_command_for_action(mgr: &mut Manager, action: &IdleActionBlock, cmd: String) {
    let is_lock = matches!(action.kind, crate::config::model::IdleAction::LockScreen);
    if is_lock {
        match run_command_detached(&cmd).await {
            Ok(pid) => {
                mgr.state.lock_state.pid = Some(pid);
                mgr.state.lock_state.is_locked = true;
                log_message(&format!("Lock screen started with PID {}", pid));
            }
            Err(e) => log_message(&format!("Failed to run lock command '{}': {}", cmd, e)),
        }
    } else {
        let spawned = tokio::spawn(async move {
            if let Err(e) = run_command_silent(&cmd).await {
                log_message(&format!("Failed to run command '{}': {}", cmd, e));
            }
        });
        mgr.spawned_tasks.push(spawned);
    }
}

pub async fn lock_still_active(state: &ManagerState) -> bool {
    if let Some(cmd) = &state.lock_state.command {
        is_process_running(cmd).await
    } else {
        false
    }
}

pub async fn trigger_all_idle_actions(mgr: &mut Manager) {
    use crate::config::model::IdleAction;

    let block_name = if !mgr.state.ac_actions.is_empty() || !mgr.state.battery_actions.is_empty() {
        match mgr.state.on_battery() {
            Some(true) => "battery",
            Some(false) => "ac",
            None => "default",
        }
    } else {
        "default"
    };

    // Clone the actions so we don't borrow mgr mutably while iterating
    let actions_to_trigger: Vec<IdleActionBlock> = match block_name {
        "ac" => mgr.state.ac_actions.clone(),
        "battery" => mgr.state.battery_actions.clone(),
        "default" => mgr.state.default_actions.clone(),
        _ => unreachable!(),
    };

    if actions_to_trigger.is_empty() {
        log_message("No actions defined to trigger");
        return;
    }

    log_message(&format!("Triggering all idle actions for '{}'", block_name));

    for action in actions_to_trigger {
        // Skip lockscreen if already locked
        if matches!(action.kind, IdleAction::LockScreen) && mgr.state.lock_state.is_locked {
            log_message("Skipping lock action: already locked");
            continue;
        }

        log_message(&format!("Triggering idle action '{}'", action.name));
        run_action(mgr, &action).await;
    }

    // Now update `last_triggered` after all actions are done
    let now = std::time::Instant::now();
    let actions_mut: &mut Vec<IdleActionBlock> = match block_name {
        "ac" => &mut mgr.state.ac_actions,
        "battery" => &mut mgr.state.battery_actions,
        "default" => &mut mgr.state.default_actions,
        _ => unreachable!(),
    };

    for a in actions_mut.iter_mut() {
        a.last_triggered = Some(now);
    }

    mgr.state.action_index = actions_mut.len().saturating_sub(1);
    log_message("All idle actions triggered manually");
}

