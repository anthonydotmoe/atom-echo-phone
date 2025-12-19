use std::sync::mpsc::RecvTimeoutError;
use std::{sync::mpsc::TryRecvError, time::Instant};
use std::time::Duration;

use heapless::Vec as HVec;

use hardware::AudioDevice;
use rtp_audio::{decode_ulaw, JitterBuffer};
use crate::messages::{MediaOut, MediaOutSender};
use crate::{
    messages::{
        AudioCommand, AudioCommandReceiver, AudioMode,
        MediaIn, MediaInReceiver, PhoneState, RxRtpPacket
    },
    tasks::task::{AppTask, TaskMeta}
};


const FRAME_SAMPLES_8K: usize = 160; // 20ms at 8kHz
const FRAME_DURATION: Duration = Duration::from_millis(20);

type Jb = JitterBuffer<10, FRAME_SAMPLES_8K>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Personality {
    Idle,
    Listen,
    Talk,
}

pub struct AudioTask {
    cmd_rx: AudioCommandReceiver,
    audio_device: AudioDevice,
    media_rx: MediaInReceiver,
    media_tx: MediaOutSender,
    call_state: PhoneState,
    mode: AudioMode,
    persona: Personality,

    // Listen side
    jitter: Jb,
    next_playout_deadline: Option<Instant>,
    speaker_running: bool,

    // Talk side
    next_capture_deadline: Option<Instant>,
    inject_tone_as_mic: bool,
    mic_running: bool,
    
    // Tone generator
    tone_phase: f32,
}

impl AppTask for AudioTask {
    fn into_runner(mut self: Box<Self>) -> Box<dyn FnOnce() + Send + 'static> {
        Box::new(move || {
            self.run()
        })
    }

    fn meta(&self) -> TaskMeta {
        TaskMeta {
            name: "audio",
            stack_bytes: Some(16384),
        }
    }
}

impl AudioTask {
    pub fn new(
        cmd_rx: AudioCommandReceiver,
        audio_device: AudioDevice,
        media_rx: MediaInReceiver,
        media_tx: MediaOutSender,
    ) -> Self {
        Self {
            cmd_rx,
            audio_device,
            media_rx,
            media_tx,
            call_state: PhoneState::Idle,
            mode: AudioMode::Listen,
            persona: Personality::Idle,

            jitter: Jb::new(),
            next_playout_deadline: None,
            speaker_running: false,

            next_capture_deadline: None,
            inject_tone_as_mic: false,
            mic_running: false,

            tone_phase: 0.0,
        }
    }

    fn run(&mut self) {
        loop {
            // Drain commands (non-blocking)
            if !self.poll_commands() {
                break;
            }

            // Always drain inbound RTP media so jitter stays current
            self.poll_media();

            self.update_personality();

            match self.persona {
                Personality::Idle => {
                    // Block a bit so we don't spin, but keep checking commands
                    match self.cmd_rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(cmd) => self.handle_command(cmd),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }

                Personality::Listen => {
                    self.wait_until_next_deadline_or_command(self.next_playout_deadline);
                    self.maybe_playout_one_frame();
                }

                Personality::Talk => {
                    self.wait_until_next_deadline_or_command(self.next_capture_deadline);
                    self.maybe_capture_one_frame();
                }
            }
        }

        self.teardown_all();
    }

    fn poll_commands(&mut self) -> bool {
        loop {
            match self.cmd_rx.try_recv() {
                Ok(cmd) => self.handle_command(cmd),
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn handle_command(&mut self, cmd: AudioCommand) {
        match cmd {
            AudioCommand::SetDialogState(st) => {
                self.call_state = st;
                // If the call ends, clear jitter
                if !matches!(self.call_state, PhoneState::Established) {
                    self.jitter.reset();
                }
            }
            AudioCommand::SetMode(m) => {
                self.mode = m;
                // PTT toggles should not wipe jitter
            }
        }
    }

    fn poll_media(&mut self) {
        loop {
            match self.media_rx.try_recv() {
                Ok(MediaIn::RtpPcmuPacket(pkt)) => self.handle_rtp_pcmu(pkt),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    log::info!("audio: media_rx disconnected");
                    return;
                }
            }
        }
    }

    fn handle_rtp_pcmu(&mut self, pkt: RxRtpPacket) {
        let decoded: heapless::Vec<i16, 512> = decode_ulaw(&pkt.payload);
        self.jitter.push_frame(pkt.header.sequence_number, &decoded);
    }

    fn update_personality(&mut self) {
        let want = match (self.call_state, self.mode) {
            (PhoneState::Established, AudioMode::Listen) => Personality::Listen,
            (PhoneState::Established, AudioMode::Talk) => Personality::Talk,
            _ => Personality::Idle,
        };

        if want == self.persona {
            return;
        }

        // Transition
        self.teardown_all();

        self.persona = want;

        match self.persona {
            Personality::Idle => {
                log::info!("Switch to Personality::Idle");
            }

            Personality::Listen => {
                log::info!("Switch to Personality::Listen");
                self.start_speaker();
                self.next_playout_deadline = None;
            }

            Personality::Talk => {
                log::info!("Switch to Personality::Talk");
                self.start_mic();
                self.next_capture_deadline = None;
            }
        }
    }

    fn teardown_all(&mut self) {
        self.stop_speaker();
        self.stop_mic();

        self.next_playout_deadline = None;
        self.next_capture_deadline = None;
    }

    fn wait_until_next_deadline_or_command(&mut self, deadline: Option<Instant>) {
        let Some(deadline) = deadline else {
            // No schedule yet; return immediately.
            return;
        };

        let now = Instant::now();
        if now >= deadline {
            return;
        }

        // Sleep by blocking on the command queue with timeout
        // If a command arrives, handle it immediately
        let timeout = deadline - now;

        match self.cmd_rx.recv_timeout(timeout) {
            Ok(cmd) => {
                self.handle_command(cmd);
                // After a command, drain any queued commands so we're responsive
                let _ = self.poll_commands();
            }
            Err(RecvTimeoutError::Timeout) => {
                // deadline reached, return to let caller feed I2S
            }
            Err(RecvTimeoutError::Disconnected) => {
                // treat as shutdown
                self.persona = Personality::Idle;
            }
        }
    }

    // --- Listen personality: jitter -> I2S TX ---

    fn start_speaker(&mut self) {
        if self.speaker_running {
            return;
        }

        // Ensure TX exists and is primed/enabled
        if let Err(e) = self.audio_device.ensure_tx_ready() {
            log::warn!("ensure_tx_ready failed: {:?}", e);
            return;
        }

        // Prime & enable
        self.prime_dma_with_silence(3);
        if let Err(e) = self.audio_device.tx_enable() {
            log::warn!("tx_enable failed: {:?}", e);
            return;
        }

        self.speaker_running = true;
    }

    fn stop_speaker(&mut self) {
        if self.speaker_running {
            self.audio_device.stop_current();
            self.speaker_running = false;
        }
    }

    fn maybe_playout_one_frame(&mut self) {
        if !self.speaker_running {
            return;
        }

        let now = Instant::now();

        let Some(deadline) = self.next_playout_deadline else {
            // Initial buffering delay
            self.next_playout_deadline = Some(now + FRAME_DURATION);
            return;
        };

        if now < deadline {
            return;
        }
        self.next_playout_deadline = Some(deadline + FRAME_DURATION);

        let (frame, had_real) = self.jitter.pop_frame();
        log::debug!(
            "playout frame, real={}, first_sample={}",
            had_real,
            frame.get(0).copied().unwrap_or(0)
        );
        // frame is filled with samples or silence

        // Interleave to stereo
        let mut stereo = [0i16; FRAME_SAMPLES_8K * 2];
        for (i, s) in frame.iter().enumerate() {
            stereo[2 * i] = *s;
            stereo[2 * i + 1] = *s;
        }

        let bytes: &[u8] = bytemuck::cast_slice(&stereo);
        self.write_all(bytes);
    }

    fn write_all(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            // Short timeout to not block the thread forever if DMA is full
            match self.audio_device.write(data, Duration::from_millis(4)) {
                Ok(0) => {
                    // TX buffer full, try again later.
                    log::info!("F");
                    break;
                }
                Ok(n) => {
                    //log::trace!("n{}", n);
                    data = &data[n..];
                }
                Err(e) => {
                    log::warn!("write failed: {:?}", e);
                    break;
                }
            }
        }
    }

    fn prime_dma_with_silence(&mut self, frames: usize) {
        static SILENCE_STEREO: [i16; FRAME_SAMPLES_8K * 2] = [0i16; FRAME_SAMPLES_8K * 2];
        let bytes: &[u8] = bytemuck::cast_slice(&SILENCE_STEREO);

        for _ in 0..frames {
            match self.audio_device.preload_data(bytes) {
                Ok(written) if written == bytes.len() => {}
                Ok(written) => {
                    log::debug!(
                        "audio: preload buffer full after {} bytes (wanted {})",
                        written,
                        bytes.len()
                    );
                    break;
                }
                Err(e) => {
                    log::warn!("audio: preload_data failed: {:?}", e);
                    break;
                }
            }
        }
    }

    // --- Talk personality: mic -> MediaOut ---

    fn start_mic(&mut self) {
        if self.mic_running {
            return;
        }

        if let Err(e) = self.audio_device.ensure_rx_ready() {
            log::warn!("ensure_rx_ready failed: {:?}", e);
            return;
        }

        self.mic_running = true;
    }

    fn stop_mic(&mut self) {
        if self.mic_running {
            self.audio_device.stop_current();
            self.mic_running = false;
        }
    }

    fn maybe_capture_one_frame(&mut self) {
        if !self.mic_running {
            return;
        }

        let now = Instant::now();

        let Some(deadline) = self.next_capture_deadline else {
            self.next_capture_deadline = Some(now + FRAME_DURATION);
            return;
        };

        if now < deadline {
            return;
        }
        self.next_capture_deadline = Some(deadline + FRAME_DURATION);

        let frame = if self.inject_tone_as_mic {
            self.gen_tone_frame_8k()
        } else {
            self.capture_frame_8k_or_silence()
        };

        // Best-effort send; if RTP task can't keep up, oh well.
        let _ = self.media_tx.send(MediaOut::PcmFrame(frame));
    }

    fn capture_frame_8k_or_silence(&mut self) -> HVec<i16, FRAME_SAMPLES_8K> {
        let mut in16 = [0i16; 320];
        let mut out8 = HVec::new();
        let _ = out8.resize_default(FRAME_SAMPLES_8K);

        match self.audio_device.read(&mut in16, Duration::from_millis(25)) {
            Ok(nsamp) if nsamp >= 320 => {
                // average-pairs downsample
                for i in 0..160 {
                    let a = in16[2*i] as i32;
                    let b = in16[2*i + 1] as i32;
                    out8[i] = ((a + b) / 2) as i16;
                }
            }
            Ok(_short) => {}
            Err(e) => {
                log::warn!("mic read failed: {:?}", e);
            }
        }

        out8
    }

    fn gen_tone_frame_8k(&mut self) -> HVec<i16, FRAME_SAMPLES_8K> {
        use std::f32::consts::PI;
        const AMP: f32 = 8_000.0;
        const FREQ: f32 = 447.0;
        const SR: f32 = 8_000.0;

        let step = 2.0 * PI * FREQ / SR;

        let mut pcm = HVec::new();
        for _ in 0..FRAME_SAMPLES_8K {
            let s = (self.tone_phase.sin() * AMP) as i16;
            self.tone_phase += step;
            if self.tone_phase > 2.0 * PI {
                self.tone_phase -= 2.0 * PI;
            }
            let _ = pcm.push(s);
        }
        pcm
    }
}
