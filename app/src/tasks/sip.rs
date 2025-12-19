use std::io::ErrorKind::WouldBlock;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use hardware::ButtonState;
use heapless::String as HString;
use sdp::{MediaDescription, SessionDescription};
use sip_core::{
    CoreDialogEvent, CoreEvent, CoreRegistrationEvent, DigestCredentials,
    InviteKind, RegistrationResult, RegistrationState, SipStack,
    authorization_header,
};

use crate::tasks::task::{AppTask, TaskMeta};
use crate::messages::{
    AudioCommand, AudioCommandSender, AudioMode, ButtonEvent, PhoneState,
    RtpCommand, RtpCommandSender,
    SipCommand, SipCommandReceiver,
    UiCommand, UiCommandSender,
};

#[derive(Debug)]
struct CallContext {
    invite: sip_core::Request,
    remote_sdp: SessionDescription,
    local_sdp: SessionDescription,
    ring_deadline: Option<Instant>, // Some(...) while ringing, None otherwise
    remote_addr: SocketAddr,
}

pub struct SipTask {
    // App wiring
    settings: &'static crate::settings::Settings,
    sip_rx: SipCommandReceiver,
    ui_tx: UiCommandSender,
    audio_tx: AudioCommandSender,
    rtp_tx: RtpCommandSender,

    // Core SIP logic
    core: SipStack,
    call_ctx: Option<CallContext>,
    ring_timeout: Duration,

    // Networking
    rx_buf: [u8; 1500],
    sip_socket: UdpSocket,
    registrar: String,
    local_ip: String,
    local_sip_port: u16,
    local_rtp_port: u16,

    // Timers
    next_register: Instant,

    // Local mirror of reg state so we can log transitions
    last_reg_state: RegistrationState,
}

impl AppTask for SipTask {
    fn into_runner(mut self: Box<Self>) -> Box<dyn FnOnce() + Send + 'static> {
        Box::new(move || {
            self.run()
        })
    }

    fn meta(&self) -> TaskMeta {
        TaskMeta {
            name: "sip",
            stack_bytes: Some(16384),
        }
    }
}

impl SipTask {
    pub fn new(
        settings: &'static crate::settings::Settings,
        addr: IpAddr,
        local_rtp_port: u16,
        sip_rx: SipCommandReceiver,
        ui_tx: UiCommandSender,
        audio_tx: AudioCommandSender,
        rtp_tx: RtpCommandSender,
    ) -> Self {
        let core = SipStack::default();

        let registrar = parse_uri(settings.sip_registrar);

        // SIP socket
        let sip_socket = UdpSocket::bind((addr, 0)).expect("create SIP socket");
        sip_socket
            .set_nonblocking(true)
            .expect("set SIP socket non-blocking");

        if let Ok(addr) = registrar.parse::<SocketAddr>() {
            let _ = sip_socket.connect(addr);
        }

        let (local_ip, local_sip_port) = local_ip_port(&sip_socket);

        Self {
            settings,
            sip_rx,
            ui_tx,
            audio_tx,
            rtp_tx,

            core,
            call_ctx: None,
            ring_timeout: Duration::from_secs(settings.ring_timeout as u64),

            rx_buf: [0u8; 1500],
            sip_socket,
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

        // Set initial LED
        self.broadcast_phone_state();

        loop {
            let now = Instant::now();

            self.maybe_send_register(now);
            self.poll_sip_socket();
            if !self.poll_commands() {
                log::info!("SIP task exiting: command channel closed");
                break;
            }
            self.check_call_timeouts(now);
            self.process_core_timers(now);

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
                self.next_register = Instant::now();
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
                        //log::debug!("parse_message:\r\n{}", text); switching to logging `Message`
                        match sip_core::parse_message(text) {
                            Ok(msg) => {
                                log::debug!("parse_message ->\r\n{:?}", &msg);
                                let now = Instant::now();
                                let events = self.core.on_message(msg, addr, now);
                                for ev in events {
                                    self.handle_core_event(ev, addr);
                                }
                            }
                            Err(e) => {
                                log::error!("parse_message: {:?}\r\n{}", e, text);
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
                    log::debug!("Sending response");
                    send_sip_addr(&self.sip_socket, remote_addr, &text);
                } else {
                    log::warn!("Failed to render response");
                }
            }
            CoreEvent::SendResponseTo { response, target } => {
                if let Ok(text) = response.render() {
                    log::debug!("Sending response (timer)");
                    send_sip_addr(&self.sip_socket, target, &text);
                } else {
                    log::warn!("Failed to render response from timer");
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
                    let is_registered = matches!(state, RegistrationState::Registered);
                    let _ = self
                        .ui_tx
                        .send(UiCommand::RegistrationStateChanged(is_registered));
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
            CoreDialogEvent::IncomingInvite { request, kind: InviteKind::Initial } => {
                log::info!("Incoming INVITE from {}", remote_addr);
                self.on_incoming_initial_invite(request, remote_addr);
            }
            CoreDialogEvent::IncomingInvite { request, kind: InviteKind::Reinvite } => {
                log::info!("Incoming re-INVITE from {}", remote_addr);
                self.on_incoming_reinvite(request, remote_addr);
            }
            CoreDialogEvent::IncomingInvite { request, kind: InviteKind::InitialWhileBusy } => {
                log::info!("Incoming INVITE while busy from {}, sending 486", remote_addr);
                self.on_incoming_initial_while_busy(request, remote_addr);
            }
            CoreDialogEvent::DialogStateChanged(state) => {
                log::info!("Dialog state -> {}", state);
                self.on_dialog_state_changed(&state);
            }
        }
    }

    fn on_dialog_state_changed(&mut self, state: &sip_core::DialogState) {
        self.broadcast_phone_state();

        match state {
            sip_core::DialogState::Established { .. } => {
                self.start_rtp_streams_from_ctx();
            }
            sip_core::DialogState::Terminated | sip_core::DialogState::Idle => {
                self.stop_rtp_streams();
                self.call_ctx = None;
            }
            _ => {}
        }
    }

    fn start_rtp_streams_from_ctx(&mut self) {
        let ctx = match &self.call_ctx {
            Some(c) => c,
            None => {
                log::warn!("start_rtp_streams_from_ctx: no call context");
                return;
            }
        };

        if ctx.remote_sdp.media.port == 0 {
            log::info!("remote RTP port is 0 (hold); stopping RTP");
            self.stop_rtp_streams();
            return;
        }

        let mut remote_ip: HString<48> = HString::new();
        if remote_ip
            .push_str(ctx.remote_sdp.connection_address.as_str())
            .is_err()
        {
            log::warn!(
                "start_rtp_streams_from_ctx: remote IP too long: {}",
                ctx.remote_sdp.connection_address
            );
            return;
        }

        let cmd = RtpCommand::StartStream {
            remote_ip: remote_ip.clone(),
            remote_port: ctx.remote_sdp.media.port,
            expected_remote_ssrc: None,
            local_ssrc: None,
            payload_type: ctx.remote_sdp.media.payload_type,
        };

        if let Err(e) = self.rtp_tx.send(cmd) {
            log::warn!("Failed to start RTP: {:?}", e);
        }
    }

    fn stop_rtp_streams(&mut self) {
        if let Err(e) = self.rtp_tx.send(RtpCommand::StopStream) {
            log::debug!("stop_rtp_streams: receiver dropped? {:?}", e);
        }
    }

    fn on_incoming_initial_invite(&mut self, req: sip_core::Request, remote_addr: SocketAddr) {
        if req.body.is_empty() {
            log::warn!("INVITE had no SDP body; ignoring");
            return;
        }

        let sdp = match sdp::parse(req.body.as_str()) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("failed to parse SDP: {:?}", e);
                if let Err(e) =
                    self.send_response_488_not_acceptable_here(&req, remote_addr)
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
        if let Err(e) = self.send_response_180_ringing(&req, remote_addr) {
            log::warn!("failed to send 180: {:?}", e);
        }

        // Store state
        self.call_ctx = Some(CallContext {
            invite: req,
            remote_sdp: sdp,
            local_sdp: self.build_local_sdp(),
            ring_deadline: Some(ring_deadline),
            remote_addr,
        });

        // UI and audio
        let _ = self.ui_tx.send(UiCommand::DialogStateChanged(PhoneState::Ringing));
        let _ = self.audio_tx.send(AudioCommand::SetDialogState(PhoneState::Ringing));
    }

    fn on_incoming_reinvite(&mut self, req: sip_core::Request, remote_addr: SocketAddr) {
        // Check for an SDP
        if req.body.is_empty() {
            // offerless INVITE
            log::warn!("received offerless re-INVITE");
            if let Err(e) = self.send_response_488_not_acceptable_here(&req, remote_addr) {
                log::warn!("failed to send 488: {:?}", e);
            }
            return;
        }

        // Update remote SDP
        let sdp = match sdp::parse(req.body.as_str()) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("failed to parse SDP on re-INVITE: {:?}", e);
                if let Err(e) = self.send_response_488_not_acceptable_here(&req, remote_addr) {
                    log::warn!("failed to send 488: {:?}", e);
                }
                return;
            }
        };

        if let Some(ctx) = &mut self.call_ctx {
            ctx.remote_sdp = sdp;
            self.start_rtp_streams_from_ctx();
        }

        // For now, just acknowledge with our current local SDP
        if let Some(ctx) = &self.call_ctx {
            let local_sdp = ctx.local_sdp.clone();
            if let Err(e) = self.send_response_200_ok_with_sdp(&req, remote_addr, &local_sdp) {
                log::warn!("failed to respond to re-INVITE: {:?}", e);
            }
        } else {
            log::warn!("re-INVITE received but no call context; sending 481");
            if let Err(e) = self.send_response_481_call_does_not_exist(&req, remote_addr) {
                log::warn!("failed to send 481: {:?}", e);
            }
        }
    }

    fn on_incoming_initial_while_busy(&mut self, req: sip_core::Request, remote_addr: SocketAddr) {
        if let Err(e) = self.send_response_486_busy_here(&req, remote_addr) {
            log::warn!("failed to respond to INVITE: {:?}", e);
        }
    }

    // --- Network responses ---------------------------------------------------

    fn send_response_180_ringing(&mut self, invite: &sip_core::Request, remote_addr: SocketAddr) -> Result<(), sip_core::SipError> {
        let resp = self
            .core
            .dialog
            .build_response_for_request(invite, 180, "Ringing", None)?;

        let text = resp.render()?;
        self.core.record_outgoing_response(&resp, remote_addr, Instant::now());
        log::debug!("Sending 180 Ringing");
        send_sip_addr(&self.sip_socket, remote_addr, &text);
        Ok(())
    }

    fn send_response_200_ok_with_sdp(
        &mut self,
        invite: &sip_core::Request,
        remote_addr: SocketAddr,
        local_sdp: &SessionDescription,
    ) -> Result<(), sip_core::SipError> {
        let body = local_sdp.render().unwrap_or_default();
        let mut resp = self
            .core
            .dialog
            .build_response_for_request(invite, 200, "OK", Some(("application/sdp", &body)))?;

        let contact_uri = build_contact_uri(
            self.settings.sip_contact,
            &self.local_ip,
            self.local_sip_port,
        );
        let contact_value = format!("<{}>", contact_uri);
        resp.add_header(sip_core::Header::new("Contact", &contact_value)?);

        let text = resp.render()?;
        self.core.record_outgoing_response(&resp, remote_addr, Instant::now());
        log::debug!("Sending 200 OK");
        send_sip_addr(&self.sip_socket, remote_addr, &text);
        Ok(())
    }

    fn send_response_480_temporarily_unavailable(
        &mut self,
        invite: &sip_core::Request,
        remote_addr: SocketAddr
    ) -> Result<(), sip_core::SipError> {
        let resp = self
            .core
            .dialog
            .build_response_for_request(invite, 480, "Temporarily Unavailable", None)?;

        let text = resp.render()?;
        self.core.record_outgoing_response(&resp, remote_addr, Instant::now());
        log::debug!("Sending 480 Temporarily Unavailable");
        send_sip_addr(&self.sip_socket, remote_addr, &text);
        Ok(())
    }

    fn send_response_481_call_does_not_exist(
        &mut self,
        invite: &sip_core::Request,
        remote_addr: SocketAddr
    ) -> Result<(), sip_core::SipError> {
        let resp = self
            .core
            .dialog
            .build_response_for_request(invite, 481, "Call/Transaction Does Not Exist", None)?;

        let text = resp.render()?;
        self.core.record_outgoing_response(&resp, remote_addr, Instant::now());
        log::debug!("Sending 481 Call/Transaction Does Not Exist");
        send_sip_addr(&self.sip_socket, remote_addr, &text);
        Ok(())
    }

    fn send_response_486_busy_here(
        &mut self,
        invite: &sip_core::Request,
        remote_addr: SocketAddr
    ) -> Result<(), sip_core::SipError> {
        let resp = self
            .core
            .dialog
            .build_response_for_request(invite, 486, "Busy Here", None)?;

        let text = resp.render()?;
        self.core.record_outgoing_response(&resp, remote_addr, Instant::now());
        log::debug!("Sending 486 Busy Here");
        send_sip_addr(&self.sip_socket, remote_addr, &text);
        Ok(())
    }

    fn send_response_488_not_acceptable_here(
        &mut self,
        invite: &sip_core::Request,
        remote_addr: SocketAddr
    ) -> Result<(), sip_core::SipError> {
        let resp = self
            .core
            .dialog
            .build_response_for_request(invite, 488, "Not Acceptable Here", None)?;

        let text = resp.render()?;
        self.core.record_outgoing_response(&resp, remote_addr, Instant::now());
        log::debug!("Sending 488 Not Acceptable Here");
        send_sip_addr(&self.sip_socket, remote_addr, &text);
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

    fn handle_button_event(&mut self, event: ButtonEvent) {
        log::debug!("received button event {:?}", event);

        match event {
            ButtonEvent::ShortPress => self.handle_answer(),
            ButtonEvent::DoubleTap  => self.handle_hangup(),
            ButtonEvent::StateChanged(s) => self.handle_button_state_changed(s),
        }
    }

    fn handle_answer(&mut self) {
        match (&self.core.dialog.state, &self.call_ctx) {

            // Incoming call, ringing: answer
            (
                sip_core::DialogState::Ringing { role, .. },
                Some(ctx),
            ) if *role == sip_core::DialogRole::Uas => {
                // Clone what we need
                let invite = ctx.invite.clone();
                let local_sdp = ctx.local_sdp.clone();
                let remote_addr = ctx.remote_addr;

                // Build and send 200 OK + SDP, start RTP
                if let Err(e) = self.send_response_200_ok_with_sdp(&invite, remote_addr, &local_sdp) {
                    log::warn!("Failed to send 200 OK: {:?}", e);
                }

                // TODO: Flip the dialog state in core
                if let Some(ref mut c) = self.call_ctx {
                    c.ring_deadline = None;
                }

            }

            // Button pressed in some other state
            _ => {}
        }
    }

    fn handle_hangup(&mut self) {
        match &self.call_ctx {

            // Established call, not ringing
            Some(ctx) if ctx.ring_deadline.is_none() => {
                // TODO: Build BYE, send it
                // Probably implement an "end dialog" helper in core
                self.stop_rtp_streams();
                self.core.dialog.terminate_local();
                self.broadcast_phone_state();
                self.call_ctx = None;
            }

            // Double-tap in some other state
            _ => {}
        }
    }

    fn handle_button_state_changed(&mut self, state: ButtonState) {
        if let None = self.call_ctx {
            return;
        }

        // Established call
        match state {
            ButtonState::Pressed => {
                log::info!("PTT Enable!");
                let _ = self.audio_tx.send(AudioCommand::SetMode(AudioMode::Talk));
            }
            ButtonState::Released => {
                log::info!("PTT Release!");
                let _ = self.audio_tx.send(AudioCommand::SetMode(AudioMode::Listen));
            }
        }

    }

    fn build_local_sdp(&self) -> SessionDescription {
        SessionDescription {
            origin: "-".to_string(),
            connection_address: self.local_ip.clone(),
            media: MediaDescription {
                port: self.local_rtp_port,
                payload_type: 0, // PCMU/8000
                codec: sdp::Codec::Pcmu,
            }
        }
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

        let _ = self.send_response_480_temporarily_unavailable(&ctx.invite, ctx.remote_addr);

        // Move dialog to Terminated in core
        self.stop_rtp_streams();
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

    fn process_core_timers(&mut self, now: Instant) {
        let events = self.core.poll_timers(now);
        for ev in events {
            let target = match &ev {
                CoreEvent::SendResponseTo { target, .. } => *target,
                _ => SocketAddr::from(([0, 0, 0, 0], 0)),
            };
            self.handle_core_event(ev, target);
        }
    }
}

// --- Small helpers -----------------------------------------------------------

fn send_sip(socket: &UdpSocket, target: &str, payload: &str) {
    if let Ok(addr) = target.parse::<std::net::SocketAddr>() {
        log::debug!("send_sip: to={:?}\r\n{}", addr, payload);
        let _ = socket.send_to(payload.as_bytes(), addr);
    } else if target.starts_with("sip:") {
        // try stripping scheme
        match target.trim_start_matches("sip:").parse::<std::net::SocketAddr>() {
            Ok(addr) => {
                log::debug!("send_sip: to={:?}\r\n{}", addr, payload);
                let _ = socket.send_to(payload.as_bytes(), addr);
            }
            Err(e) => {
                log::error!("send_sip: couldn't parse {} to SocketAddr: {:?}", target, e);

            }
        }
    } else {
        log::error!("send_sip: couldn't parse {} to SocketAddr", target);
    }
}

fn send_sip_addr(socket: &UdpSocket, addr: SocketAddr, payload: &str) {
    log::debug!("send_sip_addr: to={:?}\r\n{}", addr, payload);
    let _ = socket.send_to(payload.as_bytes(), addr);
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

fn dialog_state_to_phone_state(dialog_state: &sip_core::DialogState) -> PhoneState {
    match dialog_state {
        &sip_core::DialogState::Idle => PhoneState::Idle,
        &sip_core::DialogState::Inviting => PhoneState::Ringing,
        &sip_core::DialogState::Ringing { .. } => PhoneState::Ringing,
        &sip_core::DialogState::Established { .. } => PhoneState::Established,
        &sip_core::DialogState::Terminated => PhoneState::Idle,
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
