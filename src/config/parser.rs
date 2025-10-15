use eyre::{Result, eyre, WrapErr};
use regex::Regex;
use rune_cfg::{RuneConfig, Value};
use std::collections::HashMap;
use crate::config::model::*;
use crate::log::log_message;
use crate::utils::is_laptop;

// --- helpers ---
fn parse_app_pattern(s: &str) -> Result<AppPattern> {
    let regex_meta = ['.', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '\\', '^', '$'];
    if s.chars().any(|c| regex_meta.contains(&c)) {
        Ok(AppPattern::Regex(Regex::new(s).wrap_err("invalid regex in inhibit_apps")?))
    } else {
        Ok(AppPattern::Literal(s.to_string()))
    }
}

fn normalize_key(key: &str) -> String {
    key.replace('_', "-")
}

fn is_special_key(key: &str) -> bool {
    matches!(
        key,
        "resume_command" | "resume-command"
            | "pre_suspend_command" | "pre-suspend-command"
            | "monitor_media" | "monitor-media"
            | "ignore_remote_media" | "ignore-remote-media"
            | "respect_idle_inhibitors" | "respect-idle-inhibitors"
            | "inhibit_apps" | "inhibit-apps"
            | "debounce_seconds" | "debounce-seconds"
    )
}

fn collect_actions(config: &RuneConfig, path: &str, prefix: &str) -> Result<HashMap<String, IdleAction>> {
    let mut actions = HashMap::new();

    let keys = config
        .get_keys(path)
        .or_else(|_| config.get_keys(&path.replace('-', "_")))
        .unwrap_or_default();

    for key in keys {
        if is_special_key(&key) {
            continue;
        }

        let command_path = format!("{}.{}.command", path, key);
        let command = config
            .get::<String>(&command_path)
            .or_else(|_| config.get::<String>(&command_path.replace('-', "_")))
            .wrap_err_with(|| eyre!("missing or invalid command for '{}'", key))
            .ok();
        if command.is_none() {
            continue;
        }
        let command = command.unwrap();

        let timeout_path = format!("{}.{}.timeout", path, key);
        let timeout_seconds = config
            .get::<u64>(&timeout_path)
            .or_else(|_| config.get::<u64>(&timeout_path.replace('-', "_")))
            .wrap_err_with(|| eyre!("missing or invalid timeout for '{}'", key))
            .ok();
        if timeout_seconds.is_none() {
            continue;
        }
        let timeout_seconds = timeout_seconds.unwrap();

        let kind = match key.as_str() {
            "lock_screen" | "lock-screen" => IdleActionKind::LockScreen,
            "suspend" => IdleActionKind::Suspend,
            "dpms" => IdleActionKind::Dpms,
            "brightness" => IdleActionKind::Brightness,
            _ => IdleActionKind::Custom,
        };

        let lock_command = if matches!(kind, IdleActionKind::LockScreen) {
            let lock_path = format!("{}.{}.lock_command", path, key);
            config
                .get::<String>(&lock_path)
                .or_else(|_| config.get::<String>(&lock_path.replace('-', "_")))
                .ok()
        } else {
            None
        };

        let resume_path = format!("{}.{}.resume_command", path, key);
        let resume_command = config
            .get::<String>(&resume_path)
            .or_else(|_| config.get::<String>(&resume_path.replace('-', "_")))
            .ok();

        actions.insert(
            format!("{}.{}", prefix, normalize_key(&key)),
            IdleAction {
                timeout_seconds,
                command,
                kind,
                lock_command,
                resume_command,
            },
        );
    }

    Ok(actions)
}

// --- main loader ---
pub fn load_config(path: &str) -> Result<IdleConfig> {
    let config = RuneConfig::from_file(path)
        .wrap_err_with(|| eyre!("failed to load Rune config from '{}'", path))?;

    let pre_suspend_command = config
        .get::<String>("idle.pre_suspend_command")
        .or_else(|_| config.get::<String>("idle.pre-suspend-command"))
        .ok();

    let monitor_media = config
        .get::<bool>("idle.monitor_media")
        .or_else(|_| config.get::<bool>("idle.monitor-media"))
        .or_else(|err| {
            // only fallback if it's a "not found" error
            if err.to_string().contains("not found") {
                Ok(true)
            } else {
                Err(err)
            }
        })
        .wrap_err("invalid value for 'idle.monitor_media'")?;

    let ignore_remote_media = config
        .get::<bool>("idle.ignore_remote_media")
        .or_else(|_| config.get::<bool>("idle.ignore-remote-media"))
        .or_else(|err| {
            if err.to_string().contains("not found") {
                Ok(true)
            } else {
                Err(err)
            }
        })
        .wrap_err("invalid value for 'idle.ignore_remote_media'")?;

    let respect_idle_inhibitors = config
        .get::<bool>("idle.respect_idle_inhibitors")
        .or_else(|_| config.get::<bool>("idle.respect-idle-inhibitors"))
        .or_else(|err| {
            if err.to_string().contains("not found") {
                Ok(true)
            } else {
                Err(err)
            }
        })
        .wrap_err("invalid value for 'idle.respect_idle_inhibitors'")?;

    let debounce_seconds = config.get_or("idle.debounce_seconds", 3u8);

    let inhibit_apps: Vec<AppPattern> = config
        .get_value("idle.inhibit_apps")
        .or_else(|_| config.get_value("idle.inhibit-apps"))
        .ok()
        .and_then(|v| match v {
            Value::Array(arr) => Some(
                arr.iter()
                    .filter_map(|v| match v {
                        Value::String(s) => parse_app_pattern(s).ok(),
                        Value::Regex(s) => Regex::new(s).ok().map(AppPattern::Regex),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();

    let laptop = is_laptop();
    let actions = if laptop {
        let mut map = HashMap::new();
        map.extend(
            collect_actions(&config, "idle.on_ac", "ac")
                .or_else(|_| collect_actions(&config, "idle.on-ac", "ac"))
                .wrap_err("failed to parse on_ac section")?,
        );
        map.extend(
            collect_actions(&config, "idle.on_battery", "battery")
                .or_else(|_| collect_actions(&config, "idle.on-battery", "battery"))
                .wrap_err("failed to parse on_battery section")?,
        );
        map
    } else {
        collect_actions(&config, "idle", "desktop").wrap_err("failed to parse idle section")?
    };

    if actions.is_empty() {
        return Err(eyre!("no valid idle actions found in config"));
    }

    log_message("Parsed Config:");
    log_message(&format!("  pre_suspend_command = {:?}", pre_suspend_command));
    log_message(&format!("  monitor_media = {:?}", monitor_media));
    log_message(&format!("  ignore_remote_media = {:?}", ignore_remote_media));
    log_message(&format!(
        "  respect_idle_inhibitors = {:?}",
        respect_idle_inhibitors
    ));
    log_message(&format!("  debounce_seconds = {:?}", debounce_seconds));
    log_message(&format!(
        "  inhibit_apps = [{}]",
        inhibit_apps
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    log_message("  actions:");
    for (key, action) in &actions {
        let mut details = format!(
            "    {}: timeout={}s, kind={:?}, command=\"{}\"",
            key, action.timeout_seconds, action.kind, action.command
        );
        if let Some(lock_cmd) = &action.lock_command {
            details.push_str(&format!(", lock_command=\"{}\"", lock_cmd));
        }
        if let Some(resume_cmd) = &action.resume_command {
            details.push_str(&format!(", resume_command=\"{}\"", resume_cmd));
        }
        log_message(&details);
    }

    Ok(IdleConfig {
        actions,
        pre_suspend_command,
        monitor_media,
        ignore_remote_media,
        respect_idle_inhibitors,
        inhibit_apps,
        debounce_seconds,
    })
}
