use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::Duration;

use atom_echo_hw::{ButtonState, Device, LedState};
use heapless::{String as HString, Vec as HVec};
use log::{debug, warn};
use rtp_audio::{decode_ulaw, JitterBuffer, RtpPacket};

use crate::messages::{AudioCommand, AudioCommandReceiver, SipCommand, SipCommandSender};

const FRAME_SAMPLES: usize = 160;

pub fn spawn_hardware_task(
    mut device: Device,
    sip_tx: SipCommandSender,
    audio_rx: AudioCommandReceiver,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        hardware_loop(&mut device, sip_tx, audio_rx);
    })
}

fn hardware_loop(
    device: &mut Device,
    sip_tx: SipCommandSender,
    audio_rx: AudioCommandReceiver,
) {
    let mut jitter: JitterBuffer<8, FRAME_SAMPLES> = JitterBuffer::new();
    let mut last_button = device.read_button_state();
    let mut remote_rtp: Option<(HString<48>, u16)> = None;

    loop {
        // Handle messages from SIP.
        loop {
            match audio_rx.try_recv() {
                Ok(cmd) => match cmd {
                    AudioCommand::DialogStateChanged(state) => {
                        let led = match state {
                            sip_core::DialogState::Idle => LedState::Color {
                                red: 0,
                                green: 32,
                                blue: 0,
                            },
                            sip_core::DialogState::Inviting | sip_core::DialogState::Ringing => {
                                LedState::Color {
                                    red: 32,
                                    green: 32,
                                    blue: 0,
                                }
                            }
                            sip_core::DialogState::Established => LedState::Color {
                                red: 0,
                                green: 0,
                                blue: 48,
                            },
                            sip_core::DialogState::Terminated => LedState::Off,
                        };
                        let _ = device.set_led_state(led);
                    }
                    AudioCommand::SetRemoteRtpEndpoint { ip, port } => {
                        remote_rtp = Some((ip, port));
                        debug!("remote RTP endpoint set");
                    }
                    AudioCommand::IncomingRtpPacket(bytes) => {
                        if let Ok(pkt) = RtpPacket::<512>::unpack(&bytes) {
                            let decoded = decode_ulaw(&pkt.payload);
                            jitter.push_frame(pkt.header.sequence_number, &decoded);
                        }
                    }
                    AudioCommand::SetLed(state) => {
                        let _ = device.set_led_state(state);
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    warn!("audio channel closed; hardware task exiting");
                    return;
                }
            }
        }

        // Button polling (simple edge detection).
        let btn = device.read_button_state();
        if btn != last_button {
            last_button = btn;
            let event = match btn {
                ButtonState::Pressed => SipCommand::PttPressed,
                ButtonState::Released => SipCommand::PttReleased,
            };
            let _ = sip_tx.send(event);
        }

        // Playback path: drain jitter buffer (silence if empty).
        let (frame, _had_audio) = jitter.pop_frame();
        let _ = device.write_speaker_frame(&frame);

        // Capture path: always send a frame when PTT is pressed and we have a remote endpoint.
        if last_button == ButtonState::Pressed && remote_rtp.is_some() {
            let mut mic_buf = [0_i16; FRAME_SAMPLES];
            match device.read_mic_frame(&mut mic_buf) {
                Ok(count) => {
                    let mut vec: HVec<i16, FRAME_SAMPLES> = HVec::new();
                    for sample in mic_buf.iter().copied().take(count) {
                        let _ = vec.push(sample);
                    }
                    let _ = sip_tx.send(SipCommand::OutgoingPcmFrame(vec));
                }
                Err(err) => {
                    warn!("mic read error: {:?}", err);
                }
            }
        }

        thread::sleep(Duration::from_millis(10));
    }
}
