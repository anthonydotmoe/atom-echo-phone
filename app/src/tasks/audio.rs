use std::sync::mpsc::RecvTimeoutError;
use std::{sync::mpsc::TryRecvError, time::Instant};
use std::time::Duration;

use heapless::Vec as HVec;

use hardware::AudioDevice;
use rtp_audio::{decode_ulaw, JitterBuffer};
use crate::messages::{MediaOut, MediaOutSender};
use crate::agc::Agc;
use crate::dsp::Up6Polyphase;
use crate::{
    messages::{
        AudioCommand, AudioCommandReceiver, AudioMode,
        MediaIn, MediaInReceiver, PhoneState, RxRtpPacket
    },
    tasks::task::{AppTask, TaskMeta}
};


const FRAME_SAMPLES_8K: usize = 160; // 20ms at 8kHz
const FRAME_SAMPLES_48K: usize = 960; // 20ms at 48kHz
const FRAME_DURATION: Duration = Duration::from_millis(20);

type Jb = JitterBuffer<10, FRAME_SAMPLES_8K>;

#[derive(Debug, Clone, Copy)]
enum Engine {
    Off,
    Listen { next: Option<Instant> },
    Talk { next: Option<Instant> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineKind {
    Off,
    Listen,
    Talk
}

impl Engine {
    const fn kind(&self) -> EngineKind {
        match self {
            Engine::Off => EngineKind::Off,
            Engine::Listen { .. } => EngineKind::Listen,
            Engine::Talk { .. } => EngineKind::Talk,
        }
    }
}

pub struct AudioTask {
    cmd_rx: AudioCommandReceiver,
    audio_device: AudioDevice,
    media_rx: MediaInReceiver,
    media_tx: MediaOutSender,
    call_state: PhoneState,
    mode: AudioMode,
    engine: Engine,

    // Listen side
    jitter: Jb,
    up6: Up6Polyphase,

    // Talk side
    inject_tone_as_mic: bool,
    agc: Agc,
    
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
            engine: Engine::Off,

            jitter: Jb::new(),
            up6: Up6Polyphase::new(),

            inject_tone_as_mic: false,
            agc: Agc::new(),

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

            self.update_engine();

            match self.engine {
                Engine::Off => {
                    // Block a bit so we don't spin, but keep checking commands
                    match self.cmd_rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(cmd) => self.handle_command(cmd),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }

                Engine::Listen { next } => {
                    self.wait_until_next_deadline_or_command(next);
                    self.maybe_playout_one_frame();
                }

                Engine::Talk{ next } => {
                    self.wait_until_next_deadline_or_command(next);
                    self.maybe_capture_one_frame();
                }
            }
        }

        self.stop_engine();
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

    fn update_engine(&mut self) {
        let want = match (self.call_state, self.mode) {
            (PhoneState::Established, AudioMode::Listen) => EngineKind::Listen,
            (PhoneState::Established, AudioMode::Talk) => EngineKind::Talk,
            _ => EngineKind::Off,
        };

        if self.engine.kind() == want {
            return;
        }

        // Transition
        self.stop_engine();
        self.start_engine(want);
    }

    fn start_engine(&mut self, want: EngineKind) {
        match want {
            EngineKind::Off => self.engine = Engine::Off,

            EngineKind::Listen => {
                if self.audio_device.ensure_tx_ready().is_ok()
                {
                    self.prime_dma_with_silence(3);
                    if let Err(e) = self.audio_device.tx_enable() {
                        log::warn!("tx_enable failed: {:?}", e);
                        self.engine = Engine::Off;
                        return;
                    }
                    self.engine = Engine::Listen { next: None };
                } else {
                    self.engine = Engine::Off;
                }
            }

            EngineKind::Talk => {
                if self.audio_device.ensure_rx_ready().is_ok()
                {
                    self.engine = Engine::Talk { next: None };
                } else {
                    self.engine = Engine::Off;
                }
            }
        }
    }

    fn stop_engine(&mut self) {
        match self.engine {
            Engine::Listen { .. } => {
                self.audio_device.stop_current();
            }
            Engine::Talk { .. } => {
                self.audio_device.stop_current();
            }
            Engine::Off => {}
        }
        self.engine = Engine::Off;
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
                self.engine = Engine::Off;
            }
        }
    }

    // --- Listen personality: jitter -> I2S TX ---
    fn maybe_playout_one_frame(&mut self) {
        let Engine::Listen { next } = self.engine else {
            return;
        };

        let now = Instant::now();

        let Some(deadline) = next else {
            // Initial buffering delay
            self.engine = Engine::Listen { next: Some(now + FRAME_DURATION) };
            return;
        };

        if now < deadline {
            return;
        }
        self.engine = Engine::Listen{ next: Some(deadline + FRAME_DURATION) };

        let (frame, had_real) = self.jitter.pop_frame();
        log::debug!(
            "playout frame, real={}, first_sample={}",
            had_real,
            frame.get(0).copied().unwrap_or(0)
        );
        // frame is filled with samples or silence

        // TODO: potentially ugly copy?
        let frame_as_array_160 = {
            let mut f = [0i16; FRAME_SAMPLES_8K];
            for (i, s) in frame.iter().enumerate() {
                f[i] = *s;
            }
            f
        };

        let mut out_mono_48k = [0i16; FRAME_SAMPLES_48K];
        self.up6.process_frame(&frame_as_array_160, &mut out_mono_48k);

        // Interleave to stereo and gain
        let mut stereo = [0i16; FRAME_SAMPLES_48K * 2];
        for (i, s) in out_mono_48k.iter().enumerate() {
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
        static SILENCE_STEREO: [i16; FRAME_SAMPLES_48K * 2] = [0i16; FRAME_SAMPLES_48K * 2];
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
    fn maybe_capture_one_frame(&mut self) {
        let Engine::Talk { next } = self.engine else {
            return;
        };

        let now = Instant::now();

        let Some(deadline) = next else {
            self.engine = Engine::Talk { next: Some(now + FRAME_DURATION) };
            return;
        };

        if now < deadline {
            return;
        }
        self.engine = Engine::Talk { next: Some(deadline + FRAME_DURATION) };

        let mut frame = if self.inject_tone_as_mic {
            self.gen_tone_frame_8k()
        } else {
            self.capture_frame_8k_or_silence()
        };

        let (gain_q12, rms) = self.agc.process_frame(frame.as_mut_slice());
        log::info!("agc gain_q12={} rms={}", gain_q12, rms);

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
