pub mod actions;
pub mod helpers;
pub mod state;
pub mod tasks;

use std::{sync::Arc, time::{Duration, Instant}};
use tokio::{
    sync::Mutex, 
    task::JoinHandle, 
    time::{Instant as TokioInstant, sleep, sleep_until}
};

pub use self::state::ManagerState;
use crate::{
    config::model::StasisConfig, 
    core::manager::{
        actions::{is_process_running, run_command_detached},
        helpers::{restore_brightness, run_action}, 
    }, 
    log::log_message
};

pub struct Manager {
    pub state: ManagerState,
    pub spawned_tasks: Vec<JoinHandle<()>>,
    pub idle_task_handle: Option<JoinHandle<()>>,
    pub lock_task_handle: Option<JoinHandle<()>>,
    pub media_task_handle: Option<JoinHandle<()>>,
    pub input_task_handle: Option<JoinHandle<()>>,
}

impl Manager {
    pub fn new(cfg: Arc<StasisConfig>) -> Self {
        Self {
            state: ManagerState::new(cfg),
            spawned_tasks: Vec::new(),
            idle_task_handle: None,
            lock_task_handle: None,
            media_task_handle: None,
            input_task_handle: None,
        }
    }

    pub async fn trigger_instant_actions(&mut self) {
        if self.state.instants_triggered {
            return;
        }

        let instant_actions = self.state.instant_actions.clone();

        log_message("Triggering instant actions at startup...");
        for action in instant_actions {
            run_action(self, &action).await;
        }

        self.state.instants_triggered = true;
    }

    pub fn reset_instant_actions(&mut self) {
        self.state.instants_triggered = false;
        log_message("Instant actions reset; they can trigger again");
    }

    // Called when libinput service resets (on user activity)
    pub async fn reset(&mut self) {
        let cfg = match &self.state.cfg {
            Some(cfg) => Arc::clone(cfg),
            None => {
                log_message("No configuration available, skipping reset");
                return;
            }
        };
        
        // Restore brightness if needed
        if self.state.previous_brightness.is_some() {
            if let Err(e) = restore_brightness(&mut self.state).await {
                log_message(&format!("Failed to restore brightness: {}", e));
            }
        }
        
        let now = Instant::now();
        self.state.last_activity_display = now;
        let debounce = Duration::from_secs(cfg.debounce_seconds as u64);
        self.state.debounce = Some(now + debounce);

        // Clear all last_triggered times so actions count from debounce end
        for actions in [&mut self.state.default_actions, &mut self.state.ac_actions, &mut self.state.battery_actions] {
            for a in actions.iter_mut() {
                a.last_triggered = None;
            }
        }

        let block_name = if !self.state.ac_actions.is_empty() || !self.state.battery_actions.is_empty() {
            match self.state.on_battery() {
                Some(true) => "battery",
                Some(false) => "ac",
                None => "default",
            }
        } else {
            "default"
        };

        // Only update current_block if it changed
        if self.state.current_block.as_deref() != Some(block_name) {
            self.state.current_block = Some(block_name.to_string());
        }

        // Recompute action_index for the current block
        let actions = match block_name {
            "ac" => &mut self.state.ac_actions,
            "battery" => &mut self.state.battery_actions,
            "default" => &mut self.state.default_actions,
            _ => unreachable!(),
        };

        // Skip instant actions here. handled elsewhere
        let index = actions.iter()
            .position(|a| a.last_triggered.is_none())
            .unwrap_or(actions.len().saturating_sub(1));

        if actions[index].is_instant() {
            return;
        }

        if self.state.lock_state.is_locked {
            if let Some(lock_index) = actions.iter().position(|a| matches!(a.kind, crate::config::model::IdleAction::LockScreen)) {
                if self.state.action_index <= lock_index {


                    self.state.action_index = lock_index.saturating_add(1);
                    
                    let debounce_end = now + debounce;
                    if self.state.action_index < actions.len() {
                        actions[self.state.action_index].last_triggered = Some(debounce_end);
                    }
                    
                    self.state.lock_state.post_advanced = true;
                }
            }
        }
        self.state.notify.notify_one();
    }

    // Check whether we have been idle enough to elapse one of the timeouts
    pub async fn check_timeouts(&mut self) {
        if self.state.paused || self.state.manually_paused {
            return;
        }
        
        let now = Instant::now();
        if let Some(until) = self.state.debounce {
            if now < until {
                return;
            } else {
                self.state.last_activity = now;
                self.state.debounce = None;
            }
        }
        
        // Determine which block to use
        let block_name = if !self.state.ac_actions.is_empty() || !self.state.battery_actions.is_empty() {
            match self.state.on_battery() {
                Some(true) => "battery",
                Some(false) => "ac",
                None => "default",
            }
        } else {
            "default"
        };
        
        // Only update if changed
        if self.state.current_block.as_deref() != Some(block_name) {
            self.state.current_block = Some(block_name.to_string());
        }
            
        // Get reference to the right actions Vec
        let actions = match block_name {
            "ac" => &mut self.state.ac_actions,
            "battery" => &mut self.state.battery_actions,
            "default" => &mut self.state.default_actions,
            _ => unreachable!(),
        };
        
        if actions.is_empty() {
            return;
        }
        
        let index = self.state.action_index.min(actions.len() - 1);
        
        // Skip lock if already locked
        if matches!(actions[index].kind, crate::config::model::IdleAction::LockScreen) 
            && self.state.lock_state.is_locked {
            return;
        }
        
        // Calculate elapsed - read the data we need before calling run_action
        let last_ref = actions[index].last_triggered.unwrap_or(self.state.last_activity);
        let elapsed = now.duration_since(last_ref);
        let timeout = actions[index].timeout;
        
        if elapsed >= Duration::from_secs(timeout as u64) {
            // Clone the action to pass to run_action (avoids borrow conflict)
            let action_clone = actions[index].clone();
            
            // Update timing BEFORE running action
            actions[index].last_triggered = Some(now);
            
            // Advance index
            self.state.action_index += 1;
            if self.state.action_index < actions.len() {
                actions[self.state.action_index].last_triggered = Some(now);
            } else {
                self.state.action_index = actions.len() - 1;
            }
            
            // Now we can call run_action with full mutable self access
            run_action(self, &action_clone).await;
        }
    }

    pub fn next_action_instant(&self) -> Option<Instant> {
        if self.state.paused || self.state.manually_paused {
            return None;
        }

        let mut min_time: Option<Instant> = None;

        for actions in [&self.state.default_actions, &self.state.ac_actions, &self.state.battery_actions] {
            for action in actions.iter() {
                let last = action.last_triggered.unwrap_or(self.state.last_activity);
                let next_time = last + Duration::from_secs(action.timeout as u64);
                
                min_time = Some(match min_time {
                    None => next_time,
                    Some(current_min) => current_min.min(next_time),
                });
            }
        }

        min_time
    }

    pub async fn trigger_pre_suspend(&mut self, manual: bool) {
        if !manual {
            self.state.suspend_occured = true;
        }

        let mut has_pre_suspend = false;

        if let Some(cmd) = &self.state.pre_suspend_command {
            has_pre_suspend = true;
            let cmd = cmd.clone();
            
            if let Err(e) = run_command_detached(&cmd).await {
                log_message(&format!("Pre-suspemd command failed: {}", e));

            }
        }

        if has_pre_suspend {
           sleep(Duration::from_millis(700)).await;
        }
    }

    pub async fn update_power_source(&mut self) {
        match self.state.on_battery() {
            Some(true) => {
                // on battery, proceed
            }
            Some(false) | None => {
                return;
            }
        }
    }
  
    pub async fn advance_past_lock(&mut self) {
        log_message("Advancing state past lock stage...");
        self.state.lock_state.post_advanced = true;
        self.state.lock_state.last_advanced = Some(Instant::now());
    }

    pub async fn pause(&mut self, manual: bool) {
        if manual {
            self.state.manually_paused = true;
            self.state.paused = false;
            log_message("Idle timers manually paused");
        } else if !self.state.manually_paused {
            self.state.paused = true;
            log_message("Idle timers automatically paused");
        }
    }

    pub async fn resume(&mut self, manually: bool) {
        if manually {
            if self.state.manually_paused {
                self.state.manually_paused = false;
                self.state.paused = false;
                log_message("Idle timers manually resumed");
            }
        } else if !self.state.manually_paused && self.state.paused {
            self.state.paused = false;
            log_message("Idle timers automatically resumed");
        }
    }

    pub async fn toggle_state(&mut self, inhibit: bool) {
        if inhibit {
            self.pause(true).await;
        } else {
            self.resume(true).await;
        }
    }

    pub async fn shutdown(&mut self) {
        self.state.shutdown_flag.notify_waiters();

        // Optionally: give tasks time to clean up
        sleep(Duration::from_millis(200)).await;

        if let Some(handle) = self.idle_task_handle.take() {
            handle.abort();
        }

        if let Some(handle) = self.lock_task_handle.take() {
            handle.abort();
        }

        if let Some(handle) = self.input_task_handle.take() {
            handle.abort();
        }

        for handle in self.spawned_tasks.drain(..) {
            handle.abort();
        }
    }
}

pub fn spawn_idle_task(manager: Arc<Mutex<Manager>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Grab both the next timeout and the notify handles
            let (next_instant, notify, shutdown) = {
                let mgr = manager.lock().await;
                (
                    mgr.next_action_instant(),
                    mgr.state.notify.clone(),
                    mgr.state.shutdown_flag.clone(),
                )
            };

            // Compute how long we should sleep           
            let sleep_deadline = match next_instant {
                Some(instant) => {
                    let now = Instant::now();
                    let max_sleep = Duration::from_secs(60); // wake periodically
                    if instant <= now {
                        now + Duration::from_millis(50)
                    } else if instant - now > max_sleep {
                        now + max_sleep
                    } else {
                        instant
                    }
                }
                None => Instant::now() + Duration::from_secs(60),
            };


            tokio::select! {
                _ = sleep_until(TokioInstant::from_std(sleep_deadline)) => {},
                _ = notify.notified() => {
                    // Woken up by external event (reset, AC change, playback)
                    continue; // recalc immediately
                }
                _ = shutdown.notified() => {
                    break; // exit loop cleanly
                }
            }

            // Now check timeouts only once after wake
            let mut mgr = manager.lock().await;
            if !mgr.state.paused && !mgr.state.manually_paused {
                mgr.check_timeouts().await;
            }
        }

        log_message("Idle loop shutting down...");
    })
}

pub async fn spawn_lock_watcher(manager: Arc<Mutex<Manager>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Grab shutdown notify handle outside
            let shutdown = {
                let mgr = manager.lock().await;
                mgr.state.shutdown_flag.clone()
            };

            // Wait until lock actually becomes active
            {
                let mut mgr = manager.lock().await;
                while !mgr.state.lock_state.is_locked {
                    let lock_notify = mgr.state.lock_notify.clone();
                    drop(mgr);
                    tokio::select! {
                        _ = lock_notify.notified() => {},
                        _ = shutdown.notified() => {
                            log_message("Lock watcher shutting down...");
                            return;
                        }
                    }
                    mgr = manager.lock().await;
                }
            }

            log_message("Lock detected — entering lock watcher");

            // Lock is active — monitor it until it ends or shutdown
            loop {
                // Snapshot relevant info
                let (maybe_cmd, was_locked, shutdown, lock_notify) = {
                    let mgr = manager.lock().await;
                    (
                        mgr.state.lock_state.command.clone(),
                        mgr.state.lock_state.is_locked,
                        mgr.state.shutdown_flag.clone(),
                        mgr.state.lock_notify.clone(),
                    )
                };

                if !was_locked {
                    break;
                }

                // Check if process is still running (if we have a command)
                let still_active = if let Some(cmd) = maybe_cmd {
                    is_process_running(&cmd).await
                } else {
                    sleep(Duration::from_millis(500)).await;
                    true
                };

                if !still_active {
                    let mut mgr = manager.lock().await;
                    mgr.state.lock_state.pid = None;
                    mgr.state.lock_state.post_advanced = false;
                    mgr.state.action_index = 0;
                    mgr.state.lock_state.is_locked = false;
                    mgr.state.notify.notify_one();
                    log_message("Lockscreen ended — exiting lock watcher");
                    break;
                }

                // Wait a bit or for external change / shutdown
                tokio::select! {
                    _ = lock_notify.notified() => {},
                    _ = sleep(Duration::from_millis(500)) => {},
                    _ = shutdown.notified() => {
                        log_message("Lock watcher shutting down during active lock...");
                        return;
                    }
                }
            }
        }
    })
}


