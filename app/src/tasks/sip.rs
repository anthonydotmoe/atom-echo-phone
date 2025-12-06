use std::net::UdpSocket;
use std::thread;
use std::time::{Duration, Instant};

use heapless::String as HString;
use log::{debug, warn};

use sdp::SessionDescription;
use sip_core::{
    authorization_header, DigestCredentials, DialogState, RegistrationResult, RegistrationState,
    SipStack,
};

use crate::messages::{AudioCommand, AudioCommandSender, ButtonEvent, SipCommand, SipCommandReceiver};

pub fn spawn_sip_task(
    settings: &'static crate::settings::Settings,
    sip_rx: SipCommandReceiver,
    audio_tx: AudioCommandSender,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut task = SipTask::new(settings, sip_rx, audio_tx);
        task.run();
    })
}

struct SipTask {
    settings: &'static crate::settings::Settings,
    sip_rx: SipCommandReceiver,
    audio_tx: AudioCommandSender,

    sip: SipStack,
    dialog_state: DialogState,
    remote_rtp: Option<(HString<48>, u16)>,

    seq: u16,
    timestamp: u32,

    registrar: String,
    target: String,

    sip_socket: UdpSocket,
    rtp_socket: UdpSocket,
    local_ip: String,
    local_sip_port: u16,
    local_rtp_port: u16,

    next_register: Instant,
    last_reg_state: RegistrationState,
    pending_auth: Option<sip_core::DigestChallenge>,
}

impl SipTask {
    fn new(
        settings: &'static crate::settings::Settings,
        sip_rx: SipCommandReceiver,
        audio_tx: AudioCommandSender,
    ) -> Self {
        let mut sip = SipStack::default();
        let mut dialog_state = DialogState::Idle;

        let registrar = parse_uri(settings.sip_registrar);
        let target = parse_uri(settings.sip_target);

        let sip_socket = UdpSocket::bind("0.0.0.0:0").expect("create SIP socket");
        sip_socket
            .set_nonblocking(true)
            .expect("set nonblocking for SIP socket");

        if let Ok(addr) = registrar.parse::<std::net::SocketAddr>() {
            let _ = sip_socket.connect(addr);
        }

        let (local_ip, local_sip_port) = local_ip_port(&sip_socket);

        let rtp_socket = UdpSocket::bind("0.0.0.0:0").expect("create RTP socket");
        let local_rtp_port = rtp_socket
            .local_addr()
            .map(|addr| addr.port())
            .unwrap_or(10_000);

        let next_register = Instant::now();
        let last_reg_state = RegistrationState::Unregistered;
        let pending_auth = None;

        Self {
            settings,
            sip_rx,
            audio_tx,

            sip,
            dialog_state,
            remote_rtp: None,

            seq: 1,
            timestamp: 0,

            registrar,
            target,

            sip_socket,
            rtp_socket,
            local_ip,
            local_sip_port,
            local_rtp_port,

            next_register,
            last_reg_state,
            pending_auth,
        }
    }

    fn run(&mut self) {
        loop {
            let now = Instant::now();

            self.maybe_send_register(now);
            self.poll_sip_socket();
            if !self.poll_commands() {
                // Channel closed - exit task.
                break;
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    fn maybe_send_register(&mut self, now: Instant) {
        let reg_state = self.sip.registration.state();

        // Only send REGISTER when the timer fires and we're not already in-flight
        if now < self.next_register || reg_state == RegistrationState::Registering {
            return;
        }

        let expires = if reg_state == RegistrationState::Registered {
            self.sip.registration.last_expires()
        } else {
            3600
        };

        let auth_header = self
            .pending_auth
            .take()
            .and_then(|challenge| self.build_auth_header(&challenge, "REGISTER"));
        
        let contact_uri =
            build_contact_uri(self.settings.sip_contact, &self.local_ip, self.local_sip_port);
        
        match self.sip.register(
            self.settings.sip_registrar,
            &contact_uri,
            &self.local_ip,
            self.local_sip_port,
            expires,
            auth_header,
        ) {
            Ok(req) => {
                if let Ok(rendered) = req.render() {
                    debug!("sending REGISTER:\n{}", rendered);
                    send_sip(&self.sip_socket, &self.registrar, &rendered);
                    self.next_register = now + Duration::from_secs(5);
                }

            }
            Err(err) => {
                warn!("failed to build REGISTER: {:?}", err);
                self.next_register = now + Duration::from_secs(30);
            }
        }
    }

    fn build_auth_header(
        &self,
        challenge: &sip_core::DigestChallenge,
        method: &str,
    ) -> Option<sip_core::Header> {
        let creds = DigestCredentials {
            username: self.settings.sip_username,
            password: self.settings.sip_password,
        };
        authorization_header(challenge, &creds, method, self.settings.sip_registrar).ok()
    }

    fn poll_sip_socket(&mut self) {
        let mut buf = [0u8; 1500];

        loop {
            match self.sip_socket.recv_from(&mut buf) {
                Ok((len, addr)) => {
                    let text = match core::str::from_utf8(&buf[..len]) {
                        Ok(t) => t,
                        Err(_) => {
                            warn!("received non-UTF8 SIP from {addr}");
                            continue;
                        }
                    };

                    match sip_core::parse_message(text) {
                        Ok(msg) => self.handle_sip_message(msg),
                        Err(_) => warn!("failed to parse SIP from {addr}"),
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Nothing pending.
                    break;
                }
                Err(e) => {
                    warn!("SIP recv error: {:?}", e);
                    break;
                }
            }
        }
    }

    fn handle_sip_message(&mut self, msg: sip_core::Message) {
        match msg {
            sip_core::Message::Response(resp) => {
                self.handle_register_response(&resp);

                // Also let general incoming SIP handler look at responses
                if let Some((ip, port)) =
                    handle_incoming_sip(sip_core::Message::Response(resp), &self.audio_tx)
                {
                    self.remote_rtp = Some((ip, port));
                    self.set_dialog_state(DialogState::Established);
                }
            }
            other => {
                if let Some((ip, port)) = handle_incoming_sip(other, &self.audio_tx) {
                    self.remote_rtp = Some((ip, port));
                    self.set_dialog_state(DialogState::Established);
                }
            }
        }
    }

    fn handle_register_response(&mut self, resp: &sip_core::Response) {
        match self.sip.on_register_response(resp) {
            RegistrationResult::Registered(expires) => {
                let next = (expires as u64 * 8) / 10;
                let delay = Duration::from_secs(next.max(5));
                self.next_register = Instant::now() + delay;
            }
            RegistrationResult::AuthRequired => {
                self.pending_auth = self.sip.registration.last_challenge();
                self.next_register = Instant::now() + Duration::from_secs(1);
            }
            RegistrationResult::Failed(code) => {
                warn!("registration failed with {}", code);
                self.next_register = Instant::now() + Duration::from_secs(30);
            }
            RegistrationResult::Sent => {
                // nothing special to schedule; we're already waiting
            }
        }

        let state = self.sip.registration.state();
        if state != self.last_reg_state {
            self.last_reg_state = state;
            debug!("registration state -> {:?}", state);
        }
    }

    fn set_dialog_state(&mut self, state: DialogState) {
        self.dialog_state = state;
        let _ = self
            .audio_tx
            .send(AudioCommand::SetDialogState(self.dialog_state));

    }

    /// Returns false if the command channel is closed and we should exit
    fn poll_commands(&mut self) -> bool {
        loop {
            match self.sip_rx.try_recv() {
                Ok(cmd) => self.handle_command(cmd),
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    warn!("SIP command channel closed");
                    return false;
                }
            }
        }
    }

    fn handle_command(&mut self, cmd: SipCommand) {
        match cmd {
            SipCommand::Button(event) => {
                self.handle_button_event(event);
            }
        }
    }

    fn handle_button_event(&mut self, event: ButtonEvent) {
        if !matches!(
            self.dialog_state,
            DialogState::Idle | DialogState::Terminated
        ) {
            return;
        }
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
