use std::thread;
use std::time::Duration;

use heapless::String as HString;
use log::{debug, info, warn};

use rtp_audio::{encode_ulaw, RtpHeader, RtpPacket};
use sdp::SessionDescription;
use sip_core::{DialogState, SipStack};

use crate::messages::{AudioCommand, AudioCommandSender, SipCommand, SipCommandReceiver};

pub fn spawn_sip_task(
    sip_rx: SipCommandReceiver,
    audio_tx: AudioCommandSender,
) -> thread::JoinHandle<()> {
    thread::spawn(move || sip_loop(sip_rx, audio_tx))
}

fn sip_loop(sip_rx: SipCommandReceiver, audio_tx: AudioCommandSender) {
    let mut sip = SipStack::default();
    let mut dialog_state = DialogState::Idle;
    let mut remote_rtp: Option<(HString<48>, u16)> = None;
    let mut seq: u16 = 1;
    let mut timestamp: u32 = 0;

    // Stub registration
    if let Ok(req) = sip.register("sip:user@example.com", "sip:user@example.com") {
        info!("sending REGISTER: {:?}", req.render());
        sip.on_register_response(200);
    }

    loop {
        let cmd = match sip_rx.recv() {
            Ok(c) => c,
            Err(_) => {
                warn!("SIP command channel closed; exiting sip task");
                break;
            }
        };

        match cmd {
            SipCommand::PttPressed => {
                if matches!(dialog_state, DialogState::Idle | DialogState::Terminated) {
                    // Create INVITE + SDP offer (stub send).
                    if let Ok(offer) = SessionDescription::offer("atom-echo", "0.0.0.0", 10_000) {
                        let _ = offer.render(); // placeholder; would be body
                    }
                    dialog_state = DialogState::Established;
                    let _ = audio_tx.send(AudioCommand::DialogStateChanged(dialog_state));

                    let mut ip = HString::<48>::new();
                    let _ = ip.push_str("192.0.2.10");
                    remote_rtp = Some((ip.clone(), 20_000));
                    let _ = audio_tx.send(AudioCommand::SetRemoteRtpEndpoint {
                        ip,
                        port: 20_000,
                    });
                }
            }
            SipCommand::PttReleased => {
                // nothing extra for now
            }
            SipCommand::Hangup => {
                dialog_state = DialogState::Terminated;
                let _ = audio_tx.send(AudioCommand::DialogStateChanged(dialog_state));
            }
            SipCommand::OutgoingPcmFrame(samples) => {
                if let Some((_ip, _port)) = &remote_rtp {
                    let encoded = encode_ulaw(&samples);
                    let mut header = RtpHeader::default();
                    header.sequence_number = seq;
                    header.timestamp = timestamp;
                    header.payload_type = 0;

                    let packet: RtpPacket<512> = RtpPacket::new(header, encoded);
                    if let Ok(raw) = packet.pack() {
                        debug!("would send RTP packet of {} bytes", raw.len());
                        // Stub: UDP send can be added here once networking is wired.
                        // socket.send_to(&raw, format!("{}:{}", ip, port)).ok();
                    }
                    seq = seq.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(160);
                }
            }
        }

        thread::sleep(Duration::from_millis(5));
    }
}
