use std::sync::mpsc::{Receiver, Sender};

use heapless::{String as HString, Vec as HVec};
use sip_core::DialogState;

pub enum SipCommand {
    PttPressed,
    PttReleased,
    Hangup,
    OutgoingPcmFrame(HVec<i16, 160>),
}

pub enum AudioCommand {
    DialogStateChanged(DialogState),
    SetRemoteRtpEndpoint { ip: HString<48>, port: u16 },
    IncomingRtpPacket(HVec<u8, 512>),
    SetLed(atom_echo_hw::LedState),
}

pub type SipCommandSender = Sender<SipCommand>;
pub type SipCommandReceiver = Receiver<SipCommand>;

pub type AudioCommandSender = Sender<AudioCommand>;
pub type AudioCommandReceiver = Receiver<AudioCommand>;
