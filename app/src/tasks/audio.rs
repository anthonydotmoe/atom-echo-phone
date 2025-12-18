use std::sync::mpsc::RecvTimeoutError;
use std::{sync::mpsc::TryRecvError, time::Instant};
use std::time::Duration;

use hardware::AudioDevice;
use rtp_audio::{decode_ulaw, JitterBuffer};
use crate::{
    messages::{
        AudioCommand, AudioCommandReceiver, AudioMode,
        MediaIn, MediaInReceiver, PhoneState, RxRtpPacket
    },
    tasks::task::{AppTask, TaskMeta}
};

static SILENCE_FRAME: [i16; FRAME_SAMPLES * 2] = [0i16; FRAME_SAMPLES * 2];

const FRAME_SAMPLES: usize = 160; // 20ms at 8kHz
const FRAME_DURATION: Duration = Duration::from_millis(20);

type Jb = JitterBuffer<10, FRAME_SAMPLES>; // 10 frames = 200ms

enum TxState {
    Stopped,
    Ready,
    Running,
}

pub struct AudioTask {
    cmd_rx: AudioCommandReceiver,
    audio_device: AudioDevice,
    media_rx: MediaInReceiver,
    call_state: PhoneState,
    mode: AudioMode,
    playing: bool,
    tx_state: TxState,

    jitter: Jb,
    next_playout_deadline: Option<std::time::Instant>,
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
    ) -> Self {
        Self {
            cmd_rx,
            audio_device,
            media_rx,
            call_state: PhoneState::Idle,
            mode: AudioMode::Listen,
            playing: false,
            tx_state: TxState::Ready,
            jitter: Jb::new(),
            next_playout_deadline: None,
        }
    }

    fn run(&mut self) {
        loop {
            // Drain commands (non-blocking)
            if !self.poll_commands() {
                break;
            }

            // Always drain media so RTP doesn't back up when muted.
            self.poll_media();

            if self.playing {
                // Wait until its time to play the next frame, but wake up
                // periodically so commands can be handled even if no media arrives
                self.wait_until_next_playout_or_command();
                self.maybe_feed_i2s();
            } else {
                // Idle: block briefly for commands so we don't spin, but still
                // re-check regularly to drain media.
                match self.cmd_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(cmd) => self.handle_command(cmd),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        }

        self.stop_tx()
    }

    fn wait_until_next_playout_or_command(&mut self) {
        // If we don't have a schedule yet, let `maybe_feed_i2s` init it.
        let Some(deadline) = self.next_playout_deadline else {
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
                self.playing = false;
            }
        }
    }

    fn maybe_feed_i2s(&mut self) {
        use std::time::Instant;

        let now = Instant::now();

        // Initialize playout schedule once we know we should start
        if self.next_playout_deadline.is_none() {
            // small initial buffering delay (one frame)
            self.next_playout_deadline = Some(now + FRAME_DURATION);
            return;
        }

        let deadline = self.next_playout_deadline.unwrap();

        if now < deadline {
            // Not time yet, come back later
            return;
        }

        // We might have slipped a bit; schedule the *next* playout before doing work
        self.next_playout_deadline = Some(deadline + FRAME_DURATION);

        // Now pop exactly one frame from jitter and feed I2S
        self.feed_i2s();
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
            AudioCommand::SetDialogState(p) => {
                self.handle_dialog_state(p);
            }
            AudioCommand::SetMode(mode) => {
                self.handle_mode_change(mode);
            }
        }
    }

    fn handle_dialog_state(&mut self, phone_state: PhoneState) {
        self.call_state = phone_state;

        // Clear jitter when the call ends or before a new one starts.
        let clear_jitter = !matches!(self.call_state, PhoneState::Established);
        self.update_output_mode(clear_jitter);
    }

    fn handle_mode_change(&mut self, mode: AudioMode) {
        self.mode = mode;

        // PTT toggles should not wipe buffered audio.
        self.update_output_mode(false);
    }

    fn poll_media(&mut self) {
        loop {
            match self.media_rx.try_recv() {
                Ok(MediaIn::RtpPcmuPacket(pkt)) => {
                    self.handle_rtp_pcmu(pkt);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    log::info!("audio: media_rx disconnected");
                    return;
                }
            }
        }
    }

    fn handle_rtp_pcmu(&mut self, pkt: RxRtpPacket) {
        // PCMU payload is μ-law bytes
        let decoded: heapless::Vec<i16, 512> = decode_ulaw(&pkt.payload);

        // Push into jitter buffer based on sequence number
        self.jitter
            .push_frame(pkt.header.sequence_number, &decoded);
    }

    fn feed_i2s(&mut self) {
        if !matches!(self.tx_state, TxState::Running) {
            return;
        }

        // One frame = 20ms
        let (frame, had_real) = self.jitter.pop_frame();
        log::debug!(
            "playout frame, real={}, first_sample={}",
            had_real,
            frame.get(0).copied().unwrap_or(0)
        );
        // frame is filled with samples or silence

        // Interleave to stereo
        let mut stereo = [0i16; FRAME_SAMPLES * 2];
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

    fn start_playback(&mut self) {
        if self.playing {
            return;
        }

        // Reset playout scheduling so a new call does not reuse an old deadline.
        self.next_playout_deadline = None;
        self.playing = true;
        self.start_tx();
    }

    fn start_tx(&mut self) {
        // Try to get back to READY no matter what previous state was.
        match self.tx_state {
            TxState::Running => {
                let _ = self.audio_device.tx_disable();
                self.tx_state = TxState::Stopped;
            }
            _ => {}
        }

        if let Err(e) = self.audio_device.ensure_tx_ready() {
            log::warn!("audio: ensure_tx_ready failed: {:?}", e);
            self.tx_state = TxState::Stopped;
            return;
        }

        // At this point the underlying channel should be READY
        // or at least in a state where preload is allowed.
        self.tx_state = TxState::Ready;

        // Prime DMA with a few silence frames so when we enable, the line is clean
        self.prime_dma_with_silence(3);

        // Now enable TX
        if let Err(e) = self.audio_device.tx_enable() {
            log::warn!("audio: tx_enable failed: {:?}", e);
            // bail out; leave state as Ready so another Start might try again.
            return;
        }
        self.tx_state = TxState::Running;
    }

    fn stop_tx(&mut self) {
        if matches!(self.tx_state, TxState::Running) {
            // Drop the TX driver entirely for half-duplex PTT; dropping handles
            // disabling internally.
            self.audio_device.drop_tx();
        }
        self.tx_state = TxState::Stopped;
    }

    fn stop_playback(&mut self, clear_jitter: bool) {
        if self.playing {
            log::info!("stop playback");
        }
        self.playing = false;
        self.next_playout_deadline = None;
        self.stop_tx();
        if clear_jitter {
            self.jitter.reset();
        }
    }

    /// Decide whether to feed the speaker based on call state and current PTT mode.
    fn update_output_mode(&mut self, clear_jitter: bool) {
        match (self.call_state, self.mode) {
            (PhoneState::Established, AudioMode::Listen) => self.start_playback(),
            _ => self.stop_playback(clear_jitter),
        }
    }

    fn prime_dma_with_silence(&mut self, frames: usize) {
        let bytes: &[u8] = bytemuck::cast_slice(&SILENCE_FRAME);

        for _ in 0..frames {
            if let TxState::Ready = self.tx_state {
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
            } else {
                break;
            }
        }
    }
}

pub fn audio_test(mut audio: AudioDevice) -> ! {
    use std::f32::consts::PI;
    audio.tx_enable().unwrap();

    let frame_count = 100;

    const SR: u32 = 48_000;
    const FRAME: usize = 160; // 20 ms
    const F_TONE: f32 = 447.0;

    let mut phase: f32 = 0.0;
    let step = 2.0 * PI * F_TONE / SR as f32;

    loop {
        let mut frame_mono = [0i16; FRAME];
        for s in &mut frame_mono {
            *s = (phase.sin() * 8000.0) as i16;
            phase += step;
            if phase > 2.0 * PI {
                phase -= 2.0 * PI;
            }
        }

        // Duplicate to stereo
        let mut stereo = [0i16; FRAME * 2];
        for (i, &s) in frame_mono.iter().enumerate() {
            let idx = i * 2;
            stereo[idx] = s;
            stereo[idx + 1] = s;
        }

        let bytes: &[u8] = bytemuck::cast_slice(&stereo);
        let _ = audio.write(bytes, Duration::from_millis(50)).unwrap();
        //frame_count -= 1;

        if frame_count == 0 {
            break;
        }
    }

    let _ = audio.tx_disable();
    log::debug!("DONE");

    loop {}
}
