use std::thread;
use std::time::Duration;

use log::debug;

/// Placeholder Wi-Fi maintenance task. Hardware init happens in `atom_echo_hw`,
/// this task just leaves a hook for future reconnection logic.
pub fn spawn_wifi_task() -> thread::JoinHandle<()> {
    thread::spawn(move || loop {
        debug!("wifi_task: tick");
        thread::sleep(Duration::from_secs(5));
    })
}
