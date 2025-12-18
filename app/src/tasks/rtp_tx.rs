use std::thread;
use std::time::Duration;

use crate::messages::{
    MediaOut, MediaOutReceiver, RtpTxCommandReceiver
};

use crate::tasks::task::{AppTask, TaskMeta};

pub struct RtpTxTask {
    cmd_rx: RtpTxCommandReceiver,
    media_rx: MediaOutReceiver,
}

impl AppTask for RtpTxTask {
    fn into_runner(mut self: Box<Self>) -> Box<dyn FnOnce() + Send + 'static> {
        Box::new(move || {
            self.run();
        })
    }

    fn meta(&self) -> TaskMeta {
        TaskMeta {
            name: "rtp_tx",
            stack_bytes: Some(16384),
        }
    }
}

impl RtpTxTask {
    pub fn new(
        cmd_rx: RtpTxCommandReceiver,
        media_rx: MediaOutReceiver,
    ) -> Self {
        Self {
            cmd_rx,
            media_rx,
        }
    }

    fn run(&mut self) {
        log::info!("RTP TX task started");

        loop {
            thread::sleep(Duration::from_secs(4));
        }
    }
}
