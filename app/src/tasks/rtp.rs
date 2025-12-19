use std::io::ErrorKind::WouldBlock;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::{Duration, Instant};

use rtp_audio::{encode_ulaw, RtpHeader, RtpPacket};

use crate::messages::{
    MediaIn, MediaInSender, MediaOut, MediaOutReceiver, RtpCommand, RtpCommandReceiver,
};
use crate::tasks::task::{AppTask, TaskMeta};

const RX_BUF_SIZE: usize = 1500;

pub struct RtpTask {
    socket: UdpSocket,

    cmd_rx: RtpCommandReceiver,
    media_in_tx: MediaInSender,
    media_out_rx: MediaOutReceiver,

    buf: [u8; RX_BUF_SIZE],

    active: bool,

    // Peer selection
    signaled_peer: Option<SocketAddr>,
    observed_peer: Option<SocketAddr>,

    // RX filtering / lock-on
    expected_remote_ssrc: Option<u32>,
    payload_type: Option<u8>,
    // Optional: if you want to be stricter, remember signaled IP and require it.
    signaled_ip: Option<std::net::IpAddr>,

    // TX state
    local_ssrc: u32,
    seq: u16,
    ts: u32,

    // Timing
    next_tick: Instant,
    tick: Duration,
    frame_samples: u32,
}

impl AppTask for RtpTask {
    fn into_runner(mut self: Box<Self>) -> Box<dyn FnOnce() + Send + 'static> {
        Box::new(move || self.run())
    }

    fn meta(&self) -> TaskMeta {
        TaskMeta {
            name: "rtp",
            stack_bytes: Some(16384),
        }
    }
}

impl RtpTask {
    pub fn new(
        socket: UdpSocket,
        cmd_rx: RtpCommandReceiver,
        media_in_tx: MediaInSender,
        media_out_rx: MediaOutReceiver,
    ) -> Self {
        let _ = socket.set_nonblocking(true);

        Self {
            socket,
            cmd_rx,
            media_in_tx,
            media_out_rx,
            buf: [0u8; RX_BUF_SIZE],

            active: false,

            signaled_peer: None,
            observed_peer: None,

            expected_remote_ssrc: None,
            payload_type: None,
            signaled_ip: None,

            local_ssrc: hardware::random_u32(),
            seq: 0,
            ts: 0,

            next_tick: Instant::now(),
            tick: Duration::from_millis(20),
            frame_samples: 160, // 20 ms @ 8 kHz (PCMU/PCMA/G.722 uses 8k RTP clock too)
        }
    }

    fn run(&mut self) {
        log::info!(
            "RTP task started on {}",
            self.socket
                .local_addr()
                .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap())
        );

        loop {
            if !self.poll_commands() {
                log::info!("RTP task exiting: command channel closed");
                break;
            }

            if self.active {
                self.poll_rx_socket();

                // Drive TX at a fixed cadence.
                let now = Instant::now();
                if now >= self.next_tick {
                    self.send_one();
                    while self.next_tick <= now {
                        self.next_tick += self.tick;
                    }
                }

                // Avoid spinning; RX is nonblocking.
                thread::sleep(Duration::from_millis(10));
            } else {
                thread::sleep(Duration::from_millis(50));
                self.next_tick = Instant::now() + self.tick;
            }
        }
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

    fn handle_command(&mut self, cmd: RtpCommand) {
        match cmd {
            RtpCommand::StartStream {
                remote_ip,
                remote_port,
                expected_remote_ssrc,
                local_ssrc,
                payload_type,
            } => {
                let addr_str = format!("{}:{}", remote_ip, remote_port);
                match addr_str.parse::<SocketAddr>() {
                    Ok(addr) => {
                        self.signaled_peer = Some(addr);
                        self.signaled_ip = Some(addr.ip());
                        self.observed_peer = None;

                        self.expected_remote_ssrc = expected_remote_ssrc;
                        self.payload_type = Some(payload_type);

                        if let Some(ssrc) = local_ssrc {
                            self.local_ssrc = ssrc;
                        } else {
                            self.local_ssrc = hardware::random_u32();
                        }

                        self.seq = 0;
                        self.ts = 0;

                        self.active = true;
                        self.next_tick = Instant::now() + self.tick;

                        log::info!(
                            "RTP start: signaled_peer={}, pt={}, expected_remote_ssrc={:?}, local_ssrc={}",
                            addr, payload_type, expected_remote_ssrc, self.local_ssrc
                        );
                    }
                    Err(e) => {
                        log::warn!("RTP start: invalid remote addr {} ({:?})", addr_str, e);
                    }
                }
            }
            RtpCommand::StopStream => {
                self.active = false;

                self.signaled_peer = None;
                self.observed_peer = None;

                self.expected_remote_ssrc = None;
                self.payload_type = None;
                self.signaled_ip = None;

                log::info!("RTP stopped");
            }
        }
    }

    fn poll_rx_socket(&mut self) {
        loop {
            match self.socket.recv_from(&mut self.buf) {
                Ok((len, addr)) => self.handle_rx_packet(len, addr),
                Err(ref e) if e.kind() == WouldBlock => break,
                Err(e) => {
                    log::warn!("RTP RX socket error: {:?}", e);
                    break;
                }
            }
        }
    }

    fn handle_rx_packet(&mut self, len: usize, addr: SocketAddr) {
        if len < 12 {
            return;
        }

        // Optional sanity: if we have a signaled IP, require the *IP* to match,
        // but DO NOT require the port to match (NAT).
        if let Some(ip) = self.signaled_ip {
            if addr.ip() != ip {
                return;
            }
        }

        let data = &self.buf[..len];

        let pkt = match RtpPacket::unpack(data) {
            Ok(p) => p,
            Err(_) => return,
        };

        // Filter on payload type (if set)
        if let Some(expected_pt) = self.payload_type {
            if pkt.header.payload_type != expected_pt {
                return;
            }
        }

        // SSRC lock-on / verify
        match self.expected_remote_ssrc {
            Some(expected) if expected != pkt.header.ssrc => return,
            None => {
                self.expected_remote_ssrc = Some(pkt.header.ssrc);
                log::info!("RTP RX: learned remote SSRC {}", pkt.header.ssrc);
            }
            _ => {}
        }

        // Now that we've accepted the stream, remember the observed tuple.
        // Observed outranks signaled for TX destination.
        if self.observed_peer != Some(addr) {
            self.observed_peer = Some(addr);
            log::info!("RTP peer (observed) -> {}", addr);
        }

        // Forward inbound packet to the audio/jitter/decoder pipeline.
        let _ = self.media_in_tx.send(MediaIn::RtpPcmuPacket(pkt));
    }

    fn send_one(&mut self) {
        let dest = self.observed_peer.or(self.signaled_peer);
        let dest = match dest {
            Some(d) => d,
            None => return,
        };

        // Pull a frame from media_out, or generate a tone for testing.
        let payload = self.build_payload();

        let header = RtpHeader {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type: self.payload_type.unwrap_or(0),
            sequence_number: self.seq,
            timestamp: self.ts,
            ssrc: self.local_ssrc,
        };

        let pkt: RtpPacket<512> = RtpPacket { header, payload };

        self.seq = self.seq.wrapping_add(1);
        self.ts = self.ts.wrapping_add(self.frame_samples);

        if let Ok(bytes) = pkt.pack() {
            let _ = self.socket.send_to(&bytes, dest);
        }
    }

    fn build_payload(&mut self) -> heapless::Vec<u8, 512> {
        match self.media_out_rx.try_recv() {
            Ok(MediaOut::PcmFrame(samples)) => {
                // For PCMU this is fine; for other codecs, this needs to be a
                // codec-specific encoder + frame sizing.
                encode_ulaw(&samples)
            }
            Err(TryRecvError::Empty) => self.tone_payload(),
            Err(TryRecvError::Disconnected) => {
                self.active = false;
                heapless::Vec::new()
            }
        }
    }

    fn tone_payload(&mut self) -> heapless::Vec<u8, 512> {
        const AMP: f32 = 8_000.0;
        const FREQ: f32 = 447.0;
        static mut PHASE: f32 = 0.0;

        let step = 2.0 * std::f32::consts::PI * FREQ / 8_000.0;

        // Generate one frame of PCM tone and encode to μ-law.
        let mut pcm = [0i16; 160];
        unsafe {
            for s in &mut pcm {
                *s = (PHASE.sin() * AMP) as i16;
                PHASE += step;
                if PHASE > 2.0 * std::f32::consts::PI {
                    PHASE -= 2.0 * std::f32::consts::PI;
                }
            }
        }

        encode_ulaw(&pcm)
    }
}
