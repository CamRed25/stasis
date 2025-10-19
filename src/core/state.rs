use std::time::{Duration, Instant};
use super::tasks::{cleanup_tasks, spawn_task_limited};
use crate::core::brightness::restore_brightness;
use crate::log::log_message;
use super::IdleTimer;

impl IdleTimer {
    /// Resets idle timer activity and sets a short debounce window.
    pub async fn reset(&mut self) {
        self.last_activity = Instant::now();

        self.apply_reset().await;
        let debounce_delay = Duration::from_secs(self.cfg.debounce_seconds as u64);
        self.debounce_until = Some(Instant::now() + debounce_delay);
    }

    /// Internal helper for clearing idle flags, brightness state, and resuming.
    pub(crate) async fn apply_reset(&mut self) {
        let was_idle = self.is_idle_flags.iter().any(|&b| b);

        if self.is_lock_running().await && (!self.lock_notified || self.action_epoch > self.lock_advanced_epoch) {
            log_message("User active — lock detected, advancing timers past lock");
            self.advance_past_lock().await;
            self.lock_notified = true;
            self.lock_advanced_epoch = self.action_epoch;
        }


        self.lock_resume_command = None;
        self.lock_pid = None; // Clear tracked lock PID on reset
        
        if let Some(state) = self.previous_brightness.take() {
            log_message("Restoring brightness immediately on user activity");
            restore_brightness(&state);
        }
        
        self.last_activity = Instant::now();
        cleanup_tasks(&mut self.spawned_tasks);
        
        if was_idle {
            for triggered_action in &self.triggered_actions {
                if let Some(action) = triggered_action {
                    if action.kind == crate::config::IdleActionKind::LockScreen {
                        continue;
                    }
                    
                    if let Some(resume_cmd) = &action.resume_command {
                        let cmd_clone = resume_cmd.clone();
                        let action_kind = format!("{:?}", action.kind);
                        spawn_task_limited(&mut self.spawned_tasks, async move {
                            log_message(&format!("Firing resume command for {}", action_kind));
                            if let Err(e) = super::actions::run_command_silent(&cmd_clone).await {
                                log_message(&format!("Resume command failed: {}", e));
                            }
                        });
                    }
                }
            }
            
            self.suspend_occurred = false;
        }
        
        self.is_idle_flags.fill(false);
        self.idle_debounce_until = None;
        self.active_kinds.clear();
        self.triggered_actions.iter_mut().for_each(|a| *a = None);
    }

    pub fn is_manually_inhibited(&self) -> bool {
        self.manually_paused
    }

    pub async fn set_manual_inhibit(&mut self, inhibit: bool) {
        if inhibit {
            self.pause(true);
        } else {
            self.resume(true).await;
        }
    }

    pub fn pause(&mut self, manually: bool) {
        if manually {
            self.manually_paused = true;
            self.paused = false;
            log_message("Idle timers manually paused");
        } else if !self.manually_paused {
            self.paused = true;
            log_message("Idle timers automatically paused");
        }
    }

    pub async fn resume(&mut self, manually: bool) {
        if manually {
            if self.manually_paused {
                self.manually_paused = false;
                self.paused = false;
                log_message("Idle timers manually resumed");
                self.reset_state_after_resume().await;
            }
        } else if !self.manually_paused && self.paused {
            self.paused = false;
            log_message("Idle timers automatically resumed");
            self.reset_state_after_resume().await;
        }
    }

    /// Internal helper for handling resume when timers are unpaused
    pub async fn reset_state_after_resume(&mut self) {
        self.last_activity = Instant::now();
        cleanup_tasks(&mut self.spawned_tasks);
        self.is_idle_flags.fill(false);
        self.active_kinds.clear();

        // Clear tracked lock PID on resume
        self.lock_pid = None;

        // Restore brightness if we have it saved
        if let Some(state) = self.previous_brightness.take() {
            log_message("Restoring brightness on manual resume");
            restore_brightness(&state);
        }

        self.triggered_actions.iter_mut().for_each(|a| *a = None);

        if self.is_lock_running().await && (!self.lock_notified || self.action_epoch > self.lock_advanced_epoch) {
            log_message("Resuming — lock detected, advancing timers past lock");
            self.advance_past_lock().await;
            self.lock_notified = true;
            self.lock_advanced_epoch = self.action_epoch;
        }
    }
}
