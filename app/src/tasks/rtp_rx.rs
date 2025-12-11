use std::io::ErrorKind::WouldBlock;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::Duration;

use rtp_audio::RtpPacket;

use crate::messages::{MediaIn, MediaInSender, RtpRxCommand, RtpRxCommandReceiver};

const RX_BUF_SIZE: usize = 1500;

/// Spawn the RTP RX task. Owns the UDP socket bound to our advertised RTP port,
/// listens for inbound RTP, filters on SSRC/payload type/remote addr, and
/// forwards accepted packets to the audio pipeline as `MediaIn::EncodedRtpPacket`.
pub fn spawn_rtp_rx_task(
    socket: UdpSocket,
    cmd_rx: RtpRxCommandReceiver,
    media_tx: MediaInSender,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("rtp-rx".into())
        .spawn(move || {
            let mut task = RtpRxTask::new(socket, cmd_rx, media_tx);
            task.run();
        })
        .expect("failed to spawn RTP RX task")
}

struct RtpRxTask {
    socket: UdpSocket,
    cmd_rx: RtpRxCommandReceiver,
    media_tx: MediaInSender,
    buf: [u8; RX_BUF_SIZE],

    active: bool,
    expected_ssrc: Option<u32>,
    payload_type: Option<u8>,
    remote_addr: Option<SocketAddr>,
}

impl RtpRxTask {
    fn new(
        socket: UdpSocket,
        cmd_rx: RtpRxCommandReceiver,
        media_tx: MediaInSender,
    ) -> Self {
        // Best-effort: if this fails we'll just block in recv_from.
        let _ = socket.set_nonblocking(true);

        Self {
            socket,
            cmd_rx,
            media_tx,
            buf: [0u8; RX_BUF_SIZE],
            active: false,
            expected_ssrc: None,
            payload_type: None,
            remote_addr: None,
        }
    }

    fn run(&mut self) {
        log::info!("RTP RX task started on {}", self.socket.local_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap()));

        loop {
            if !self.poll_commands() {
                log::info!("RTP RX task exiting: command channel closed");
                break;
            }

            if self.active {
                self.poll_socket();
            }

            thread::sleep(Duration::from_millis(10));
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

    fn handle_command(&mut self, cmd: RtpRxCommand) {
        match cmd {
            RtpRxCommand::StartStream {
                remote_ip,
                remote_port,
                expected_ssrc,
                payload_type,
            } => {
                let addr_str = format!("{}:{}", remote_ip, remote_port);
                match addr_str.parse::<SocketAddr>() {
                    Ok(addr) => {
                        self.remote_addr = Some(addr);
                        self.expected_ssrc = expected_ssrc;
                        self.payload_type = Some(payload_type);
                        self.active = true;
                        log::info!(
                            "RTP RX start: remote={}, pt={}, expected_ssrc={:?}",
                            addr,
                            payload_type,
                            expected_ssrc
                        );
                    }
                    Err(e) => {
                        log::warn!("RTP RX start: invalid remote addr {} ({:?})", addr_str, e);
                    }
                }
            }
            RtpRxCommand::StopStream => {
                self.active = false;
                self.expected_ssrc = None;
                self.payload_type = None;
                self.remote_addr = None;
                log::info!("RTP RX stopped");
            }
        }
    }

    fn poll_socket(&mut self) {
        loop {
            match self.socket.recv_from(&mut self.buf) {
                Ok((len, addr)) => {
                    self.handle_packet(len, addr);
                }
                Err(ref e) if e.kind() == WouldBlock => break,
                Err(e) => {
                    log::warn!("RTP RX socket error: {:?}", e);
                    break;
                }
            }
        }
    }

    fn handle_packet(&mut self, len: usize, addr: SocketAddr) {
        if len < 12 {
            return;
        }

        if let Some(expected) = self.remote_addr {
            if expected.ip() != addr.ip() || expected.port() != addr.port() {
                log::debug!("RTP RX: ignoring packet from unexpected {}", addr);
                return;
            }
        }

        let data = &self.buf[..len];

        let pkt = match RtpPacket::unpack(data) {
            Ok(p) => p,
            Err(e) => {
                log::debug!("RTP RX: unpack failed: {:?}", e);
                return;
            }
        };

        // Filter on payload type
        if let Some(expected_pt) = self.payload_type {
            if pkt.header.payload_type != expected_pt {
                log::debug!(
                    "RTP RX: dropping packet with unexpected PT {} (expected {})",
                    pkt.header.payload_type,
                    expected_pt
                );
                return;
            }
        }

        match self.expected_ssrc {
            Some(expected_ssrc) if expected_ssrc != pkt.header.ssrc => {
                log::debug!(
                    "RTP RX: dropping packet with unexpected SSRC {} (expected {})",
                    pkt.header.ssrc,
                    expected_ssrc
                );
                return;
            }
            None => {
                // Lock on to the first SSRC we see if none was provided.
                self.expected_ssrc = Some(pkt.header.ssrc);
                log::info!("RTP RX: learned remote SSRC {}", pkt.header.ssrc);
            }
            _ => {}
        }

        if pkt.payload.len() > 512 {
            log::warn!(
                "RTP RX: packet too large ({} bytes), dropping",
                pkt.payload.len()
            );
            return;
        }

        log::debug!(
            "received packet: seq={}, ts={}, payload_len={}",
            pkt.header.sequence_number,
            pkt.header.timestamp,
            pkt.payload.len()
        );

        if let Err(e) = self.media_tx.send(MediaIn::RtpPcmuPacket(pkt)) {
            log::warn!("RTP RX: failed to forward packet to audio: {:?}", e);
        }
    }
}
