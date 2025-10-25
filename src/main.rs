pub mod cli;
pub mod config;
pub mod core;
pub mod ipc;
pub mod log;

use std::{env::var, fs, process::exit, sync::Arc};
use clap::Parser;
use eyre::Result;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream}, 
    sync::Mutex, 
    task::LocalSet
};

use crate::{
    cli::Command,
    config::parser::load_config,
    core::{
        manager::{spawn_idle_task, spawn_lock_watcher, Manager}, 
        services::{
            app_inhibit::{AppInhibitor, spawn_app_inhibit_task},
            dbus::listen_for_power_events, 
            input::spawn_input_task,
            media::spawn_media_monitor_dbus,
            power_detection::spawn_power_source_monitor,
            wayland::{setup as setup_wayland},
        }
    },
};

use crate::{
    cli::Args, 
    config::get_config_path, 
    log::{log_error_message, log_message, set_verbose}
};

const SOCKET_PATH: &str = "/tmp/stasis.sock";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    if var("WAYLAND_DISPLAY").is_err() {
        eprintln!("Warn: Stasis requires wayland to run.");
        exit(1);
    }

    // --- Handle subcommands via socket ---
    if let Some(cmd) = &args.command {
        use tokio::net::UnixStream;

        match cmd {
            Command::Info { json } => {
                if let Ok(mut stream) = UnixStream::connect(SOCKET_PATH).await {
                    let msg = if *json { "info --json" } else { "info" };
                    let _ = stream.write_all(msg.as_bytes()).await;

                    let mut response = Vec::new();
                    let _ = stream.read_to_end(&mut response).await;
                    println!("{}", String::from_utf8_lossy(&response));
                } else {
                    if *json {
                        println!(r#"{{"text":"😴","tooltip":"No running Stasis instance found"}}"#);
                    } else {
                        eprintln!("No running Stasis instance found");
                        std::process::exit(1);
                    }
                }
            }
            Command::Trigger { step } => {
                if let Ok(mut stream) = UnixStream::connect(SOCKET_PATH).await {
                    let msg = format!("trigger {}", step);
                    let _ = stream.write_all(msg.as_bytes()).await;

                    let mut response = Vec::new();
                    let _ = stream.read_to_end(&mut response).await;
                    let response_text = String::from_utf8_lossy(&response);
                    
                    if response_text.starts_with("ERROR:") {
                        eprintln!("{}", response_text.trim_start_matches("ERROR:").trim());
                        std::process::exit(1);
                    } else if !response_text.is_empty() {
                        println!("{}", response_text);
                    } else {
                        println!("Action '{}' triggered", step);
                    }
                } else {
                    eprintln!("No running Stasis instance found");
                    std::process::exit(1);
                }
            }
            Command::ListActions => {
                if let Ok(mut stream) = UnixStream::connect(SOCKET_PATH).await {
                    let _ = stream.write_all(b"list_actions").await;

                    let mut response = Vec::new();
                    let _ = stream.read_to_end(&mut response).await;
                    println!("{}", String::from_utf8_lossy(&response));
                } else {
                    eprintln!("No running Stasis instance found");
                    std::process::exit(1);
                }
            }
            _ => {
                let msg = match cmd {
                    Command::Reload => "reload",
                    Command::Pause => "pause",
                    Command::Resume => "resume",
                    Command::ToggleInhibit => "toggle_inhibit",
                    Command::Stop => "stop",
                    _ => unreachable!(),
                };

                if let Ok(mut stream) = UnixStream::connect(SOCKET_PATH).await {
                    let _ = stream.write_all(msg.as_bytes()).await;

                    // Only read response for commands that send one back
                    if msg == "toggle_inhibit" {
                        let mut response = Vec::new();
                        let _ = stream.read_to_end(&mut response).await;
                        println!("{}", String::from_utf8_lossy(&response));
                    } else {
                        // Success message for other commands
                        let success_msg = match cmd {
                            Command::Reload => "Configuration reloaded successfully",
                            Command::Pause => "Idle timers paused",
                            Command::Resume => "Idle timers resumed",
                            Command::Stop => "Stasis daemon stopped",
                            _ => "",
                        };
                        if !success_msg.is_empty() {
                            println!("{}", success_msg);
                        }
                    }
                } else {
                    eprintln!("No running Stasis instance found");
                    std::process::exit(1);
                }
            }
        }

        return Ok(());
    }
    
    // --- Single Instance enforcement ---
    let just_help_or_version = std::env::args().any(|a| matches!(a.as_str(), "-V" | "--version" | "-h" | "--help" | "help"));
    if let Ok(_) = UnixStream::connect(SOCKET_PATH).await {
        if !just_help_or_version {
            eprintln!("Another instance of Stasis is already running");
        }
        log_error_message("Another instance is already running.");
        return Ok(());
    }
    let _ = fs::remove_file(SOCKET_PATH);
    let listener = UnixListener::bind(SOCKET_PATH).map_err(|_| {
        eyre::eyre!("Failed to bind control socket. Another instance may be running.")
    })?;
    
    // --- Load config ---
    let config_path = args.config.unwrap_or(get_config_path().await?);
    if args.verbose {
        log_message("Verbose mode enabled");
        set_verbose(true);
    }
    let cfg = Arc::new(load_config(config_path.to_str().unwrap())?);
    let manager = Manager::new(Arc::clone(&cfg));
    let manager = Arc::new(Mutex::new(manager));

    // Immediately trigger instants at startup
    {
        let mut mgr = manager.lock().await;
        mgr.trigger_instant_actions().await;
    }
    
    // --- Spawn background tasks ---
    let idle_handle = spawn_idle_task(Arc::clone(&manager));
    let lock_handle = spawn_lock_watcher(Arc::clone(&manager)).await;
    let input_handle = spawn_input_task(Arc::clone(&manager));
    
    // Store handles in manager
    {
        let mut mgr = manager.lock().await;
        mgr.idle_task_handle = Some(idle_handle);
        mgr.lock_task_handle = Some(lock_handle);
        mgr.input_task_handle = Some(input_handle);
    } 
    
    // --- Spawn suspend event listener ---
    let dbus_manager = Arc::clone(&manager);
    tokio::spawn(async move {
        if let Err(e) = listen_for_power_events(dbus_manager).await {
            log_error_message(&format!("D-Bus suspend event listener failed: {}", e));
        }
    });
    
    // --- AC/Battery Detection ---
    let laptop_manager = Arc::clone(&manager);
    tokio::spawn(spawn_power_source_monitor(laptop_manager));

   // --- Spawn app inhibit task ---
    let app_inhibitor = spawn_app_inhibit_task(
        Arc::clone(&manager),
        Arc::clone(&cfg)
    ).await;
   
    // --- Spawn media monitor task ---
    if cfg.monitor_media {
        if let Err(e) = spawn_media_monitor_dbus(Arc::clone(&manager), cfg.ignore_remote_media).await {
            log_error_message(&format!("Failed to spawn media monitor: {}", e));
        }
    }
    
    // --- Wayland setup ---
    let wayland_manager = Arc::clone(&manager);
    let _ = setup_wayland(wayland_manager, cfg.respect_wayland_inhibitors).await?;

    // -- IPC Control Socket ---
    ipc::spawn_ipc_socket_with_listener(
        Arc::clone(&manager),
        Arc::clone(&app_inhibitor),
        config_path.to_str().unwrap().to_string(),
        listener,
    ).await;

    setup_shutdown_handler(
        Arc::clone(&manager),
        Arc::clone(&app_inhibitor),
    ).await;

   // Monitor Wayland compositor connection
    spawn_wayland_monitor(
        Arc::clone(&manager),
        Arc::clone(&app_inhibitor),
    ).await;
    
    // --- Log startup message ---
    log_message(&format!("Running. Idle actions loaded: {}", cfg.actions.len()));
    
    // --- Run main async tasks ---
    let local = LocalSet::new();
    local.run_until(async {
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        Ok::<(), eyre::Report>(())
    }).await?;
    
    Ok(())
}

/// Async shutdown handler (Ctrl+C / SIGTERM)
async fn setup_shutdown_handler(
    idle_timer: Arc<Mutex<Manager>>,
    app_inhibitor: Arc<Mutex<AppInhibitor>>,
) {
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).unwrap();

    tokio::spawn({
        let manager = Arc::clone(&idle_timer);
        let app_inhibitor = Arc::clone(&app_inhibitor);
        async move {
            tokio::select! {
                _ = sigint.recv() => {
                    log_message("Received SIGINT, shutting down...");
                },
                _ = sigterm.recv() => {
                    log_message("Received SIGTERM, shutting down...");
                },
                _ = sighup.recv() => {
                    log_message("Received SIGHUP, shutting down...");
                },
            }

            // Shutdown idle timer
            manager.lock().await.shutdown().await;

            // Shutdown app inhibitor
            app_inhibitor.lock().await.shutdown().await;

            let _ = std::fs::remove_file(SOCKET_PATH);
            log_message("Shutdown complete, goodbye!");
            std::process::exit(0);
        }
    });
}

async fn spawn_wayland_monitor(
    manager: Arc<Mutex<Manager>>,
    app_inhibitor: Arc<Mutex<AppInhibitor>>,
) {
    tokio::spawn(async move {
        let wayland_display = match var("WAYLAND_DISPLAY") {
            Ok(display) => display,
            Err(_) => return,
        };
        
        let xdg_runtime = match var("XDG_RUNTIME_DIR") {
            Ok(dir) => dir,
            Err(_) => "/run/user/1000".to_string(),
        };
        
        let socket_path = format!("{}/{}", xdg_runtime, wayland_display);
        
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            
            // Check if Wayland socket still exists
            if !std::path::Path::new(&socket_path).exists() {
                log_message("Wayland compositor socket disappeared, shutting down...");
                
                // Shutdown idle timer
                manager.lock().await.shutdown().await;
                
                // Shutdown app inhibitor
                app_inhibitor.lock().await.shutdown().await;
                
                let _ = std::fs::remove_file(SOCKET_PATH);
                log_message("Shutdown complete, goodbye!");
                std::process::exit(0);
            }
        }
    });
}
