use std::{fs, time::Duration};

pub fn is_laptop() -> bool {
    let chassis_path = "/sys/class/dmi/id/chassis_type";

    if let Ok(content) = fs::read_to_string(chassis_path) {
        match content.trim() {
            "8" | "9" | "10" => true,
            _ => false,
        }
    } else {
        false
    }
}

pub fn format_duration(dur: Duration) -> String {
    let secs = dur.as_secs();

    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let minutes = secs / 60;
        let seconds = secs % 60;
        format!("{}m {}s", minutes, seconds)
    } else {
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        format!("{}h {}m", hours, minutes)
    }
}
