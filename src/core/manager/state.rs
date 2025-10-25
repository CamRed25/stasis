use std::{sync::Arc, time::{Duration, Instant}};

use tokio::sync::Notify;

use crate::{
    config::model::{IdleAction, IdleActionBlock, StasisConfig}, log::log_message
};

#[derive(Debug)]
pub struct ManagerState {
    pub ac_actions: Vec<IdleActionBlock>,
    pub action_index: usize,
    pub active_flags: ActiveFlags,
    pub app_inhibit_debounce: Option<Instant>,
    pub battery_actions: Vec<IdleActionBlock>,
    pub cfg: Option<Arc<StasisConfig>>,
    pub chassis: ChassisType, 
    pub compositor_managed: bool,
    pub current_block: Option<String>,
    pub debounce: Option<Instant>,
    pub default_actions: Vec<IdleActionBlock>,
    pub instant_actions: Vec<IdleActionBlock>,
    pub instants_triggered: bool,
    pub last_activity: Instant,
    pub last_activity_display: Instant,
    pub lock_state: LockState,
    pub paused: bool,
    pub manually_paused: bool,
    pub notify: Arc<Notify>,
    pub lock_notify: Arc<Notify>,
    pub previous_brightness: Option<u8>,
    pub pre_suspend_command: Option<String>,
    pub shutdown_flag: Arc<Notify>,
    pub start_time: Instant,
    pub suspend_occured: bool,
}

impl Default for ManagerState {
    fn default() -> Self {
        let now = Instant::now();

        Self {
            ac_actions: Vec::new(),
            action_index: 0,
            active_flags: ActiveFlags::default(),
            app_inhibit_debounce: None,
            battery_actions: Vec::new(),
            cfg: None,
            chassis: ChassisType::Desktop(DesktopState),
            compositor_managed: false,
            current_block: None,
            debounce: None,
            default_actions: Vec::new(),
            instant_actions: Vec::new(),
            instants_triggered: false,
            last_activity: now, 
            last_activity_display: now,
            lock_state: LockState::default(),
            manually_paused: false,
            notify: Arc::new(Notify::new()),
            lock_notify: Arc::new(Notify::new()),
            paused: false,
            previous_brightness: None,
            pre_suspend_command: None,
            shutdown_flag: Arc::new(Notify::new()),
            start_time: now,
            suspend_occured: false,
        }
    }
}

impl ManagerState {
    pub fn new(cfg: Arc<StasisConfig>) -> Self { 
        let default_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| !a.name.starts_with("ac.") && !a.name.starts_with("battery."))
            .cloned()
            .collect();

        let ac_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("ac."))
            .cloned()
            .collect();

        let battery_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("battery."))
            .cloned()
            .collect();


        let now = Instant::now();
        let debounce = Some(now + Duration::from_secs(cfg.debounce_seconds as u64));

        let instant_actions: Vec<_> = default_actions
            .iter()
            .chain(&ac_actions)
            .chain(&battery_actions)
            .filter(|a| a.is_instant())
            .cloned()
            .collect();

        let state = Self {
            ac_actions,
            action_index: 0,
            active_flags: ActiveFlags::default(),
            app_inhibit_debounce: None,
            battery_actions,
            cfg: Some(cfg.clone()),
            chassis: ChassisType::Desktop(DesktopState),
            compositor_managed: false,
            current_block: None,
            debounce,
            default_actions,
            instant_actions,
            instants_triggered: false,
            last_activity: now,
            last_activity_display: now,
            lock_state: LockState::from_config(&cfg),
            manually_paused: false,
            notify: Arc::new(Notify::new()),
            lock_notify: Arc::new(Notify::new()),
            paused: false,
            previous_brightness: None,
            pre_suspend_command: None,
            shutdown_flag: Arc::new(Notify::new()),
            start_time: now,
            suspend_occured: false,
        };

        state
    }

    pub fn is_laptop(&self) -> bool {
        matches!(self.chassis, ChassisType::Laptop(_))
    }

    pub fn on_battery(&self) -> Option<bool> {
        match &self.chassis {
            ChassisType::Laptop(l) => Some(l.on_battery),
            ChassisType::Desktop(_) => None,
        }
    }

    pub fn set_on_battery(&mut self, value: bool) {
        if let ChassisType::Laptop(l) = &mut self.chassis {
            l.on_battery = value;
        }
    }

    pub async fn update_from_config(&mut self, cfg: &StasisConfig) {
        self.active_flags = ActiveFlags::default();
        self.previous_brightness = None;
        self.pre_suspend_command = cfg.pre_suspend_command.clone();

        // Split actions into blocks
        let default_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| !a.name.starts_with("ac.") && !a.name.starts_with("battery."))
            .cloned()
            .collect();

        let ac_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("ac."))
            .cloned()
            .collect();

        let battery_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("battery."))
            .cloned()
            .collect();

        // Replace the old state vectors
        self.default_actions = default_actions;
        self.ac_actions = ac_actions;
        self.battery_actions = battery_actions;

        // Reset last_triggered for all actions
        for actions in [&mut self.default_actions, &mut self.ac_actions, &mut self.battery_actions] {
            for a in actions.iter_mut() {
                a.last_triggered = None;
            }
        }

        // Recompute instant_actions for new config
        self.instant_actions = self
            .default_actions
            .iter()
            .chain(&self.ac_actions)
            .chain(&self.battery_actions)
            .filter(|a| a.is_instant())
            .cloned()
            .collect();

        // Reset instant trigger flag
        self.instants_triggered = false;

        self.cfg = Some(Arc::new(cfg.clone()));
        self.lock_state = LockState::from_config(cfg);
        self.last_activity = Instant::now();
        self.last_activity_display = Instant::now();

        // Reset action index
        self.action_index = 0;

        // Reset debounce according to new cfg
        let debounce = Duration::from_secs(cfg.debounce_seconds as u64);
        self.debounce = Some(Instant::now() + debounce);

        // Wake idle task to recalc immediately
        self.notify.notify_one();

        log_message("Idle timers reloaded from config");
    }
}

#[derive(Debug)]
pub enum ChassisType {
    Laptop(LaptopState),
    Desktop(DesktopState),
}

#[derive(Debug)]
pub struct LaptopState {
    pub on_battery: bool,
}

#[derive(Debug)]
pub struct DesktopState;

impl Default for ChassisType {
    fn default() -> Self {
        ChassisType::Desktop(DesktopState)
    }
}

#[derive(Debug)]
pub struct LockState {
    pub is_locked: bool,
    pub pid: Option<u32>,
    pub command: Option<String>,
    pub last_advanced: Option<Instant>,
    pub post_advanced: bool,
}

impl Default for LockState {
    fn default() -> Self {
        Self {
            is_locked: false,
            pid: None,
            command: None,
            last_advanced: None,
            post_advanced: false,
        }
    }
}

impl LockState {
    pub fn from_config(cfg: &StasisConfig) -> Self {
        // Find the first LockScreen action (there should usually be one)
        let lock_action = cfg.actions.iter().find(|a| a.kind == IdleAction::LockScreen);

        let command = lock_action.map(|a| a.command.clone());

        Self {
            is_locked: false,
            pid: None,
            command,
            last_advanced: None,
            post_advanced: false,
        }
    }
}

#[derive(Debug)]
pub struct ActiveFlags {
    pub pre_suspend_triggered: bool,
    pub brightness_captured: bool,
}

impl Default for ActiveFlags {
    fn default() -> Self {
        Self {
            pre_suspend_triggered: false,
            brightness_captured: false,
        }
    }
}

