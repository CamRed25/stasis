use std::time::Duration;
use eyre::Result;
use tokio::process::Command;
use std::process::Stdio;

use crate::config::model::{IdleActionBlock, IdleAction};
use crate::log::log_message;

#[derive(Debug, Clone)]
pub enum ActionRequest {
    RunCommand(String),
    PreSuspend,
    #[allow(dead_code)]
    Skip(String),
}

/// Prepare action for execution, similar to old logic
pub async fn prepare_action(action: &IdleActionBlock) -> Vec<ActionRequest> {
    let cmd = action.command.clone();

    match action.kind {
        IdleAction::Suspend => {
            let mut reqs = Vec::new();
            reqs.push(ActionRequest::PreSuspend);
            if !cmd.trim().is_empty() {
                reqs.push(ActionRequest::RunCommand(cmd));
            }
            reqs
        }

        IdleAction::LockScreen => {
            let probe_cmd = &action.command;

            if is_process_running(probe_cmd).await {
                log_message("Lockscreen already running, skipping action.");
                vec![ActionRequest::Skip(probe_cmd.to_string())]
            } else {
                vec![ActionRequest::RunCommand(action.command.clone())]
            }
        }

        _ => {
            if cmd.trim().is_empty() {
                vec![]
            } else {
                vec![ActionRequest::RunCommand(cmd)]
            }
        }
    }
}

/// Run a shell command silently (log to /tmp/stasis.log)
pub async fn run_command_silent(cmd: &str) -> Result<()> {
    let log_file = "/tmp/stasis.log";
    let fut = async {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!("{cmd} >> {log_file} 2>&1"))
            .envs(std::env::vars())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let status = child.wait().await?;
        if !status.success() {
            eyre::bail!("Command '{}' exited with status {:?}", cmd, status.code());
        }
        Ok::<(), eyre::Report>(())
    };

    tokio::time::timeout(Duration::from_secs(30), fut).await??;
    Ok(())
}

/// Run a command detached (e.g., lock screen) and return its PID
pub async fn run_command_detached(command: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return Err("Empty command".into());
    }

    let child = Command::new(parts[0])
        .args(&parts[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let pid = child.id().ok_or("Failed to get child PID")?;
    Ok(pid)
}

/// Check if a process matching `cmd` is running (by name)
pub async fn is_process_running(cmd: &str) -> bool {
    if cmd.trim().is_empty() {
        return false;
    }
    let first_word = cmd.split_whitespace().next().unwrap_or("");
    if first_word.is_empty() {
        return false;
    }

    match Command::new("pgrep").arg(first_word).output().await {
        Ok(output) => !output.stdout.is_empty(),
        Err(_) => false,
    }
}
