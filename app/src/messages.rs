use std::sync::mpsc::{Receiver, Sender};

use atom_echo_hw::{ButtonState, LedState};
use heapless::{String as HString, Vec as HVec};
use rtp_audio::RtpPacket;

/// High-level call mode from the perspective of audio:
/// - Idle: no call
/// - Listen: speaker on, mic muted
/// - Talk: speaker muted, mic forwarded to network
#[derive(Debug, Clone, Copy)]
pub enum AudioMode {
    Idle,
    Listen,
    Talk,
}

#[derive(Debug)]
pub enum ButtonEvent {
    StateChanged(ButtonState),
    ShortPress,
    LongPress,
}

#[derive(Debug)]
pub enum SipCommand {
    // From button task:
    Button(ButtonEvent),
}

pub type SipCommandSender = Sender<SipCommand>;
pub type SipCommandReceiver = Receiver<SipCommand>;

#[derive(Debug)]
pub enum AudioCodec {
    Pcmu8k,
}

#[derive(Debug)]
pub enum AudioCommand {
    /// High-level mode change: Idle/Listen/Talk
    SetMode(AudioMode),

    StartRtpPlayback {
        codec: AudioCodec,
        sample_rate: u32,
    },

    StopPlayback,

    /// Inform audio of call state if it needs to behave differently
    /// (e.g. play ringback tone vs remote audio)
    SetDialogState(PhoneState),

    // TODO: For things like comfort noise generation, tones, etc.,
    // PlayTone(ToneKind)
}

pub type AudioCommandSender = Sender<AudioCommand>;
pub type AudioCommandReceiver = Receiver<AudioCommand>;

#[derive(Debug)]
pub enum RtpTxCommand {
    /// Start sending outbound RTP with these parameters.
    StartStream {
        remote_ip: HString<48>,
        remote_port: u16,
        ssrc: u32,
        payload_type: u8,
    },

    /// Stop sending outbound RTP.
    StopStream,
}

pub type RtpTxCommandSender = Sender<RtpTxCommand>;
pub type RtpTxCommandReceiver = Receiver<RtpTxCommand>;

#[derive(Debug)]
pub enum RtpRxCommand {
    /// Configure inbound RTP expectations and mark the stream active.
    StartStream {
        remote_ip: HString<48>,
        remote_port: u16,
        expected_ssrc: Option<u32>,
        payload_type: u8,
    },

    /// Stop accepting RTP for the current call.
    StopStream,
}

pub type RtpRxCommandSender = Sender<RtpRxCommand>;
pub type RtpRxCommandReceiver = Receiver<RtpRxCommand>;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PhoneState {
    Idle,
    Ringing,
    Established,
}

#[derive(Debug)]
pub enum UiCommand {
    DialogStateChanged(PhoneState),
    SetLed(LedState),
}

pub type UiCommandSender = Sender<UiCommand>;
pub type UiCommandReceiver = Receiver<UiCommand>;

/// Raw PCM frames from mic, ready for μ-law and RTP packetization.
/// 160 samples @ 8kHz = 20ms
#[derive(Debug)]
pub enum MediaOut {
    PcmFrame(HVec<i16, 160>),
}

pub type MediaOutSender = Sender<MediaOut>;
pub type MediaOutReceiver = Receiver<MediaOut>;

// tune N to max payload (e.g. 160 bytes for PCMU/8000 20ms)
pub type RxRtpPacket = RtpPacket<512>;

#[derive(Debug)]
pub enum MediaIn {
    /// An incoming RTP packet that passed SSRC/PT checks.
    /// Audio task will decode, jitter-buffer, and play.
    RtpPcmuPacket(RxRtpPacket),
}

pub type MediaInSender = Sender<MediaIn>;
pub type MediaInReceiver = Receiver<MediaIn>;
