use std::thread;
use std::time::{Duration, Instant};

use std::net::UdpSocket;

use heapless::String as HString;
use log::{debug, warn};

use rtp_audio::{encode_ulaw, RtpHeader, RtpPacket};
use sdp::SessionDescription;
use sip_core::{
    authorization_header, DigestCredentials, DialogState, RegistrationResult, RegistrationState,
    SipStack,
};

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
    let registrar_addr = registrar.parse::<std::net::SocketAddr>().ok();
    if let Some(addr) = registrar_addr {
        let _ = sip_socket.connect(addr);
    }
    let (local_ip, local_sip_port) = local_ip_port(&sip_socket);
    let contact_uri = build_contact_uri(settings.sip_contact, &local_ip, local_sip_port);

    let rtp_socket = UdpSocket::bind("0.0.0.0:0").expect("create RTP socket");
    local_rtp_port = rtp_socket
        .local_addr()
        .map(|addr| addr.port())
        .unwrap_or(10_000);

    let mut next_register = Instant::now();
    let mut last_reg_state = RegistrationState::Unregistered;
    let mut pending_auth = None;

    loop {
        let now = Instant::now();

        if now >= next_register && sip.registration.state() != RegistrationState::Registering {
            let expires = if sip.registration.state() == RegistrationState::Registered {
                sip.registration.last_expires()
            } else {
                3600
            };

            let auth_header = pending_auth.take().and_then(|challenge| {
                let creds = DigestCredentials {
                    username: settings.sip_username,
                    password: settings.sip_password,
                };
                authorization_header(&challenge, &creds, "REGISTER", settings.sip_registrar).ok()
            });

            match sip.register(
                settings.sip_registrar,
                &contact_uri,
                &local_ip,
                local_sip_port,
                expires,
                auth_header,
            ) {
                Ok(req) => {
                    if let Ok(rendered) = req.render() {
                        debug!("sending REGISTER:\n{}", rendered);
                        send_sip(&sip_socket, &registrar, &rendered);
                        next_register = now + Duration::from_secs(5);
                    }
                }
                Err(err) => {
                    warn!("failed to build REGISTER: {:?}", err);
                    next_register = now + Duration::from_secs(30);
                }
            }
        }

        // Handle inbound SIP packets
        let mut buf = [0u8; 1500];
        match sip_socket.recv_from(&mut buf) {
            Ok((len, addr)) => {
                if let Ok(text) = core::str::from_utf8(&buf[..len]) {
                    match sip_core::parse_message(text) {
                        Ok(sip_core::Message::Response(resp)) => {
                            match sip.on_register_response(&resp) {
                                RegistrationResult::Registered(expires) => {
                                    let next = (expires as u64 * 8) / 10;
                                    next_register =
                                        Instant::now() + Duration::from_secs(next.max(5));
                                }
                                RegistrationResult::AuthRequired => {
                                    pending_auth = sip.registration.last_challenge();
                                    next_register = Instant::now() + Duration::from_secs(1);
                                }
                                RegistrationResult::Failed(code) => {
                                    warn!("registration failed with {}", code);
                                    next_register = Instant::now() + Duration::from_secs(30);
                                }
                                RegistrationResult::Sent => {}
                            }
                            if last_reg_state != sip.registration.state() {
                                last_reg_state = sip.registration.state();
                                debug!("registration state -> {:?}", last_reg_state);
                            }
                            if let Some((ip, port)) =
                                handle_incoming_sip(sip_core::Message::Response(resp), &audio_tx)
                            {
                                remote_rtp = Some((ip, port));
                                dialog_state = DialogState::Established;
                            }
                        }
                        Ok(msg) => {
                            if let Some((ip, port)) = handle_incoming_sip(msg, &audio_tx) {
                                remote_rtp = Some((ip, port));
                                dialog_state = DialogState::Established;
                            }
                        }
                        Err(_) => warn!("failed to parse SIP from {addr}"),
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

fn build_contact_uri(template: &str, ip: &str, port: u16) -> String {
    let user_part = template
        .trim_start_matches("sip:")
        .split('@')
        .next()
        .unwrap_or(template);
    format!("sip:{}@{}:{}", user_part, ip, port)
}

fn local_ip_port(sock: &UdpSocket) -> (String, u16) {
    let addr = sock
        .local_addr()
        .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());
    (addr.ip().to_string(), addr.port())
}
