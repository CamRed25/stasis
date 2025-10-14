use std::{sync::Arc, time::Duration};
use eyre::Result;
use mpris::{PlayerFinder, PlaybackStatus};
use tokio::{task, time};

use crate::core::timer::IdleTimer;
use crate::log::log_error_message;

const IGNORED_PLAYERS: &[&str] = &[
    "KDE Connect",
    "kdeconnect",
    "Chromecast",
    "chromecast",
    "Spotify Connect",
    "spotifyd",
    "vlc-http",
    "plexamp",
    "snapcast",
    "bluez",
];

/// Setup MPRIS monitoring using a Tokio task
pub fn spawn_media_monitor(
    idle_timer: Arc<tokio::sync::Mutex<IdleTimer>>,
    ignore_remote_media: bool,
) -> Result<()> {
    let idle_timer_clone = Arc::clone(&idle_timer);
    let interval = Duration::from_secs(2);

    task::spawn(async move {
        let mut ticker = time::interval(interval);
        let mut media_playing = false;

        loop {
            ticker.tick().await;

            let any_playing = match PlayerFinder::new() {
                Ok(finder) => match finder.find_all() {
                    Ok(players) => players.iter().any(|player| {
                        let identity = player.identity();
                        let bus_name = player.bus_name().to_string();

                        // Only apply ignore list if enabled
                        if ignore_remote_media {
                            if IGNORED_PLAYERS
                                .iter()
                                .any(|s| identity.contains(s) || bus_name.contains(s))
                            {
                                return false;
                            }
                        }

                        player
                            .get_playback_status()
                            .map(|s| s == PlaybackStatus::Playing)
                            .unwrap_or(false)
                    }),
                    Err(e) => {
                        log_error_message(&format!("MPRIS: failed to list players: {:?}", e));
                        false
                    }
                },
                Err(e) => {
                    log_error_message(&format!("MPRIS: failed to create finder: {:?}", e));
                    false
                }
            };

            let mut timer = idle_timer_clone.lock().await;
            if any_playing && !media_playing {
                timer.pause(false);
                media_playing = true;
            } else if !any_playing && media_playing {
                timer.resume(false);
                media_playing = false;
            }
        }
    });

    Ok(())
}
