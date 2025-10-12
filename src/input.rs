use std::sync::Arc;
use std::time::Duration;
use std::os::unix::fs::OpenOptionsExt;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::process::Command;
use input::{Libinput, LibinputInterface};
use input::event::Event;
use tokio::sync::Mutex;
use crate::idle_timer::IdleTimer;
use crate::log::log_message;

/// Minimal libinput interface
struct MyInterface;
impl LibinputInterface for MyInterface {
    fn open_restricted(
        &mut self,
        path: &std::path::Path,
        flags: i32,
    ) -> Result<std::os::unix::io::OwnedFd, i32> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(flags)
            .open(path)
            .map(|f| f.into())
            .map_err(|_| -1)
    }
    fn close_restricted(&mut self, fd: std::os::unix::io::OwnedFd) {
        drop(fd)
    }
}

/// Detect which compositor is running
fn detect_compositor() -> Option<String> {
    // Check for Hyprland
    if std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        return Some("hyprland".to_string());
    }
    
    // Check for niri
    if std::env::var("NIRI_SOCKET").is_ok() {
        return Some("niri".to_string());
    }
    
    // Check XDG_CURRENT_DESKTOP as fallback
    if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
        let desktop_lower = desktop.to_lowercase();
        if desktop_lower.contains("hyprland") {
            return Some("hyprland".to_string());
        }
        if desktop_lower.contains("niri") {
            return Some("niri".to_string());
        }
    }
    
    None
}

/// Spawn input monitoring task that adapts to the compositor
pub fn spawn_input_task(idle_timer: Arc<Mutex<IdleTimer>>) {
    let compositor = detect_compositor();
    
    match compositor.as_deref() {
        Some("hyprland") => {
            log_message("Detected Hyprland - using hyprctl for activity monitoring");
            spawn_hyprland_monitor(idle_timer);
        }
        Some("niri") => {
            log_message("Detected niri - using libinput for activity monitoring");
            spawn_libinput_task(idle_timer);
        }
        _ => {
            log_message("Unknown compositor - trying libinput for activity monitoring");
            spawn_libinput_task(idle_timer);
        }
    }
}

/// Monitor Hyprland's cursor position to detect activity
fn spawn_hyprland_monitor(idle_timer: Arc<Mutex<IdleTimer>>) {
    tokio::spawn(async move {
        let mut last_x = 0i32;
        let mut last_y = 0i32;
        let mut last_active_window = String::new();
        let mut initialized = false;
        
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            
            // Get cursor position
            let cursor_output = Command::new("hyprctl")
                .args(&["cursorpos", "-j"])
                .output();
            
            let mut activity_detected = false;
            
            if let Ok(output) = cursor_output {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    // Parse JSON for cursor position
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let (Some(x), Some(y)) = (
                            json["x"].as_i64().map(|v| v as i32),
                            json["y"].as_i64().map(|v| v as i32),
                        ) {
                            if initialized && (x != last_x || y != last_y) {
                                activity_detected = true;
                            }
                            last_x = x;
                            last_y = y;
                            initialized = true;
                        }
                    }
                }
            }
            
            // Also check active window changes
            let window_output = Command::new("hyprctl")
                .args(&["activewindow", "-j"])
                .output();
            
            if let Ok(output) = window_output {
                if let Ok(text) = String::from_utf8(output.stdout) {
                    if initialized && text != last_active_window && !text.is_empty() {
                        activity_detected = true;
                    }
                    last_active_window = text;
                }
            }
            
            if activity_detected {
                let mut timer = idle_timer.lock().await;
                if !timer.paused && !timer.manually_paused {
                    timer.reset();
                }
            }
        }
    });
}

/// Original libinput-based monitoring for compositors that allow it
fn spawn_libinput_task(idle_timer: Arc<Mutex<IdleTimer>>) {
    let idle_timer_clone = Arc::clone(&idle_timer);
    tokio::task::spawn_blocking(move || {
        // Silence libinput errors
        silence_stderr();
        
        let mut li = Libinput::new_with_udev(MyInterface);
        if let Err(e) = li.udev_assign_seat("seat0") {
            eprintln!("Failed to assign seat: {:?}", e);
            log_message("Input monitoring failed - libinput could not assign seat");
            return;
        }
        
        log_message("Input monitoring started via libinput");
        
        let rt = tokio::runtime::Handle::current();
        let mut event_count = 0u64;
        
        loop {
            // Dispatch events
            if li.dispatch().is_err() {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            
            // Batch events
            let mut reset_needed = false;
            while let Some(event) = li.next() {
                match event {
                    Event::Keyboard(_) | Event::Pointer(_) => {
                        reset_needed = true;
                        event_count += 1;
                    }
                    _ => {}
                }
            }
            
            if reset_needed {
                // Log periodically to verify input detection is working
                if event_count % 100 == 1 {
                    log_message(&format!("Input activity detected (count: {})", event_count));
                }
                
                rt.block_on(async {
                    let mut timer = idle_timer_clone.lock().await;
                    if !timer.paused && !timer.manually_paused {
                        timer.reset();
                    }
                });
            }
            
            std::thread::sleep(Duration::from_millis(10));
        }
    });
}

/// Redirect libinput stderr to /dev/null to avoid spam
fn silence_stderr() {
    if let Ok(dev_null) = OpenOptions::new().write(true).open("/dev/null") {
        unsafe {
            libc::dup2(dev_null.as_raw_fd(), libc::STDERR_FILENO);
        }
    }
}
