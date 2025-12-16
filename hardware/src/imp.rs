#[cfg(target_os = "espidf")]
mod esp;

#[cfg(not(target_os = "espidf"))]
mod host {
    use super::*;
    use log::debug;
    use std::{net::{IpAddr, UdpSocket}, time::Duration};

    /// Host-side fake device handle for unit tests / desktop builds.
    #[derive(Debug)]
    pub struct DeviceInner {
        addr: IpAddr,
    }

    #[derive(Debug)]
    pub struct AudioDevice {
        buf: Vec<u8>,
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
    }

    impl Default for AtomEchoAudioDevice {
        fn default() -> Self {
            Self {
                buf: Vec::new(),
                sample_rate: 8_000,
                channels: 2,
                bits_per_sample: 16,
            }
        }
    }

    #[derive(Debug, Default)]
    pub struct UiDevice;

    pub fn init_device(config: WifiConfig) -> Result<DeviceInner, HardwareError> {
        debug!(
            "simulated Atom Echo init: ssid='{}'",
            config.ssid
        );

        // Create a socket to get ip addr
        let sock = UdpSocket::bind("0.0.0.0:0").unwrap();
        let addr = sock.local_addr().unwrap().ip();
        Ok(DeviceInner { addr })
    }

    impl DeviceInner {
        pub fn get_audio_device(&mut self) -> Result<AtomEchoAudioDevice, HardwareError> {
            Ok(AtomEchoAudioDevice::default())
        }

        pub fn get_ui_device(&mut self) -> Result<UiDevice, HardwareError> {
            Ok(UiDevice)
        }

        pub fn get_ip_addr(&self) -> IpAddr {
            return self.addr;
        }
    }
    
    impl AtomEchoAudioDevice {
        fn dump_wav_to_path<P: AsRef<std::path::Path>>(
            &self,
            path: P,
        ) -> std::io::Result<()> {
            use std::fs::File;
            use std::io::Write;

            if self.buf.is_empty() {
                return Ok(())
            }

            let sample_rate = self.sample_rate;
            let channels = self.channels;
            let bits_per_sample = self.bits_per_sample;

            let byte_rate =
                sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
            let block_align = channels * bits_per_sample / 8;
            let subchunk2_size = self.buf.len() as u32;
            let chunk_size = 4 + (8 + 16) + (8 + subchunk2_size);

            let mut f = File::create(path)?;

            // RIFF header
            f.write_all(b"RIFF")?;
            f.write_all(&chunk_size.to_le_bytes())?;
            f.write_all(b"WAVE")?;

            // fmt chunk
            f.write_all(b"fmt ")?;
            f.write_all(&16u32.to_le_bytes())?;          // Subchunk1Size
            f.write_all(&1u16.to_le_bytes())?;           // AudioFormat = PCM
            f.write_all(&channels.to_le_bytes())?;
            f.write_all(&sample_rate.to_le_bytes())?;
            f.write_all(&byte_rate.to_le_bytes())?;
            f.write_all(&block_align.to_le_bytes())?;
            f.write_all(&bits_per_sample.to_le_bytes())?;

            // data chunk
            f.write_all(b"data")?;
            f.write_all(&subchunk2_size.to_le_bytes())?;
            f.write_all(&self.buf)?;

            Ok(())
        }
    }

    impl UiDevice {
        pub fn read_button_state(&self) -> ButtonState {
            ButtonState::Released
        }

        pub fn set_led_state(&mut self, state: LedState) -> Result<(), HardwareError> {
            debug!("simulated LED state: {:?}", state);
            Ok(())
        }
    }

    impl AtomEchoAudioDevice {
        /// Disable the I2S transmit channel.
        pub fn tx_disable(&mut self) -> Result<(), HardwareError> {
            let path = format!("audio_{:#08x}.wav", random_u32());
            if let Err(e) = self.dump_wav_to_path(&path) {
                eprintln!("failed to write {}: {}", &path, e);
            } else {
                eprintln!(
                    "write {} ({} bytes of audio)",
                    &path,
                    self.buf.len()
                );
            }
            
            Ok(())
        }

        /// Enable the I2S transmit channel.
        pub fn tx_enable(&mut self) -> Result<(), HardwareError> {
            Ok(())
        }

        /// Preload data into the transmit channel DMA buffer.
        pub fn preload_data(&mut self, data: &[u8]) -> Result<usize, HardwareError> {
            self.buf.extend_from_slice(data);
            Ok(data.len())
        }

        /// Write data to the channel.
        pub fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, HardwareError> {
            self.buf.extend_from_slice(data);
            Ok(data.len())
        }
    }

    pub fn random_u32() -> u32 {
        rand::random::<u32>()
    }
}

#[cfg(target_os = "espidf")]
pub use esp::{DeviceInner, UiDevice, random_u32};
#[cfg(not(target_os = "espidf"))]
pub use host::{DeviceInner, UiDevice, random_u32};

#[cfg(target_os = "espidf")]
pub use esp::init_device;
#[cfg(not(target_os = "espidf"))]
pub use host::init_device;
