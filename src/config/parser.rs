use eyre::{Result, eyre, WrapErr};
use regex::Regex;
use rune_cfg::{RuneConfig, Value};
use crate::config::model::*;
use crate::log::log_message;
use crate::core::utils::is_laptop;

// --- helpers ---
fn parse_app_pattern(s: &str) -> Result<AppInhibitPattern> {
    let regex_meta = ['.', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '\\', '^', '$'];
    if s.chars().any(|c| regex_meta.contains(&c)) {
        Ok(AppInhibitPattern::Regex(Regex::new(s).wrap_err("invalid regex in inhibit_apps")?))
    } else {
        Ok(AppInhibitPattern::Literal(s.to_string()))
    }
}

fn is_special_key(key: &str) -> bool {
    matches!(
        key,
        "resume_command" | "resume-command"
            | "pre_suspend_command" | "pre-suspend-command"
            | "monitor_media" | "monitor-media"
            | "ignore_remote_media" | "ignore-remote-media"
            | "respect_wayland_inhibitors" | "respect-wayland-inhibitors"
            | "inhibit_apps" | "inhibit-apps"
            | "debounce_seconds" | "debounce-seconds"
    )
}

fn collect_actions(config: &RuneConfig, path: &str) -> Result<Vec<IdleActionBlock>> {
    let mut actions = Vec::new();

    let keys = config
        .get_keys(path)
        .or_else(|_| config.get_keys(&path.replace('-', "_")))
        .unwrap_or_default();

    for key in keys {
        if is_special_key(&key) {
            continue;
        }

        let command_path = format!("{}.{}.command", path, key);
        let command = match config.get::<String>(&command_path)
            .or_else(|_| config.get::<String>(&command_path.replace('-', "_")))
        {
            Ok(c) => c,
            Err(_) => continue,
        };

        let timeout_path = format!("{}.{}.timeout", path, key);
        let timeout = match config.get::<u64>(&timeout_path)
            .or_else(|_| config.get::<u64>(&timeout_path.replace('-', "_")))
        {
            Ok(t) => t,
            Err(_) => continue,
        };

        let kind = match key.as_str() {
            "lock_screen" | "lock-screen" => IdleAction::LockScreen,
            "suspend" => IdleAction::Suspend,
            "dpms" => IdleAction::Dpms,
            "brightness" => IdleAction::Brightness,
            _ => IdleAction::Custom,
        };

        let resume_command = config.get::<String>(&format!("{}.{}.resume_command", path, key)).ok()
            .or_else(|| config.get::<String>(&format!("{}.{}.resume-command", path, key)).ok());

        actions.push(IdleActionBlock {
            name: key.clone(),
            timeout,
            command,
            kind,
            resume_command,
            last_triggered: None,
        });
    }

    Ok(actions)
}

// --- main loader ---
pub fn load_config(path: &str) -> Result<StasisConfig> {
    let config = RuneConfig::from_file(path)
        .wrap_err_with(|| eyre!("failed to load Rune config from '{}'", path))?;

    let pre_suspend_command = config
        .get::<String>("stasis.pre_suspend_command")
        .or_else(|_| config.get::<String>("stasis.pre-suspend-command"))
        .ok();

    let monitor_media = config
        .get::<bool>("stasis.monitor_media")
        .or_else(|_| config.get::<bool>("stasis.monitor-media"))
        .or_else(|err| {
            // only fallback if it's a "not found" error
            if err.to_string().contains("not found") {
                Ok(true)
            } else {
                Err(err)
            }
        })
        .wrap_err("invalid value for 'stasis.monitor_media'")?;

    let ignore_remote_media = config
        .get::<bool>("stasis.ignore_remote_media")
        .or_else(|_| config.get::<bool>("stasis.ignore-remote-media"))
        .or_else(|err| {
            if err.to_string().contains("not found") {
                Ok(true)
            } else {
                Err(err)
            }
        })
        .wrap_err("invalid value for 'stasis.ignore_remote_media'")?;

    let respect_wayland_inhibitors = config
        .get::<bool>("stasis.respect_wayland_inhibitors")
        .or_else(|_| config.get::<bool>("stasis.respect-wayland-inhibitors"))
        .or_else(|err| {
            if err.to_string().contains("not found") {
                Ok(true)
            } else {
                Err(err)
            }
        })
        .wrap_err("invalid value for 'stasis.respect_wayland_inhibitors'")?;

    let lid_close_action = config
            .get::<String>("stasis.lid_close_action")
            .or_else(|_| config.get::<String>("stasis.lid-close-action"))
            .ok()
            .map(|s| match s.as_str() {
                "ignore" => LidCloseAction::Ignore,
                "lock_screen" | "lock-screen" => LidCloseAction::LockScreen,
                "suspend" => LidCloseAction::Suspend,
                other if other.starts_with("custom:") => {
                    LidCloseAction::Custom(other.trim_start_matches("custom:").trim().to_string())
                }
                _ => {
                    log_message(&format!(
                        "Unknown lid_close_action '{}', defaulting to ignore",
                        s
                    ));
                    LidCloseAction::Ignore
                }
            })
            .unwrap_or(LidCloseAction::Ignore);

    let lid_open_action = config
            .get::<String>("stasis.lid_open_action")
            .or_else(|_| config.get::<String>("stasis.lid-open-action"))
            .ok()
            .map(|s| match s.as_str() {
                "ignore" => LidOpenAction::Ignore,
                "wake" => LidOpenAction::Wake,
                other if other.starts_with("custom:") => {
                    LidOpenAction::Custom(other.trim_start_matches("custom:").trim().to_string())
                }
                _ => {
                    log_message(&format!(
                        "Unknown lid_close_action '{}', defaulting to ignore",
                        s
                    ));
                    LidOpenAction::Ignore
                }
            })
            .unwrap_or(LidOpenAction::Ignore);

    let debounce_seconds = config
        .get::<u8>("stasis.debounce_seconds")
        .or_else(|_| config.get::<u8>("stasis.debounce-seconds"))
        .unwrap_or(3u8);

    let inhibit_apps: Vec<AppInhibitPattern> = config
        .get_value("stasis.inhibit_apps")
        .or_else(|_| config.get_value("stasis.inhibit-apps"))
        .ok()
        .and_then(|v| match v {
            Value::Array(arr) => Some(
                arr.iter()
                    .filter_map(|v| match v {
                        Value::String(s) => parse_app_pattern(s).ok(),
                        Value::Regex(s) => Regex::new(s).ok().map(AppInhibitPattern::Regex),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();

    let laptop = is_laptop();    
    let actions = if laptop {
        let mut all = Vec::new();
        all.extend(
            collect_actions(&config, "stasis.on_ac")
                .or_else(|_| collect_actions(&config, "stasis.on-ac"))?
        );
        all.extend(
            collect_actions(&config, "stasis.on_battery")
                .or_else(|_| collect_actions(&config, "stasis.on-battery"))?
        );
        all
    } else {
        collect_actions(&config, "stasis")?
    };

    if actions.is_empty() {
        return Err(eyre!("no valid idle actions found in config"));
    }

    log_message("Parsed Config:");
    log_message(&format!("  pre_suspend_command = {:?}", pre_suspend_command));
    log_message(&format!("  monitor_media = {:?}", monitor_media));
    log_message(&format!("  ignore_remote_media = {:?}", ignore_remote_media));
    log_message(&format!(
        "  respect_wayland_inhibitors = {:?}",
        respect_wayland_inhibitors
    ));
    log_message(&format!("  debounce_seconds = {:?}", debounce_seconds));
    log_message(&format!("  lid_close_action = {:?}", lid_close_action));
    log_message(&format!("  lid_open_action = {:?}", lid_open_action));
    log_message(&format!(
        "  inhibit_apps = [{}]",
        inhibit_apps
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));   
    log_message("  actions:");
    for action in &actions {
        let mut details = format!(
            "    {}: timeout={}s, command=\"{}\"",
            action.name, action.timeout, action.command
        );
        if let Some(resume_cmd) = &action.resume_command {
            details.push_str(&format!(", resume_command=\"{}\"", resume_cmd));
        }
        log_message(&details);
    }

    Ok(StasisConfig {
        actions,
        pre_suspend_command,
        monitor_media,
        ignore_remote_media,
        respect_wayland_inhibitors,
        inhibit_apps,
        debounce_seconds,
        lid_close_action,
        lid_open_action,
    })
}
