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
use super::pre_suspend::run_pre_suspend_sync;
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

    idle_task_handle: Option<JoinHandle<()>>,
    actions: Vec<IdleAction>,
    ac_actions: Vec<IdleAction>,
    battery_actions: Vec<IdleAction>,
    pre_suspend_command: Option<String>,
    compositor_managed: bool,
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

    pub async fn check_idle(&mut self) {
        if self.paused {
            return;
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

    /// Trigger a specific action by name (supports normalized names)
    pub async fn trigger_action_by_name(&mut self, name: &str) -> Result<String, String> {
        // Normalize the input name (accept both hyphens and underscores)
        let normalized = name.replace('_', "-").to_lowercase();
        
        // Special case: pre_suspend command
        if normalized == "pre-suspend" || normalized == "presuspend" {
            self.trigger_pre_suspend(false, true).await;
            return Ok("pre_suspend".to_string());
        }

        // Find matching action and its index first
        // We need to search both by kind name AND by config key name (for custom actions)
        let mut found: Option<(usize, IdleAction, String)> = None;
        
        // Get the current power state prefix
        let prefix = if self.on_ac { "ac." } else { "battery." };
        let using_power_profiles = !self.ac_actions.is_empty() || !self.battery_actions.is_empty();
        
        for (i, action) in self.actions.iter().enumerate() {
            let kind_name = format!("{:?}", action.kind).to_lowercase().replace('_', "-");
            
            // Match by kind name (lockscreen, suspend, dpms, brightness)
            if kind_name == normalized || format!("{:?}", action.kind).to_lowercase() == normalized {
                found = Some((i, action.clone(), kind_name));
                break;
            }
            
            // For custom actions, match by config key name
            if action.kind == IdleActionKind::Custom {
                // Search through config actions to find the key name
                for (key, cfg_action) in &self.cfg.actions {
                    // Check if this config action matches our current action
                    if cfg_action.command == action.command && cfg_action.timeout_seconds == action.timeout_seconds {
                        // Extract just the action name (remove ac./battery./desktop. prefix)
                        let action_name = if using_power_profiles {
                            key.strip_prefix(prefix)
                                .or_else(|| key.strip_prefix("desktop."))
                                .unwrap_or(key)
                        } else {
                            key.strip_prefix("desktop.").unwrap_or(key)
                        };
                        
                        let key_normalized = action_name.replace('_', "-").to_lowercase();
                        if key_normalized == normalized {
                            found = Some((i, action.clone(), action_name.to_string()));
                            break;
                        }
                    }
                }
                if found.is_some() {
                    break;
                }
            }
        }

        // Now trigger the action if found
        if let Some((i, action, action_name)) = found {
            if !self.is_idle_flags[i] {
                self.is_idle_flags[i] = true;
                self.triggered_actions[i] = Some(action.clone());
                self.active_kinds.insert(action.kind.to_string());

                if action.kind == IdleActionKind::Brightness && self.previous_brightness.is_none() {
                    if let Some(state) = capture_brightness() {
                        self.previous_brightness = Some(state.clone());
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
            
            return Ok(action_name);
        }

        // Action not found - provide helpful error with all available actions
        let mut available: Vec<String> = Vec::new();
        
        // Add built-in action kinds
        for action in &self.actions {
            let kind_name = format!("{:?}", action.kind).to_lowercase();
            if kind_name != "custom" && !available.contains(&kind_name) {
                available.push(kind_name);
            }
        }
        
        // Add custom action names from config
        let prefix = if self.on_ac { "ac." } else { "battery." };
        for (key, _) in &self.cfg.actions {
            let action_name = if using_power_profiles {
                key.strip_prefix(prefix)
                    .or_else(|| key.strip_prefix("desktop."))
                    .unwrap_or(key)
            } else {
                key.strip_prefix("desktop.").unwrap_or(key)
            };
            
            if !available.contains(&action_name.to_string()) {
                available.push(action_name.to_string());
            }
        }
        
        if self.pre_suspend_command.is_some() && !available.contains(&"pre_suspend".to_string()) {
            available.push("pre_suspend".to_string());
        }
        
        available.sort();
        
        Err(format!(
            "Action '{}' not found. Available actions: {}\n\
            Note: Custom actions defined in your config (e.g. `[desktop.<name>]` or `[ac.<name>]`) \
            can also be triggered using their names.",
            name,
            available.join(", ")
        ))

    }

    pub async fn trigger_idle(&mut self) {
        for i in 0..self.actions.len() {
            if !self.is_idle_flags[i] {
                self.is_idle_flags[i] = true;
                let action = self.actions[i].clone();
                self.triggered_actions[i] = Some(action.clone());
                
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
        }
    }

    pub async fn trigger_pre_suspend(&mut self, rewind_timers: bool, manual: bool) {
        if !manual {
            self.suspend_occurred = true;
        }

        if let Some(cmd) = &self.pre_suspend_command {
            if let Err(e) = run_pre_suspend_sync(cmd) {
                log_message(&format!("Pre-suspend command failed: {}", e));
            }

            if rewind_timers {
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

        for handle in self.spawned_tasks.drain(..) {
            handle.abort();
        }
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
