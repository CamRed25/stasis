use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};
use futures::future::BoxFuture;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::config::{IdleAction, IdleActionKind, IdleConfig};
use crate::log::{log_error_message, log_message};
use super::brightness::{capture_brightness, restore_brightness, BrightnessState};
use super::tasks::{cleanup_tasks, spawn_task_limited};

pub struct IdleTimer {
    pub(crate) cfg: IdleConfig,
    pub(crate) last_activity: Instant,
    pub(crate) debounce_until: Option<Instant>,
    pub(crate) idle_debounce_until: Option<Instant>,
    pub(crate) paused: bool,
    pub(crate) manually_paused: bool,
    pub(crate) previous_brightness: Option<BrightnessState>,
    pub(crate) suspend_occurred: bool,
    pub(crate) spawned_tasks: Vec<JoinHandle<()>>,
    pub(crate) is_idle_flags: Vec<bool>,
    pub(crate) active_kinds: HashSet<String>,
    pub(crate) triggered_actions: Vec<Option<IdleAction>>,
    pub(crate) on_ac: bool,
    pub(crate) start_time: Instant,
    pub(crate) actions: Vec<IdleAction>,
    pub(crate) ac_actions: Vec<IdleAction>,
    pub(crate) battery_actions: Vec<IdleAction>,
    pub(crate) pre_suspend_command: Option<String>,
    pub(crate) lock_resume_done: bool,
    pub(crate) lock_resume_command: Option<String>,

    idle_task_handle: Option<JoinHandle<()>>,
    lock_monitor_handle: Option<JoinHandle<()>>,
    compositor_managed: bool,
    lock_process_running: bool,
}

impl IdleTimer {
    pub fn new(cfg: &IdleConfig) -> Self {
        let on_ac = true;

        let default_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|(k, _)| !k.starts_with("ac.") && !k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect();

        let ac_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|(k, _)| k.starts_with("ac."))
            .map(|(_, v)| v.clone())
            .collect();

        let battery_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|(k, _)| k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect();

        let actions = if !ac_actions.is_empty() || !battery_actions.is_empty() {
            if on_ac { ac_actions.clone() } else { battery_actions.clone() }
        } else {
            default_actions.clone()
        };

        let actions_clone = actions.clone();
        let now = Instant::now();
        
        let timer = Self {
            cfg: cfg.clone(),
            start_time: now,
            last_activity: now,
            debounce_until: None,
            idle_debounce_until: None,
            actions,
            ac_actions,
            battery_actions,
            pre_suspend_command: cfg.pre_suspend_command.clone(),
            is_idle_flags: vec![false; actions_clone.len()],
            triggered_actions: vec![None; actions_clone.len()],
            compositor_managed: false,
            active_kinds: HashSet::new(),
            previous_brightness: None,
            on_ac,
            paused: false,
            manually_paused: false,
            suspend_occurred: false,
            spawned_tasks: Vec::new(),
            idle_task_handle: None,
            lock_monitor_handle: None,
            lock_process_running: false,
            lock_resume_done: false,
            lock_resume_command: None,
        };

        timer
    }

    pub async fn init(&mut self) {
        self.trigger_instant_actions().await;
    }

    pub fn elapsed_idle(&self) -> Duration {
        if let Some(until) = self.debounce_until {
            if Instant::now() < until {
                return Duration::ZERO;
            }
        }
        Instant::now().duration_since(self.last_activity)
    }

    pub fn trigger_instant_actions(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut instant_actions = Vec::new();
            for (i, action) in self.actions.iter().enumerate() {
                if action.timeout_seconds == 0 && !self.is_idle_flags[i] {
                    instant_actions.push((i, action.clone()));
                }
            }

            for (i, action) in instant_actions {
                self.is_idle_flags[i] = true;
                self.triggered_actions[i] = Some(action.clone());
                self.active_kinds.insert(action.kind.to_string());

                log_message(&format!(
                    "Instant action triggered: kind={} command=\"{}\"",
                    action.kind, action.command
                ));

                if action.kind == IdleActionKind::Brightness && self.previous_brightness.is_none() {
                    if let Some(state) = capture_brightness() {
                        self.previous_brightness = Some(state.clone());
                    } else {
                        log_error_message("Could not capture current brightness");
                    }
                }

                let requests = super::actions::prepare_action(&action).await;
                for req in requests {
                    match req {
                        super::actions::ActionRequest::PreSuspend => {
                            self.trigger_pre_suspend(false, false).await;
                        }
                        super::actions::ActionRequest::RunCommand(cmd) => {
                            let cmd_clone = cmd.clone();
                            spawn_task_limited(&mut self.spawned_tasks, async move {
                                if let Err(e) = super::actions::run_command_silent(&cmd_clone).await {
                                    log_error_message(&format!("Failed to run command '{}': {}", cmd_clone, e));
                                }
                            });
                        }
                        super::actions::ActionRequest::Skip(_) => {}
                    }
                }
            }
        })
    }

    /// Check if a lock command process is currently running
    pub async fn is_lock_running(&self) -> bool {
        for action in &self.actions {
            if action.kind == IdleActionKind::LockScreen {
                if let Some(lock_cmd) = &action.lock_command {
                    // Extract process name from command
                    let process_name = lock_cmd.split_whitespace().next().unwrap_or("");
                    if !process_name.is_empty() {
                        if is_process_running(process_name).await {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Advance timers past lock-screen when lock is detected
    pub async fn advance_past_lock(&mut self) {
        log_message("Lock process detected, advancing timers past lock-screen");

        self.lock_resume_done = false;
        self.lock_resume_command = None;

        for i in 0..self.actions.len() {
            if self.actions[i].kind == IdleActionKind::LockScreen {
                self.is_idle_flags[i] = true;
                self.triggered_actions[i] = Some(self.actions[i].clone());
                self.active_kinds.insert(self.actions[i].kind.to_string());

                if let Some(resume) = &self.actions[i].resume_command {
                    self.lock_resume_command = Some(resume.clone());
                }
            }
        }
    }

    pub async fn check_idle(&mut self) {
        if self.paused {
            return;
        }

        // Check if lock process is running
        let lock_running = self.is_lock_running().await;
        
        if lock_running && !self.lock_process_running {
            // Lock just started
            self.lock_process_running = true;
            self.advance_past_lock().await;
        } 

        if let Some(until) = self.debounce_until {
            if Instant::now() < until {
                return;
            } else {
                self.debounce_until = None;
            }
        }

        let elapsed = self.elapsed_idle();

        for i in 0..self.actions.len() {
            let action = &self.actions[i];
            let key = action.kind.to_string();

            if action.timeout_seconds == 0 || self.is_idle_flags[i] || self.active_kinds.contains(&key)
            {
                continue;
            }

            let timeout = Duration::from_secs(action.timeout_seconds);

            if elapsed >= timeout {
                if let Some(until) = self.idle_debounce_until {
                    if Instant::now() < until {
                        return;
                    } else {
                        self.idle_debounce_until = None;
                    }
                } else {
                    let debounce_delay = Duration::from_secs(self.cfg.debounce_seconds as u64);
                    self.idle_debounce_until = Some(Instant::now() + debounce_delay);
                    return;
                }

                self.is_idle_flags[i] = true;
                self.triggered_actions[i] = Some(action.clone());
                self.active_kinds.insert(key.clone());

                if action.kind == IdleActionKind::Brightness && self.previous_brightness.is_none() {
                    if let Some(state) = capture_brightness() {
                        self.previous_brightness = Some(state.clone());
                    }
                }

                let requests = super::actions::prepare_action(action).await;
                for req in requests {
                    match req {
                        super::actions::ActionRequest::PreSuspend => {
                            // Skip
                        }
                        super::actions::ActionRequest::RunCommand(cmd) => {
                            let cmd_clone = cmd.clone();
                            spawn_task_limited(&mut self.spawned_tasks, async move {
                                if let Err(e) = super::actions::run_command_silent(&cmd_clone).await {
                                    log_error_message(&format!("Failed to run command '{}': {}", cmd_clone, e));
                                }
                            });
                        }
                        super::actions::ActionRequest::Skip(_) => {}
                    }
                }
            }
        }

        cleanup_tasks(&mut self.spawned_tasks);
    }

    pub async fn update_power_source(&mut self, on_ac: bool) {
        if self.on_ac == on_ac {
            return;
        }

        self.on_ac = on_ac;
        cleanup_tasks(&mut self.spawned_tasks);

        if let Some(state) = self.previous_brightness.take() {
            restore_brightness(&state);
        }

        self.actions = if on_ac { self.ac_actions.clone() } else { self.battery_actions.clone() };
        self.is_idle_flags = vec![false; self.actions.len()];
        self.triggered_actions = vec![None; self.actions.len()];
        self.active_kinds.clear();
        self.trigger_instant_actions().await;
    }

    pub async fn trigger_pre_suspend(&mut self, rewind_timers: bool, manual: bool) {
        if !manual {
            self.suspend_occurred = true;
        }

        let mut has_pre_suspend = false;

        if let Some(cmd) = &self.pre_suspend_command {
            has_pre_suspend = true;
            let cmd_clone = cmd.clone();
        
            if let Err(e) = super::actions::run_command_detached(&cmd_clone).await {
                log_message(&format!("Pre-suspend command failed: {}", e));
            }
        }

        if has_pre_suspend {
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        }

        if rewind_timers {
            // Check if lock is running on resume
            if self.is_lock_running().await {
                self.lock_process_running = true;
                self.advance_past_lock().await;
            } else {
                self.last_activity = Instant::now();
                self.is_idle_flags.iter_mut().for_each(|f| *f = false);
                self.triggered_actions.iter_mut().for_each(|a| *a = None);
                self.active_kinds.clear();
                self.trigger_instant_actions().await;
            }
        }
    }
 
    pub fn shortest_timeout(&self) -> Duration {
        self.actions
            .iter()
            .filter(|a| a.timeout_seconds > 0)
            .map(|a| Duration::from_secs(a.timeout_seconds))
            .min()
            .unwrap_or_else(|| Duration::from_secs(60))
    }

    pub fn set_compositor_managed(&mut self, value: bool) { 
        self.compositor_managed = value; 
    }

    pub fn is_compositor_managed(&self) -> bool { 
        self.compositor_managed 
    }

    pub fn mark_all_idle(&mut self) { 
        self.is_idle_flags.fill(true); 
    }

    pub async fn update_from_config(&mut self, cfg: &IdleConfig) {
        cleanup_tasks(&mut self.spawned_tasks);

        let default_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|(k, _)| !k.starts_with("ac.") && !k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect();

        self.ac_actions = cfg
            .actions
            .iter()
            .filter(|(k, _)| k.starts_with("ac."))
            .map(|(_, v)| v.clone())
            .collect();

        self.battery_actions = cfg
            .actions
            .iter()
            .filter(|(k, _)| k.starts_with("battery."))
            .map(|(_, v)| v.clone())
            .collect();

        self.actions = if !self.ac_actions.is_empty() || !self.battery_actions.is_empty() {
            if self.on_ac {
                self.ac_actions.clone()
            } else {
                self.battery_actions.clone()
            }
        } else {
            default_actions
        };

        self.cfg = cfg.clone();
        self.is_idle_flags = vec![false; self.actions.len()];
        self.triggered_actions = vec![None; self.actions.len()];
        self.pre_suspend_command = cfg.pre_suspend_command.clone();
        self.last_activity = Instant::now();
        self.active_kinds.clear();
        self.previous_brightness = None;

        self.trigger_instant_actions().await;
        log_message("Idle timers reloaded from config");
    }

    pub async fn shutdown(&mut self) {
        log_message("Shutting down IdleTimer...");
        if let Some(handle) = self.idle_task_handle.take() {
            handle.abort();
        }
        if let Some(handle) = self.lock_monitor_handle.take() {
            handle.abort();
        }

        for handle in self.spawned_tasks.drain(..) {
            handle.abort();
        }
    }
}

/// Check if a process is currently running by name
async fn is_process_running(process_name: &str) -> bool {
    let output = tokio::process::Command::new("pgrep")
        .arg("-x")
        .arg(process_name)
        .output()
        .await;

    match output {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

/// Spawn main idle monitor task
pub async fn spawn_idle_task(idle_timer: Arc<Mutex<IdleTimer>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let min_timeout = {
            let timer = idle_timer.lock().await;
            timer.shortest_timeout()
        };

        let tick_rate = if min_timeout < Duration::from_secs(15) {
            Duration::from_millis(250)
        } else if min_timeout < Duration::from_secs(60) {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(1)
        };

        let mut ticker = tokio::time::interval(tick_rate);

        loop {
            ticker.tick().await;
            let mut timer = idle_timer.lock().await;

            if !timer.manually_paused {
                timer.check_idle().await;
            }
        }
    })
}

/// Spawn lock process monitor task (checks every 2 seconds)
pub async fn spawn_lock_monitor_task(idle_timer: Arc<Mutex<IdleTimer>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Granularity target: check every 250ms (4× per second)
        // - fast enough to react almost instantly
        // - light enough to stay near idle CPU levels
        let check_interval = Duration::from_millis(250);

        loop {
            let mut timer = idle_timer.lock().await;

            let lock_running = timer.is_lock_running().await;

            if lock_running && !timer.lock_process_running {
                // Lock just started
                timer.lock_process_running = true;
                timer.advance_past_lock().await;
            } else if !lock_running && timer.lock_process_running {
                // Lock just ended
                timer.lock_process_running = false;
                log_message("Lock process ended, resetting timers");
                timer.reset_state_after_resume().await;
            }

            drop(timer); // release the lock before sleeping
            tokio::time::sleep(check_interval).await;
        }
    })
}


