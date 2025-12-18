use std::net::UdpSocket;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use hardware::{Device, WifiConfig};
use log::info;
use thiserror::Error;

use crate::tasks::{
    audio::AudioTask,
    rtp_rx::RtpRxTask,
    sip::SipTask,
    task::{start_all, AppTask},
    ui::UiTask,
};

mod messages;
mod settings;
mod tasks;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("hardware error: {0}")]
    Hardware(String),
    #[error("sip error: {0}")]
    Sip(String),
}

pub fn run() -> Result<(), AppError> {
    info!("starting Atom Echo phone runtime");

    let wifi_config = WifiConfig::new(
        settings::SETTINGS.wifi_ssid,
        settings::SETTINGS.wifi_password,
        settings::SETTINGS.wifi_username,
    )
    .map_err(|err| AppError::Hardware(format!("{err:?}")))?;

    let mut device =
        Device::init(wifi_config).map_err(|err| AppError::Hardware(format!("{err:?}")))?;

    // Split device
    let ui_device = device.get_ui_device().unwrap();
    let audio_device = device.get_audio_device().unwrap();

    let addr = device.get_ip_addr();

    let rtp_socket = UdpSocket::bind((addr, 0)).map_err(|err| AppError::Sip(format!("{err:?}")))?;
    let _ = rtp_socket.set_nonblocking(true);
    let local_rtp_port = rtp_socket
        .local_addr()
        .map(|addr| addr.port())
        .unwrap_or(10_000);

    log::info!("rtp_socket.local_addr(): {:?}", rtp_socket.local_addr());

    // Create channels
    let (sip_tx, sip_rx) = channel::<messages::SipCommand>();
    let (audio_tx, audio_rx) = channel::<messages::AudioCommand>();
    let (rtp_tx_tx, _rtp_tx_rx) = channel::<messages::RtpTxCommand>();
    let (rtp_rx_tx, rtp_rx_rx) = channel::<messages::RtpRxCommand>();
    let (ui_tx, ui_rx) = channel::<messages::UiCommand>();
    let (media_in_tx, media_in_rx) = channel::<messages::MediaIn>();
    let (_media_out_tx, _media_out_rx) = channel::<messages::MediaOut>();

    let ui_task = Box::new(UiTask::new(ui_device, ui_rx, sip_tx));

    let rtp_rx_task = Box::new(RtpRxTask::new(rtp_socket, rtp_rx_rx, media_in_tx));

    let sip_task = Box::new(SipTask::new(
        &settings::SETTINGS,
        addr,
        local_rtp_port,
        sip_rx,
        ui_tx,
        audio_tx,
        rtp_tx_tx,
        rtp_rx_tx,
    ));

    let audio_task = Box::new(AudioTask::new(audio_rx, audio_device, media_in_rx));

    let tasks: Vec<Box<dyn AppTask>> = vec![audio_task, ui_task, rtp_rx_task, sip_task];

    start_all(tasks);

    #[cfg(target_os = "espidf")]
    esp_specific::idle_loop();

    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(target_os = "espidf")]
mod esp_specific {
    use crate::settings;
    use esp_idf_svc::sys;
    use std::fmt::Write;
    use std::{
        collections::{BTreeMap, BTreeSet},
        ffi::CStr,
        thread,
        time::{Duration, Instant},
    };

    const USER_TASK_NAMES: &[&str] = &["audio", "rtp_rx", "sip", "ui"];
    const RUNTIME_STATS_INTERVAL: Duration = Duration::from_secs(10);
    const STACK_WATERMARK_REFRESH: Duration = Duration::from_secs(30);

    pub fn idle_loop() {
        let mut stats_logger = TaskStatsLogger::new(
            settings::SETTINGS.task_stats,
            STACK_WATERMARK_REFRESH,
            USER_TASK_NAMES,
        );

        loop {
            thread::sleep(RUNTIME_STATS_INTERVAL);

            stats_logger.maybe_log();
        }
    }

    #[derive(Debug)]
    enum TaskStatsError {
        NoTasks,
        TotalRunTimeZero,
    }

    struct TaskStatsLogger {
        enabled: bool,
        stack_cache: BTreeMap<usize, usize>,
        last_stack_refresh: Instant,
        stack_refresh_every: Duration,
        user_tasks: BTreeSet<&'static str>,
    }

    struct TaskRow {
        name: String,
        run_time_ticks: u64,
        percent: f32,
        stack_bytes: Option<usize>,
        is_user_task: bool,
    }

    impl TaskStatsLogger {
        fn new(
            enabled: bool,
            stack_refresh_every: Duration,
            user_task_names: &[&'static str],
        ) -> Self {
            Self {
                enabled,
                stack_cache: BTreeMap::new(),
                last_stack_refresh: Instant::now(),
                stack_refresh_every,
                user_tasks: user_task_names.iter().copied().collect(),
            }
        }

        fn maybe_log(&mut self) {
            if !self.enabled {
                return;
            }

            let refresh_stack = self.last_stack_refresh.elapsed() >= self.stack_refresh_every;

            match self.snapshot(refresh_stack) {
                Ok(s) => log::info!("runtime stats:\n{s}"),
                Err(e) => log::warn!("run-time stats error: {:?}", e),
            }

            if refresh_stack {
                self.last_stack_refresh = Instant::now();
            }
        }

        fn snapshot(&mut self, refresh_stack: bool) -> Result<String, TaskStatsError> {
            let task_count = unsafe { sys::uxTaskGetNumberOfTasks() } as usize;
            if task_count == 0 {
                return Err(TaskStatsError::NoTasks);
            }

            // Allow a couple of extra slots in case a task is created between calls.
            let mut statuses: Vec<sys::TaskStatus_t> = (0..task_count + 4)
                .map(|_| unsafe { core::mem::zeroed() })
                .collect();

            let mut total_run_time = 0;
            let filled = unsafe {
                sys::uxTaskGetSystemState(
                    statuses.as_mut_ptr(),
                    statuses.len() as u32,
                    &mut total_run_time,
                )
            } as usize;
            statuses.truncate(filled);

            if statuses.is_empty() {
                return Err(TaskStatsError::NoTasks);
            }

            if total_run_time == 0 {
                return Err(TaskStatsError::TotalRunTimeZero);
            }

            let total_run_time = total_run_time as u64;
            let mut rows = Vec::with_capacity(statuses.len());
            let mut live_handles = BTreeSet::new();

            for status in statuses.into_iter() {
                let handle_key = status.xHandle as usize;
                live_handles.insert(handle_key);

                let name = unsafe {
                    CStr::from_ptr(status.pcTaskName)
                        .to_string_lossy()
                        .into_owned()
                };

                let stack_bytes = self.stack_high_water_bytes(
                    status.xHandle,
                    status.eCurrentState,
                    refresh_stack,
                );
                let run_time_ticks = status.ulRunTimeCounter as u64;
                let percent = (run_time_ticks as f32 / total_run_time as f32) * 100.0;
                let is_user_task = self.user_tasks.contains(name.as_str());

                rows.push(TaskRow {
                    name,
                    run_time_ticks,
                    percent,
                    stack_bytes,
                    is_user_task,
                });
            }

            self.stack_cache
                .retain(|handle, _| live_handles.contains(handle));

            rows.sort_by(|a, b| match (a.is_user_task, b.is_user_task) {
                (true, false) => core::cmp::Ordering::Less,
                (false, true) => core::cmp::Ordering::Greater,
                _ => a.name.cmp(&b.name),
            });

            Ok(format_rows(rows))
        }

        fn stack_high_water_bytes(
            &mut self,
            handle: sys::TaskHandle_t,
            state_hint: sys::eTaskState,
            refresh: bool,
        ) -> Option<usize> {
            let key = handle as usize;
            let needs_refresh = refresh || !self.stack_cache.contains_key(&key);

            if needs_refresh {
                let mut info: sys::TaskStatus_t = unsafe { core::mem::zeroed() };
                unsafe {
                    sys::vTaskGetInfo(
                        handle, &mut info, 1, // xGetFreeStackSpace
                        state_hint,
                    );
                }

                let bytes =
                    info.usStackHighWaterMark as usize * core::mem::size_of::<sys::StackType_t>();
                self.stack_cache.insert(key, bytes);
            }

            self.stack_cache.get(&key).copied()
        }
    }

    fn format_rows(rows: Vec<TaskRow>) -> String {
        let name_width = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);

        let mut out = String::new();
        let header = format!(
            "{:<name_width$} {:>12} {:>8} {:>12}",
            "task",
            "time",
            "% time",
            "free stack",
            name_width = name_width,
        );
        let separator = "-".repeat(header.len());

        let _ = writeln!(out, "{header}");
        let _ = writeln!(out, "{separator}");

        let mut in_system_section = false;
        for row in rows {
            if !row.is_user_task && !in_system_section {
                let _ = writeln!(out, "{separator}");
                in_system_section = true;
            }

            let stack_str = row
                .stack_bytes
                .map(|b| format!("{}B", b))
                .unwrap_or_else(|| "-".to_string());

            let _ = writeln!(
                out,
                "{:<name_width$} {:>12} {:>7.2}% {:>12}",
                row.name,
                row.run_time_ticks,
                row.percent,
                stack_str,
                name_width = name_width,
            );
        }

        out
    }
}
