use std::time::{Duration, Instant};

use crate::core::IdleTimer;
use crate::config::{IdleAction, IdleActionKind};
use crate::log::{log_error_message};
use super::brightness::capture_brightness;
use super::tasks::spawn_task_limited;

impl IdleTimer {
    pub async fn trigger_action_by_name(&mut self, name: &str) -> Result<String, String> {
        // Normalize the input name (accept both hyphens and underscores)
        let normalized = name.replace('_', "-").to_lowercase();
        
        // Special case: pre_suspend command
        if normalized == "pre-suspend" || normalized == "presuspend" {
            self.trigger_pre_suspend(false, true).await;
            return Ok("pre_suspend".to_string());
        }

        let mut found: Option<(usize, IdleAction, String)> = None;
        
        let prefix = if self.on_ac { "ac." } else { "battery." };
        let using_power_profiles = !self.ac_actions.is_empty() || !self.battery_actions.is_empty();
        
        for (i, action) in self.actions.iter().enumerate() {
            let kind_name = format!("{:?}", action.kind).to_lowercase().replace('_', "-");
            
            if kind_name == normalized || format!("{:?}", action.kind).to_lowercase() == normalized {
                found = Some((i, action.clone(), kind_name));
                break;
            }
            
            for (key, cfg_action) in &self.cfg.actions {
                if cfg_action.command == action.command && cfg_action.timeout_seconds == action.timeout_seconds {
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

        // Now trigger the action if found
        if let Some((i, action, action_name)) = found {
            if !self.is_idle_flags[i] {
                if action.timeout_seconds > 0 {
                    let timeout_duration = Duration::from_secs(action.timeout_seconds);
                    self.last_activity = Instant::now() - timeout_duration;
                } else {
                    self.last_activity = Instant::now() - Duration::from_secs(5);
                }
                
                // IMPORTANT: Clear any existing debounce so the manual trigger works immediately
                self.debounce_until = None;
                self.idle_debounce_until = None;
                
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
}
