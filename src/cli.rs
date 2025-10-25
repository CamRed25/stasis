use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "Stasis",
    version = env!("CARGO_PKG_VERSION")
)]
pub struct Args {
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[arg(short, long, action)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Command>
}

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(about = "Reload the configuration without restarting Stasis")]
    Reload,
    
    #[command(about = "Pause all timers")]
    Pause,
    
    #[command(about = "Resume timers after a pause")]
    Resume,

    #[command(about = "List available actions based on current config")]
    ListActions,

    #[command(about = "Manually trigger a specific idle action by name")]
    Trigger { 
        #[arg(help = "Action name to trigger (e.g., 'brightness', 'dpms', 'lock_screen', 'pre_suspend', 'suspend')")]
        step: String,
    },

    #[command(about = "Toggle manual idle inhibition (for status bars such as Waybar)")]
    ToggleInhibit,

    #[command(about = "Stop the currently running instances of Stasis")]
    Stop,

    #[command(about = "Display durrent session information")]
    Info {
        #[arg(long, help = "Output as JSON (for Waybar or scripts)")]
        json: bool,
    },
}
