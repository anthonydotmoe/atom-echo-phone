use std::thread;
use std::time::Duration;

use std::net::UdpSocket;

use heapless::String as HString;
use log::{debug, warn};

use rtp_audio::{encode_ulaw, RtpHeader, RtpPacket};
use sdp::SessionDescription;
use sip_core::{DialogState, SipStack};

use crate::messages::{AudioCommand, AudioCommandSender, SipCommand, SipCommandReceiver};

pub fn spawn_sip_task(
    settings: &'static crate::settings::Settings,
    sip_rx: SipCommandReceiver,
    audio_tx: AudioCommandSender,
) -> thread::JoinHandle<()> {
    thread::spawn(move || sip_loop(settings, sip_rx, audio_tx))
}

fn sip_loop(
    settings: &'static crate::settings::Settings,
    sip_rx: SipCommandReceiver,
    audio_tx: AudioCommandSender,
) {
    let mut sip = SipStack::default();
    let mut dialog_state = DialogState::Idle;
    let mut remote_rtp: Option<(HString<48>, u16)> = None;
    let local_rtp_port;
    let mut seq: u16 = 1;
    let mut timestamp: u32 = 0;
    let registrar = parse_uri(settings.sip_registrar);
    let target = parse_uri(settings.sip_target);

    let sip_socket = UdpSocket::bind("0.0.0.0:0").expect("create SIP socket");
    sip_socket
        .set_nonblocking(true)
        .expect("set nonblocking");

    let rtp_socket = UdpSocket::bind("0.0.0.0:0").expect("create RTP socket");
    local_rtp_port = rtp_socket
        .local_addr()
        .map(|addr| addr.port())
        .unwrap_or(10_000);

    // Stub registration
    if let Ok(req) = sip.register(settings.sip_contact, settings.sip_contact) {
        let rendered = req.render().ok();
        if let Some(body) = &rendered {
            debug!("sending REGISTER:\n{}", body);
        }
        send_sip(&sip_socket, &registrar, &req.render().unwrap());
        // assume success for now
        sip.on_register_response(200);
    }

    loop {
        // Handle inbound SIP packets
        let mut buf = [0u8; 1500];
        match sip_socket.recv_from(&mut buf) {
            Ok((len, addr)) => {
                if let Ok(text) = core::str::from_utf8(&buf[..len]) {
                    if let Ok(msg) = sip_core::parse_message(text) {
                        if let Some((ip, port)) = handle_incoming_sip(msg, &audio_tx) {
                            remote_rtp = Some((ip, port));
                            dialog_state = DialogState::Established;
                        }
                    } else {
                        warn!("failed to parse SIP from {addr}");
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => warn!("SIP recv error: {:?}", e),
        }

        // Handle commands from hardware
        match sip_rx.try_recv() {
            Ok(cmd) => match cmd {
                SipCommand::PttPressed => {
                    if matches!(dialog_state, DialogState::Idle | DialogState::Terminated) {
                        if let Ok(offer) =
                            SessionDescription::offer("atom-echo", "0.0.0.0", local_rtp_port)
                        {
                            if let Ok(mut invite) = sip.invite(settings.sip_target) {
                                if let Ok(body) = offer.render() {
                                    let _ = invite.add_header(
                                        sip_core::Header::new(
                                            "Content-Type",
                                            "application/sdp",
                                        )
                                        .unwrap(),
                                    );
                                    let _ = invite.add_header(
                                        sip_core::Header::new(
                                            "Content-Length",
                                            &body.len().to_string(),
                                        )
                                        .unwrap(),
                                    );
                                    let _ = invite.set_body(&body);
                                }
                                if let Ok(rendered) = invite.render() {
                                    send_sip(&sip_socket, &target, &rendered);
                                    dialog_state = DialogState::Inviting;
                                    let _ = audio_tx
                                        .send(AudioCommand::DialogStateChanged(dialog_state));
                                }
                            }
                        }
                    }
                }
                SipCommand::PttReleased => {}
                SipCommand::Hangup => {
                    dialog_state = DialogState::Terminated;
                    let _ = audio_tx.send(AudioCommand::DialogStateChanged(dialog_state));
                }
                SipCommand::OutgoingPcmFrame(samples) => {
                    if let Some((ip, port)) = &remote_rtp {
                        let encoded = encode_ulaw(&samples);
                        let mut header = RtpHeader::default();
                        header.sequence_number = seq;
                        header.timestamp = timestamp;
                        header.payload_type = 0;
                        header.ssrc = 1;

                        let packet: RtpPacket<512> = RtpPacket::new(header, encoded);
                        if let Ok(raw) = packet.pack() {
                            let target = format!("{}:{}", ip, port);
                            let _ = rtp_socket.send_to(&raw, &target);
                        }
                        seq = seq.wrapping_add(1);
                        timestamp = timestamp.wrapping_add(160);
                    }
                }
            },
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                warn!("SIP command channel closed");
                break;
            }
        }

        thread::sleep(Duration::from_millis(10));
    }
}

fn handle_incoming_sip(
    message: sip_core::Message,
    audio_tx: &AudioCommandSender,
) -> Option<(HString<48>, u16)> {
    match message {
        sip_core::Message::Response(resp) => {
            if resp.status_code == 200 && !resp.body.is_empty() {
                if let Ok(sdp) = sdp::parse(&resp.body) {
                    let mut ip = HString::<48>::new();
                    let _ = ip.push_str(&sdp.connection_address);
                    let port = sdp.media.port;
                    let _ = audio_tx.send(AudioCommand::SetRemoteRtpEndpoint {
                        ip: ip.clone(),
                        port,
                    });
                    let _ = audio_tx.send(AudioCommand::DialogStateChanged(
                        DialogState::Established,
                    ));
                    return Some((ip, port));
                }
            }
        }
        _ => {}
    }
    None
}

fn send_sip(socket: &UdpSocket, target: &str, payload: &str) {
    if let Ok(addr) = target.parse::<std::net::SocketAddr>() {
        let _ = socket.send_to(payload.as_bytes(), addr);
    } else if target.starts_with("sip:") {
        // try stripping scheme
        if let Ok(addr) = target
            .trim_start_matches("sip:")
            .parse::<std::net::SocketAddr>()
        {
            let _ = socket.send_to(payload.as_bytes(), addr);
        }
    }
}

fn parse_uri(uri: &str) -> String {
    let mut host = uri.trim_start_matches("sip:").to_string();
    if !host.contains(':') {
        host.push_str(":5060");
    }
    host
}
