use std::time::Duration;
use crate::HardwareError;

mod codec;

#[derive(Clone, Copy)]
pub struct SampleInfo {
    pub sample_rate: u32,
    //channels: u8,
    pub bits_per_sample: u8,
    //channel_mask: u16,
    //mclk_multiple: Option<u16>,
}

pub struct AudioDevice {
    inner: Box<dyn AudioDeviceImpl + Send>,
}

pub(crate) trait AudioDeviceImpl {
    fn caps(&self) -> AudioCaps;
    //fn open_session(&mut self, fmt: SampleInfo) -> Result<AudioSession<'_>, HardwareError>;
    fn tx_enable(&mut self) -> Result<(), HardwareError>;
    fn tx_disable(&mut self) -> Result<(), HardwareError>;
    fn preload_data(&mut self, data: &[u8]) -> Result<usize, HardwareError>;
    fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, HardwareError>;
    fn read(&mut self, pcm: &mut [u8], timeout: Duration) -> Result<usize, HardwareError>;
}

impl AudioDevice {
    pub(crate) fn new(inner: Box<dyn AudioDeviceImpl + Send>) -> Self { Self { inner } }

    pub fn caps(&self) -> AudioCaps { self.inner.caps() }
    //pub fn open_session(&mut self, fmt: SampleInfo) -> Result<AudioSession<'_>, HardwareError> { self.inner.open_session(fmt) }
    pub fn tx_enable(&mut self) -> Result<(), HardwareError> { self.inner.tx_enable() }
    pub fn tx_disable(&mut self) -> Result<(), HardwareError> { self.inner.tx_disable() }
    pub fn preload_data(&mut self, data: &[u8]) -> Result<usize, HardwareError> { self.inner.preload_data(data) }
    pub fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, HardwareError> { self.inner.write(data, timeout) }
    pub fn read(&mut self, pcm: &mut [u8], timeout: Duration) -> Result<usize, HardwareError> { self.inner.read(pcm, timeout) }
}

pub struct AudioCaps {
    pub full_duplex: bool,
}

pub struct AudioSession<'a> {
    pub spk: Option<Box<dyn SpeakerSink + Send + 'a>>,
    pub mic: Option<Box<dyn MicSource + Send + 'a>>,
    pub caps: AudioCaps,
}

pub trait SpeakerSink {
    fn write(&mut self, pcm: &[u8], timeout: Duration) -> Result<usize, HardwareError>;
}

pub trait MicSource {
    fn read(&mut self, pcm: &mut [u8], timeout: Duration) -> Result<usize, HardwareError>;
}
