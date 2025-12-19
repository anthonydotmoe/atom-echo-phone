use std::sync::mpsc::{Receiver, Sender};

use hardware::{ButtonState, LedState};
use heapless::{String as HString, Vec as HVec};
use rtp_audio::RtpPacket;

/// High-level call mode from the perspective of audio:
/// - Listen: speaker on, mic muted
/// - Talk: speaker muted, mic forwarded to network
#[derive(Debug, Clone, Copy)]
pub enum AudioMode {
    Listen,
    Talk,
}

#[derive(Debug)]
pub enum ButtonEvent {
    StateChanged(ButtonState),
    ShortPress,
    DoubleTap,
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

    /// Inform audio of call state if it needs to behave differently
    /// (e.g. play ringback tone vs remote audio)
    SetDialogState(PhoneState),

    // TODO: For things like comfort noise generation, tones, etc.,
    // PlayTone(ToneKind)
}

pub type AudioCommandSender = Sender<AudioCommand>;
pub type AudioCommandReceiver = Receiver<AudioCommand>;

#[derive(Debug, Clone)]
pub enum RtpCommand {
    StartStream {
        remote_ip: HString<48>,
        remote_port: u16,
        expected_remote_ssrc: Option<u32>,
        local_ssrc: Option<u32>,
        payload_type: u8,
    },
    StopStream,
}

pub type RtpCommandSender = Sender<RtpCommand>;
pub type RtpCommandReceiver = Receiver<RtpCommand>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhoneState {
    Idle,
    Ringing,
    Established,
}

#[derive(Debug)]
pub enum UiCommand {
    DialogStateChanged(PhoneState),
    RegistrationStateChanged(bool),
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
