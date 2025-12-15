use std::io::Write;
use std::net::{IpAddr, UdpSocket};
use std::path::Path;
use std::time::Duration;

use crate::audio::AudioSink;
use crate::HardwareError;

/// Host-side fake audio device so we can run the app and tests on x86.
#[derive(Debug, Default)]
pub struct HostAudio {
    buf: Vec<u8>,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
}

impl HostAudio {
    fn dump_wav_to_path<P: AsRef<Path>>(&self, path: P) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }

        let sample_rate = self.sample_rate;
        let channels = self.channels;
        let bits_per_sample = self.bits_per_sample;

        let byte_rate = sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
        let block_align = channels * bits_per_sample / 8;
        let subchunk2_size = self.buf.len() as u32;
        let chunk_size = 4 + (8 + 16) + (8 + subchunk2_size);

        let mut f = std::fs::File::create(path)?;

        f.write_all(b"RIFF")?;
        f.write_all(&chunk_size.to_le_bytes())?;
        f.write_all(b"WAVE")?;

        f.write_all(b"fmt ")?;
        f.write_all(&16u32.to_le_bytes())?;
        f.write_all(&1u16.to_le_bytes())?;
        f.write_all(&channels.to_le_bytes())?;
        f.write_all(&sample_rate.to_le_bytes())?;
        f.write_all(&byte_rate.to_le_bytes())?;
        f.write_all(&block_align.to_le_bytes())?;
        f.write_all(&bits_per_sample.to_le_bytes())?;

        f.write_all(b"data")?;
        f.write_all(&subchunk2_size.to_le_bytes())?;
        f.write_all(&self.buf)?;

        Ok(())
    }
}

impl AudioSink for HostAudio {
    fn tx_enable(&mut self) -> Result<(), HardwareError> {
        Ok(())
    }

    fn tx_disable(&mut self) -> Result<(), HardwareError> {
        let path = format!("audio_{:#08x}.wav", rand::random::<u32>());
        if let Err(e) = self.dump_wav_to_path(&path) {
            eprintln!("failed to write {}: {}", &path, e);
        } else {
            eprintln!("write {} ({} bytes of audio)", &path, self.buf.len());
        }
        Ok(())
    }

    fn preload_data(&mut self, data: &[u8]) -> Result<usize, HardwareError> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn write(&mut self, data: &[u8], _timeout: Duration) -> Result<usize, HardwareError> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
}

pub fn init_default_audio() -> Result<HostAudio, HardwareError> {
    Ok(HostAudio {
        sample_rate: 8_000,
        channels: 2,
        bits_per_sample: 16,
        ..Default::default()
    })
}

/// Quick helper to grab a local IP for host builds.
pub fn host_ip_addr() -> IpAddr {
    // Bind an ephemeral UDP socket and read its local address.
    let sock = UdpSocket::bind("0.0.0.0:0").expect("bind ephemeral UDP socket");
    sock.local_addr().unwrap().ip()
}
