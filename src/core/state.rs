use std::time::{Duration, Instant};
use tokio::time::sleep;
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
        self.last_activity = Instant::now();
        cleanup_tasks(&mut self.spawned_tasks);
        self.is_idle_flags.fill(false);
        self.idle_debounce_until = None;

        if was_idle {
            let lock_running = self.is_lock_running().await;
            if lock_running {
                if !self.lock_process_running {
                    log_message("Lock detected on wake — advancing past lock timeout");
                    self.lock_process_running = true;
                }
                let _ = self.advance_past_lock().await;
            } else {
                self.lock_process_running = false;
            }

            if let Some(state) = &self.previous_brightness {
                restore_brightness(state);
            }

            for triggered_action in &self.triggered_actions {
                if let Some(action) = triggered_action {
                    if action.kind == crate::config::IdleActionKind::LockScreen {
                        continue;
                    }
                    if let Some(resume_cmd) = &action.resume_command {
                        let cmd_clone = resume_cmd.clone();
                        spawn_task_limited(&mut self.spawned_tasks, async move {
                            let _ = super::actions::run_command_silent(&cmd_clone).await;
                        });
                    }
                }
            }

            self.suspend_occurred = false;
        }

        self.active_kinds.clear();
        self.previous_brightness = None;
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
            self.paused = false; // Clear automatic pause when manually pausing
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

    /// Internal helper for running lock resume command 
    pub async fn reset_state_after_resume(&mut self) {
        self.last_activity = Instant::now();
        cleanup_tasks(&mut self.spawned_tasks);
        self.is_idle_flags.fill(false);

        if !self.lock_resume_done {
            if let Some(cmd) = &self.lock_resume_command {
                let cmd_clone = cmd.clone();
                spawn_task_limited(&mut self.spawned_tasks, async move {
                    sleep(Duration::from_millis(200)).await;
                    let _ = super::actions::run_command_silent(&cmd_clone).await;
                });
            }
            self.lock_resume_done = true;
        }

        self.active_kinds.clear();
        self.previous_brightness = None;
        self.triggered_actions.iter_mut().for_each(|a| *a = None);
    }
}
