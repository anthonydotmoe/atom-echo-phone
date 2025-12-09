use std::io::ErrorKind::WouldBlock;
use std::net::{SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use heapless::String as HString;

use atom_echo_hw::random_u32;

use sip_core::{
    authorization_header, CoreDialogEvent, CoreEvent, CoreRegistrationEvent,
    DigestCredentials, RegistrationResult, RegistrationState, SipStack,
};

use crate::messages::{
    AudioCommand, AudioCommandSender, ButtonEvent,
    RtpRxCommand, RtpRxCommandSender,
    RtpTxCommand, RtpTxCommandSender,
    SipCommand, SipCommandReceiver
};

pub fn spawn_sip_task(
    settings: &'static crate::settings::Settings,
    sip_rx: SipCommandReceiver,
    audio_tx: AudioCommandSender,
    rtp_tx_tx: RtpTxCommandSender,
    rtp_rx_tx: RtpRxCommandSender,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        //.stack_size(STACK_SIZE)
        .name("sip".into())
        .spawn(move || {
            let mut task = Box::new(
                SipTask::new(settings, sip_rx, audio_tx, rtp_tx_tx, rtp_rx_tx)
            );
            task.run();
    })
    .expect("failed to spawn SIP task")
}

struct SipTask {
    // App wiring
    settings: &'static crate::settings::Settings,
    sip_rx: SipCommandReceiver,
    audio_tx: AudioCommandSender,
    rtp_tx_tx: RtpTxCommandSender,
    rtp_rx_tx: RtpRxCommandSender,

    // Core SIP logic
    core: SipStack,

    // Networking
    rx_buf: [u8; 1500],
    sip_socket: UdpSocket,
    rtp_socket: UdpSocket,
    registrar: String,
    local_ip: String,
    local_sip_port: u16,
    local_rtp_port: u16,

    // Timers
    next_register: Instant,

    // Local mirror of reg state so we can log transitions
    last_reg_state: RegistrationState,
}

impl SipTask {
    fn new(
        settings: &'static crate::settings::Settings,
        sip_rx: SipCommandReceiver,
        audio_tx: AudioCommandSender,
        rtp_tx_tx: RtpTxCommandSender,
        rtp_rx_tx: RtpRxCommandSender,
    ) -> Self {
        let core = SipStack::default();

        let registrar = parse_uri(settings.sip_registrar);

        // SIP socket
        let sip_socket = UdpSocket::bind("0.0.0.0:0").expect("create SIP socket");
        sip_socket
            .set_nonblocking(true)
            .expect("set SIP socket non-blocking");

        if let Ok(addr) = registrar.parse::<SocketAddr>() {
            let _ = sip_socket.connect(addr);
        }

        let (local_ip, local_sip_port) = local_ip_port(&sip_socket);

        // RTP socket
        let rtp_socket = UdpSocket::bind("0.0.0.0:0").expect("create RTP socket");
        let local_rtp_port = rtp_socket
            .local_addr()
            .map(|addr| addr.port())
            .unwrap_or(10_000);

        Self {
            settings,
            sip_rx,
            audio_tx,
            rtp_tx_tx,
            rtp_rx_tx,

            core,

            rx_buf: [0u8; 1500],
            sip_socket,
            rtp_socket,
            registrar,
            local_ip,
            local_sip_port,
            local_rtp_port,

            next_register: Instant::now(),
            last_reg_state: RegistrationState::Unregistered,
        }
    }

    fn run(&mut self) {
        log::info!("SIP task started: local SIP {}:{}, RTP {}",
            self.local_ip, self.local_sip_port, self.local_rtp_port
        );

        loop {
            let now = Instant::now();

            self.maybe_send_register(now);
            self.poll_sip_socket();


            if !self.poll_commands() {
                log::info!("SIP task exiting: command channel closed");
                break;
            }

            thread::sleep(Duration::from_millis(10));
        }
    }

    // --- Registration --------------------------------------------------------

    fn maybe_send_register(&mut self, now: Instant) {
        const REGISTER_TIMEOUT: Duration = Duration::from_secs(5);
        let reg_state = self.core.registration.state();

        // If we've been stuck in Registering for too long, treat it as a timeout
        // and allow a retry.
        if reg_state == RegistrationState::Registering && now >= self.next_register {
            log::warn!("registration attempt timed out; retrying");
            self.core.registration.reset_to_unregistered();
            self.handle_reg_event(CoreRegistrationEvent::StateChanged(RegistrationState::Unregistered));
        }

        let reg_state = self.core.registration.state();

        // Only send REGISTER when the timer fires and we're not already in-flight
        if now < self.next_register || reg_state == RegistrationState::Registering {
            return;
        }

        log::info!("Attempting SIP registration");
        //log_stack_high_water();

        // If already registered, keep the same Expires
        // otherwise use a small initial value.
        let expires = if reg_state == RegistrationState::Registered {
            self.core.registration.last_expires()
        } else {
            30
        };

        let auth_header = self
            .core
            .last_challenge()
            .and_then(|challenge| self.build_auth_header(&challenge, "REGISTER"));
        
        let contact_uri =
            build_contact_uri(
                self.settings.sip_contact,
                &self.local_ip,
                self.local_sip_port,
            );

        let req = match self.core.build_register(
            self.settings.sip_registrar,
            &contact_uri,
            &self.local_ip,
            self.local_sip_port,
            expires,
            auth_header,
        ) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("failed to build REGISTER: {:?}", e);
                self.next_register = now + Duration::from_secs(30);
                return;
            }
        };

        let rendered = match req.render() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("failed to render REGISTER: {:?}", e);
                self.next_register = now + Duration::from_secs(30);
                return;
            }
        };

        log::info!("sending REGISTER" /*\n{}", rendered*/ );
        send_sip(&self.sip_socket, &self.registrar, &rendered);

        // Give a short window for the first response
        self.next_register = now + REGISTER_TIMEOUT;
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
        authorization_header(
            challenge,
            &creds,
            method,
            self.settings.sip_registrar
        ).ok()
    }

    fn handle_registration_result(&mut self, result: RegistrationResult) {
        match result {
            RegistrationResult::Registered(_) => {
                let refresh_secs = self.core.registration_refresh_interval_secs();
                let refresh_secs = refresh_secs.max(5);
                log::info!(
                    "registration succeeded; scheduling refresh in {}s",
                    refresh_secs
                );
                self.next_register = Instant::now() + Duration::from_secs(refresh_secs);
            }
            RegistrationResult::AuthRequired => {
                log::info!("registration: auth required; retrying soon");
                self.next_register = Instant::now() + Duration::from_secs(1);
            }
            RegistrationResult::Failed(code) => {
                log::warn!("registration failed with status {}", code);
                self.next_register = Instant::now() + Duration::from_secs(30);
            }
            RegistrationResult::Sent => {
                // We don't actually return Sent from this core right now,
                // but we keep it for completeness.
            }
        }
    }

    // --- Network receive -----------------------------------------------------

    fn poll_sip_socket(&mut self) {
        loop {
            match self.sip_socket.recv_from(&mut self.rx_buf) {
                Ok((len, addr)) => {
                    if let Ok(text) = core::str::from_utf8(&self.rx_buf[..len]) {
                        if let Ok(msg) = sip_core::parse_message(text) {
                            let events = self.core.on_message(msg);
                            for ev in events {
                                self.handle_core_event(ev, addr);
                            }
                        }

                    }
                }
                Err(ref e) if e.kind() == WouldBlock => break,
                Err(e) => {
                    log::warn!("SIP recv error: {:?}", e);
                    break;
                }
            }
        }
    }

    fn handle_core_event(&mut self, ev: CoreEvent, remote_addr: SocketAddr) {
        match ev {
            CoreEvent::Registration(reg_ev) => self.handle_reg_event(reg_ev),
            CoreEvent::Dialog(dialog_ev) => {
                self.handle_dialog_event(dialog_ev, remote_addr)
            }
        }
    }

    fn handle_reg_event(&mut self, ev: CoreRegistrationEvent) {
        match ev {
            CoreRegistrationEvent::Result(result) => {
                self.handle_registration_result(result);
            }
            CoreRegistrationEvent::StateChanged(state) => {
                if state != self.last_reg_state {
                    self.last_reg_state = state;
                    log::info!("registration state -> {:?}", state);
                }
            }
        }
    }

    fn handle_dialog_event(
        &mut self,
        ev: CoreDialogEvent,
        remote_addr: SocketAddr,
    ) {
        match ev {
            CoreDialogEvent::IncomingInvite { request, .. } => {
                log::info!("Incoming INVITE from {}", remote_addr);
                self.on_incoming_invite(request);
            }
            CoreDialogEvent::DialogStateChanged(state) => {
                log::info!("Dialog state -> {:?}", state);
                let _ = self
                    .audio_tx
                    .send(AudioCommand::SetDialogState(state));
            }
        }
    }

    fn on_incoming_invite(&mut self, req: sip_core::Request) {
        // Very simple SDP offer handling:
        // - Parse remote SDP from INVITE body.
        // - Start RTP TX/RX toward the address/port in the SDP.
        //
        // TODO: Answer (200 OK)

        if req.body.is_empty() {
            log::warn!("INVITE had no SDP body; ignoring for now");
            return;
        }

        let sdp_text = req.body.as_str();
        let sdp = match sdp::parse(sdp_text) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("failed to parse SDP: {:?}", e);
                return;
            }
        };

        // Decide where to send RTP
        let remote_ip = sdp.connection_address.clone();
        let remote_port = sdp.media.port;

        log::info!(
            "SDP offer: remote RTP {}:{} (local RTP port {})",
            remote_ip, remote_port, self.local_rtp_port
        );

        // Kick RTP threads
        let mut ip = HString::<48>::new();
        if let Err(_) = ip.push_str(&remote_ip) {
            log::error!("Couldn't push remote IP to RtpCommand");
        }

        let _ = self.rtp_tx_tx.send(RtpTxCommand::StartStream {
            remote_ip: ip,
            remote_port,
            ssrc: random_u32(),
            payload_type: sdp.media.payload_type,
        });

        let _ = self.rtp_rx_tx.send(RtpRxCommand::StartStream {
            expected_ssrc: 0, // TODO: configure
            payload_type: sdp.media.payload_type,
        });

        //TODO: Extend this by
        // - Building 180 Ringing and 200 OK via core.dialog.build_response_for_request(...)
        // - Rendering and sending them via send_sip().
    }

    // --- Commands from UI / other tasks --------------------------------------

    fn poll_commands(&mut self) -> bool {
        loop {
            match self.sip_rx.try_recv() {
                Ok(cmd) => self.handle_command(cmd),
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    log::warn!("SIP command channel closed");
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

    fn handle_button_event(&mut self, _event: ButtonEvent) {
        // TODO: wire in call control (answer, hangup, etc.)
        // Using:
        // - self.core.dialog.start_outgoing(...)
        // - self.core.dialog.build_bye(...)
        // - plus RTP commands and UI updates.
    }
}

// --- Small helpers -----------------------------------------------------------

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

// --- Stack size logging facility ---------------------------------------------
/*
extern "C" {
    fn uxTaskGetStackHighWaterMark(handle: *mut core::ffi::c_void) -> u32;
}

pub fn log_stack_high_water() {
    unsafe {
        // NULL -> "current task"
        let words_left = uxTaskGetStackHighWaterMark(core::ptr::null_mut());
        let bytes_left = words_left as usize * core::mem::size_of::<usize>();
        log::info!("min remaining stack: {} bytes", bytes_left);
    }
}
*/
