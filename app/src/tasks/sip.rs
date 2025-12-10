use std::io::ErrorKind::WouldBlock;
use std::net::{SocketAddr, UdpSocket};
use std::{mem, thread};
use std::time::{Duration, Instant};

use heapless::String as HString;

use atom_echo_hw::random_u32;

use sip_core::{
    authorization_header, CoreDialogEvent, CoreEvent, CoreRegistrationEvent,
    DigestCredentials, RegistrationResult, RegistrationState, SipStack,
};

use crate::messages::{
    self, AudioCommand, AudioCommandSender, ButtonEvent, RtpRxCommand, RtpRxCommandSender, RtpTxCommand, RtpTxCommandSender, SipCommand, SipCommandReceiver, UiCommand, UiCommandSender
};

pub fn spawn_sip_task(
    settings: &'static crate::settings::Settings,
    sip_rx: SipCommandReceiver,
    ui_tx: UiCommandSender,
    audio_tx: AudioCommandSender,
    rtp_tx_tx: RtpTxCommandSender,
    rtp_rx_tx: RtpRxCommandSender,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        //.stack_size(STACK_SIZE)
        .name("sip".into())
        .spawn(move || {
            let mut task = Box::new(
                SipTask::new(settings, sip_rx, ui_tx, audio_tx, rtp_tx_tx, rtp_rx_tx)
            );
            task.run();
    })
    .expect("failed to spawn SIP task")
}

#[derive(Debug)]
struct CallContext {
    invite: sip_core::Request,
    remote_sdp: sdp::SessionDescription,
    ring_deadline: Option<Instant>, // Some(...) while ringing, None otherwise
}

struct SipTask {
    // App wiring
    settings: &'static crate::settings::Settings,
    sip_rx: SipCommandReceiver,
    ui_tx: UiCommandSender,
    audio_tx: AudioCommandSender,
    rtp_tx_tx: RtpTxCommandSender,
    rtp_rx_tx: RtpRxCommandSender,

    // Core SIP logic
    core: SipStack,
    call_ctx: Option<CallContext>,
    ring_timeout: Duration,

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
        ui_tx: UiCommandSender,
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
            ui_tx,
            audio_tx,
            rtp_tx_tx,
            rtp_rx_tx,

            core,
            call_ctx: None,
            ring_timeout: Duration::from_secs(settings.ring_timeout as u64),

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
            self.check_call_timeouts(now);

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
                        log::debug!("parse_message:\r\n{}", text);
                        match sip_core::parse_message(text) {
                            Ok(msg) => {
                                let events = self.core.on_message(msg);
                                for ev in events {
                                    self.handle_core_event(ev, addr);
                                }
                            }
                            Err(e) => {
                                log::error!("parse_message: {:?}", e);
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
            CoreEvent::SendResponse(resp) => {
                if let Ok(text) = resp.render() {
                    log::debug!("Sending response:\r\n{}", text);
                    send_sip(&self.sip_socket, &self.registrar, &text);
                } else {
                    log::warn!("Failed to render response");
                }
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
                log::info!("Dialog state -> {}", state);
                self.broadcast_phone_state();
            }
        }
    }

    fn on_incoming_invite(&mut self, req: sip_core::Request) {
        if req.body.is_empty() {
            log::warn!("INVITE had no SDP body; ignoring");
            return;
        }

        let sdp = match sdp::parse(req.body.as_str()) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("failed to parse SDP: {:?}", e);
                if let Err(e) =
                    self.send_response_488_not_acceptable_here(&req)
                {
                    log::warn!("Failed to send 488 Not Acceptable Here: {:?}", e);
                }
                return;
            }
        };

        let now = Instant::now();
        let ring_deadline = now + self.ring_timeout;

        log::info!(
            "Incoming INVITE: remote RTP {}:{}, ring timeout {:?}",
            sdp.connection_address,
            sdp.media.port,
            self.ring_timeout,
        );

        // Send 180 Ringing
        self.send_response_180_ringing(&req);

        // Store state
        self.call_ctx = Some(CallContext {
            invite: req,
            remote_sdp: sdp,
            ring_deadline: Some(ring_deadline),
        });
    }

    // --- Network responses ---------------------------------------------------

    fn send_response_180_ringing(&mut self, invite: &sip_core::Request) -> Result<(), sip_core::SipError> {
        let resp = self
            .core
            .dialog
            .build_response_for_request(invite, 180, "Ringing", None)?;

        let text = resp.render()?;
        log::debug!("Sending 180 Ringing:\r\n{}", text);
        send_sip(&self.sip_socket, &self.registrar, &text);
        Ok(())
    }

    fn send_response_200_ok_with_sdp(
        &mut self,
        invite: &sip_core::Request,
        local_sdp: &sdp::SessionDescription,
    ) -> Result<(), sip_core::SipError> {
        let body = local_sdp.render().unwrap_or_default();
        let resp = self
            .core
            .dialog
            .build_response_for_request(invite, 200, "OK", Some(&body))?;

        let text = resp.render()?;
        log::debug!("Sending 200 OK:\r\n{}", text);
        send_sip(&self.sip_socket, &self.registrar, &text);
        Ok(())
    }

    fn send_response_488_not_acceptable_here(
        &mut self,
        invite: &sip_core::Request,
    ) -> Result<(), sip_core::SipError> {
        let resp = self
            .core
            .dialog
            .build_response_for_request(invite, 488, "Not Acceptable Here", None)?;

        let text = resp.render()?;
        log::debug!("Sending 488 Not Acceptable Here:\r\n{}", text);
        send_sip(&self.sip_socket, &self.registrar, &text);
        Ok(())
    }

    fn send_response_486_busy_here(
        &mut self,
        invite: &sip_core::Request,
    ) -> Result<(), sip_core::SipError> {
        let resp = self
            .core
            .dialog
            .build_response_for_request(invite, 486, "Busy Here", None)?;

        let text = resp.render()?;
        log::debug!("Sending 486 Busy Here:\r\n{}", text);
        send_sip(&self.sip_socket, &self.registrar, &text);
        Ok(())
    }

    fn send_response_480_temporarily_unavailable(
        &mut self,
        invite: &sip_core::Request,
    ) -> Result<(), sip_core::SipError> {
        let resp = self
            .core
            .dialog
            .build_response_for_request(invite, 480, "Temporarily Unavailable", None)?;

        let text = resp.render()?;
        log::debug!("Sending 480 Temporarily Unavailable:\r\n{}", text);
        send_sip(&self.sip_socket, &self.registrar, &text);
        Ok(())
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

    fn answer_call(&mut self) {}

    fn end_call(&mut self) {
        self.call_ctx = None;
    }

    fn check_call_timeouts(&mut self, now: Instant) {
        let should_timeout = match (&self.core.dialog.state, &self.call_ctx) {
            (
                sip_core::DialogState::Ringing { role, .. },
                Some(ctx),
            ) if *role == sip_core::DialogRole::Uas => {
                match ctx.ring_deadline {
                    Some(deadline) => now >= deadline,
                    None => false,
                }
            }
            _ => false,
        };

        if !should_timeout {
            return;
        }

        log::info!("Ringing timed out: sending 480 and returning to idle");

        // Take the context out of self so we don't keep an immutable borrow
        let ctx = match self.call_ctx.take() {
            Some(ctx) => ctx,
            None => return,
        };

        let _ = self.send_response_480_temporarily_unavailable(&ctx.invite);

        // Move dialog to Terminated in core
        self.core.dialog.terminate_local();
        self.broadcast_phone_state();

        self.call_ctx = None;
    }

    fn broadcast_phone_state(&mut self) {
        let phone = dialog_state_to_phone_state(&self.core.dialog.state);

        let _ = self
            .ui_tx
            .send(UiCommand::DialogStateChanged(phone.clone()));

        let _ = self
            .audio_tx
            .send(AudioCommand::SetDialogState(phone));
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

fn dialog_state_to_phone_state(dialog_state: &sip_core::DialogState) -> messages::PhoneState {
    match dialog_state {
        &sip_core::DialogState::Idle => messages::PhoneState::Idle,
        &sip_core::DialogState::Inviting => messages::PhoneState::Ringing,
        &sip_core::DialogState::Ringing { .. } => messages::PhoneState::Ringing,
        &sip_core::DialogState::Established { .. } => messages::PhoneState::Established,
        &sip_core::DialogState::Terminated => messages::PhoneState::Idle,
    }
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
