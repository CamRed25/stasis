pub mod actions;
pub mod brightness;
pub mod pre_suspend;
pub mod timer;
pub mod state;
pub mod tasks;

pub use timer::IdleTimer;
use crate::config::IdleConfig;

/// Build idle timer (legacy only for now)
pub fn build_idle_timer(cfg: &IdleConfig) -> IdleTimer {
    IdleTimer::new(cfg)
}
