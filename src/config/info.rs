use std::{collections::BTreeSet, time::Duration};
use crate::{config::model::StasisConfig, core::utils};

impl StasisConfig {
    pub fn pretty_print(
        &self,
        idle_time: Option<Duration>,
        uptime: Option<Duration>,
        is_inhibited: Option<bool>,
    ) -> String {
        let mut out = String::new();

        // General settings
        out.push_str("General:\n");
        out.push_str(&format!(
            "  PreSuspendCommand  = {}\n",
            self.pre_suspend_command.as_deref().unwrap_or("-")
        ));
        out.push_str(&format!(
            "  MonitorMedia       = {}\n",
            if self.monitor_media { "true" } else { "false" }
        ));
        out.push_str(&format!("  IgnoreRemoteMedia  = {}\n", self.ignore_remote_media));
        out.push_str(&format!(
            "  RespectInhibitors  = {}\n",
            if self.respect_wayland_inhibitors { "true" } else { "false" }
        ));
        out.push_str(&format!("  DebounceSeconds    = {}\n", self.debounce_seconds));
        out.push_str(&format!("  LidCloseAction     = {}\n", self.lid_close_action));
        out.push_str(&format!("  LidOpenAction      = {}\n", self.lid_open_action));

        let apps = if self.inhibit_apps.is_empty() {
            "-".to_string()
        } else {
            self.inhibit_apps
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        out.push_str(&format!("  InhibitApps        = {}\n", apps));

        if let Some(idle) = idle_time {
            out.push_str(&format!("  IdleTime           = {}\n", utils::format_duration(idle)));
        }
        if let Some(up) = uptime {
            out.push_str(&format!("  Uptime             = {}\n", utils::format_duration(up)));
        }
        if let Some(inhibited) = is_inhibited {
            out.push_str(&format!("  IdleInhibited      = {}\n", inhibited));
        }

        // Actions
        out.push_str("\nActions:\n");

        // Track groups in order of first occurrence
        let mut seen_groups = BTreeSet::new();
        for action in &self.actions {
            let group = if action.name.starts_with("ac.") {
                "AC"
            } else if action.name.starts_with("battery.") {
                "Battery"
            } else {
                "Desktop"
            };

            // Print group header only once
            if seen_groups.insert(group) {
                out.push_str(&format!("  [{}]\n", group));
            }

            out.push_str(&format!(
                "    {:<20} Timeout={} Kind={} Command=\"{}\"",
                action.name,
                action.timeout,
                action.kind,
                action.command
            ));

            if let Some(resume_cmd) = &action.resume_command {
                out.push_str(&format!(" ResumeCommand=\"{}\"", resume_cmd));
            }

            out.push('\n');
        }

        out
    }
}
