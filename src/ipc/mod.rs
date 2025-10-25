pub mod commands;

use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

use crate::{
    config, core::{
        manager::{helpers::{get_manual_inhibit, set_manual_inhibit, trigger_all_idle_actions}, Manager}, 
        services::app_inhibit::AppInhibitor,
    }, 
    ipc::commands::trigger_action_by_name, 
    log::{log_error_message, log_message}, 
    SOCKET_PATH
};

/// Spawn the IPC control socket task using a pre-bound listener.
pub async fn spawn_ipc_socket_with_listener(
    manager: Arc<tokio::sync::Mutex<Manager>>,
    app_inhibitor: Arc<tokio::sync::Mutex<AppInhibitor>>,
    cfg_path: String,
    listener: UnixListener,
) {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((mut stream, _addr)) => {
                    let mut buf = vec![0u8; 256];
                    if let Ok(n) = stream.read(&mut buf).await {
                        let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                        log_message(&format!("Received IPC command: {}", cmd));

                        let response = match cmd.as_str() {
                            // === CONFIG ===
                            "reload" => {
                                match config::parser::load_config(&cfg_path) {
                                    Ok(new_cfg) => {
                                        let mut mgr = manager.lock().await;
                                        mgr.state.update_from_config(&new_cfg).await;
                                        log_message("Config reloaded successfully");
                                        "Config reloaded successfully".to_string()
                                    }
                                    Err(e) => {
                                        log_error_message(&format!("Failed to reload config: {}", e));
                                        format!("ERROR: Failed to reload config: {e}")
                                    }
                                }
                            }

                            // === PAUSE / RESUME ===
                            "pause" => {
                                let mut mgr = manager.lock().await;
                                mgr.pause(true).await;
                                "Idle manager paused".to_string()
                            }

                            "resume" => {
                                let mut mgr = manager.lock().await;
                                mgr.resume(true).await;
                                "Idle manager resumed".to_string()
                            }

                            // === TRIGGER ===
                            cmd if cmd.starts_with("trigger ") => {
                                let step = cmd.strip_prefix("trigger ").unwrap_or("").trim();

                                if step.is_empty() {
                                    log_error_message("Trigger command missing action name");
                                    "ERROR: No action name provided".to_string()
                                } else if step == "all" {
                                    let mut mgr = manager.lock().await;
                                    trigger_all_idle_actions(&mut mgr).await;
                                    log_message("Triggered all idle actions");
                                    "All idle actions triggered".to_string()
                                } else {
                                    match trigger_action_by_name(manager.clone(), step).await {
                                        Ok(action) => format!("Action '{}' triggered successfully", action),
                                        Err(e) => format!("ERROR: {e}"),
                                    }
                                }
                            }

                            // === STOP ===
                            "stop" => {
                                log_message("Received stop command — shutting down gracefully");
                                let manager_clone = Arc::clone(&manager);
                                tokio::spawn(async move {
                                    let mut mgr = manager_clone.lock().await;
                                    mgr.shutdown().await;
                                    log_message("Manager shutdown complete, exiting process");
                                    let _ = std::fs::remove_file(SOCKET_PATH);
                                    std::process::exit(0);
                                });
                                "Stopping Stasis...".to_string()
                            }

                            // === TOGGLE INHIBIT ===
                            "toggle_inhibit" => {
                                let mut mgr = manager.lock().await;
                                let currently_inhibited = get_manual_inhibit(&mut mgr.state);

                                if currently_inhibited {
                                    set_manual_inhibit(&mut mgr, false).await;
                                    log_message("Manual inhibit disabled (toggle)");
                                } else {
                                    set_manual_inhibit(&mut mgr, true).await;
                                    log_message("Manual inhibit enabled (toggle)");
                                }

                                // Send JSON response for Waybar feedback
                                let response = if currently_inhibited {
                                    serde_json::json!({
                                        "text": "⌚",
                                        "tooltip": "Idle inhibition cleared"
                                    })
                                } else {
                                    serde_json::json!({
                                        "text": "🚫",
                                        "tooltip": "Idle inhibition active"
                                    })
                                };

                                if let Err(e) = stream.write_all(response.to_string().as_bytes()).await {
                                    log_error_message(&format!("Failed to send toggle response: {e}"));
                                }

                                "Manual inhibit toggled".to_string()
                            }

                            // === IPC info handling fixed ===
                            "info" | "info --json" => {
                                let as_json = cmd.contains("--json");

                                let mgr = manager.lock().await;
                                let idle_time = mgr.state.last_activity_display.elapsed();
                                let uptime = mgr.state.start_time.elapsed();
                                let mut inhibitor = app_inhibitor.lock().await;
                                let app_blocking = inhibitor.is_any_app_running().await;
                                let idle_inhibited = mgr.state.manually_paused || mgr.state.paused || app_blocking;

                                if as_json {
                                    // Build JSON output and return as string
                                    serde_json::json!({
                                        "text": if idle_inhibited { "☕" } else { "⌚" },
                                        "tooltip": format!(
                                            "{}\nIdle time: {}s\nUptime: {}s\nPaused: {}\nManually paused: {}\nApp blocking: {}",
                                            if idle_inhibited { "Idle inhibited" } else { "Idle active" },
                                            idle_time.as_secs(),
                                            uptime.as_secs(),
                                            mgr.state.paused,
                                            mgr.state.manually_paused,
                                            app_blocking
                                        )
                                    })
                                    .to_string()
                                } else if let Some(cfg) = &mgr.state.cfg {
                                    // Dereference Arc to call pretty_print
                                    cfg.pretty_print(Some(idle_time), Some(uptime), Some(idle_inhibited))
                                } else {
                                    "No configuration loaded".to_string()
                                }
                            }

                            "list_actions" => {
                                match crate::ipc::commands::list_available_actions(manager.clone()).await.as_slice() {
                                    [] => "No actions available".to_string(),
                                    actions => actions.join(", "),
                                }
                            }

                            // === UNKNOWN ===
                            _ => {
                                log_error_message(&format!("Unknown IPC command: {}", cmd));
                                format!("ERROR: Unknown command '{}'", cmd)
                            }
                        };

                        if let Err(e) = stream.write_all(response.as_bytes()).await {
                            log_error_message(&format!("Failed to write IPC response: {e}"));
                        }
                    }
                }

                Err(e) => log_error_message(&format!("Failed to accept IPC connection: {}", e)),
            }
        }
    });
}
