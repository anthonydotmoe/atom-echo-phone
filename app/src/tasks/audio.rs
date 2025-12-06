use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use atom_echo_hw::{ButtonState, Device};
use heapless::{String as HString, Vec as HVec};
use log::{debug, warn};
use rtp_audio::{decode_ulaw, JitterBuffer, RtpPacket};

use crate::messages::{
    AudioCommand, AudioCommandReceiver, AudioControl, AudioControlReceiver, SipCommand,
    SipCommandSender, UiCommand, UiCommandSender,
};

const FRAME_SAMPLES: usize = 160;

pub fn spawn_audio_task(
    device: Arc<Mutex<Device>>,
    sip_tx: SipCommandSender,
    audio_rx: AudioCommandReceiver,
    control_rx: AudioControlReceiver,
    ui_tx: UiCommandSender,
) -> thread::JoinHandle<()> {
    thread::spawn(move || audio_loop(device, sip_tx, audio_rx, control_rx, ui_tx))
}

fn audio_loop(
    device: Arc<Mutex<Device>>,
    sip_tx: SipCommandSender,
    audio_rx: AudioCommandReceiver,
    control_rx: AudioControlReceiver,
    ui_tx: UiCommandSender,
) {
    let mut jitter: JitterBuffer<8, FRAME_SAMPLES> = JitterBuffer::new();
    let mut remote_rtp: Option<(HString<48>, u16)> = None;
    let mut ptt_pressed = false;

    loop {
        // Handle messages from SIP.
        loop {
            match audio_rx.try_recv() {
                Ok(cmd) => match cmd {
                    AudioCommand::DialogStateChanged(state) => {
                        debug!("audio_task: dialog state {:?}", state);
                        let _ = ui_tx.send(UiCommand::DialogStateChanged(state));
                    }
                    AudioCommand::SetRemoteRtpEndpoint { ip, port } => {
                        debug!("audio_task: remote RTP {}:{}", ip, port);
                        remote_rtp = Some((ip, port));
                    }
                    AudioCommand::IncomingRtpPacket(bytes) => {
                        if let Ok(pkt) = RtpPacket::<512>::unpack(&bytes) {
                            let decoded = decode_ulaw(&pkt.payload);
                            jitter.push_frame(pkt.header.sequence_number, &decoded);
                        } else {
                            warn!("audio_task: failed to unpack RTP");
                        }
                    }
                    AudioCommand::SetLed(state) => {
                        let _ = ui_tx.send(UiCommand::SetLed(state));
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    warn!("audio_task: audio channel closed; exiting");
                    return;
                }
            }
        }

        // Button state from UI for PTT capture gating.
        loop {
            match control_rx.try_recv() {
                Ok(AudioControl::ButtonState(state)) => {
                    ptt_pressed = state == ButtonState::Pressed;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        // Playback path: drain jitter buffer (silence if empty).
        let (frame, _had_audio) = jitter.pop_frame();
        if let Ok(mut dev) = device.lock() {
            let _ = dev.write_speaker_frame(&frame);
        }

        // Capture path: only when PTT is pressed and we have a remote endpoint.
        if ptt_pressed {
            if let Some((_, _port)) = remote_rtp.as_ref() {
                let mut mic_buf = [0_i16; FRAME_SAMPLES];
                if let Ok(mut dev) = device.lock() {
                    match dev.read_mic_frame(&mut mic_buf) {
                        Ok(count) => {
                            let mut vec: HVec<i16, FRAME_SAMPLES> = HVec::new();
                            for sample in mic_buf.iter().copied().take(count) {
                                let _ = vec.push(sample);
                            }
                            if sip_tx.send(SipCommand::OutgoingPcmFrame(vec)).is_err() {
                                warn!("audio_task: failed to send PCM frame to SIP");
                            }
                        }
                        Err(err) => {
                            warn!("audio_task: mic read error: {:?}", err);
                        }
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(10));
    }
}
