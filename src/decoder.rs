use anyhow::{bail, Result};
use byteorder::{LittleEndian, ReadBytesExt};

pub const CODEC_PCM8: u32 = 0x01;
pub const CODEC_PCM16: u32 = 0x02;
pub const CODEC_PCMFLOAT: u32 = 0x05;

pub trait Decoder {
    fn decode(&self, data: &[u8], channels: u16) -> Result<Vec<f32>>;
}

pub struct Pcm8Decoder;

impl Decoder for Pcm8Decoder {
    fn decode(&self, data: &[u8], channels: u16) -> Result<Vec<f32>> {
        if channels == 0 {
            bail!("channel count must be > 0");
        }
        if !data.len().is_multiple_of(channels as usize) {
            bail!(
                "PCM8 data length {} is not divisible by channel count {}",
                data.len(),
                channels
            );
        }

        Ok(data.iter().map(|&s| (s as f32 / 127.5) - 1.0).collect())
    }
}

pub struct Pcm16Decoder;

impl Decoder for Pcm16Decoder {
    fn decode(&self, data: &[u8], channels: u16) -> Result<Vec<f32>> {
        if channels == 0 {
            bail!("channel count must be > 0");
        }
        if !data.len().is_multiple_of(2) {
            bail!("PCM16 data length {} is not even", data.len());
        }
        let sample_count = data.len() / 2;
        if !sample_count.is_multiple_of(channels as usize) {
            bail!(
                "sample count {} is not divisible by channel count {}",
                sample_count,
                channels
            );
        }

        let mut samples = Vec::with_capacity(sample_count);
        let mut cursor = data;
        for _ in 0..sample_count {
            let s = cursor.read_i16::<LittleEndian>()?;
            samples.push(s as f32 / i16::MAX as f32);
        }

        Ok(samples)
    }
}

pub struct PcmFloatDecoder;

impl Decoder for PcmFloatDecoder {
    fn decode(&self, data: &[u8], channels: u16) -> Result<Vec<f32>> {
        if channels == 0 {
            bail!("channel count must be > 0");
        }
        if !data.len().is_multiple_of(4) {
            bail!("PCMFLOAT data length {} is not a multiple of 4", data.len());
        }
        let sample_count = data.len() / 4;
        if !sample_count.is_multiple_of(channels as usize) {
            bail!(
                "sample count {} is not divisible by channel count {}",
                sample_count,
                channels
            );
        }

        let mut samples = Vec::with_capacity(sample_count);
        let mut cursor = data;
        for _ in 0..sample_count {
            let s = cursor.read_f32::<LittleEndian>()?;
            samples.push(s.clamp(-1.0, 1.0));
        }

        Ok(samples)
    }
}

pub fn get_decoder(codec_id: u32) -> Result<Box<dyn Decoder>> {
    match codec_id {
        CODEC_PCM8 => Ok(Box::new(Pcm8Decoder)),
        CODEC_PCM16 => Ok(Box::new(Pcm16Decoder)),
        CODEC_PCMFLOAT => Ok(Box::new(PcmFloatDecoder)),
        _ => bail!("unsupported codec ID: 0x{:02X}", codec_id),
    }
}

pub fn decode_audio(
    codec_id: u32,
    data: &[u8],
    channels: u16,
    sample_rate: u32,
) -> Result<DecodedAudio> {
    let decoder = get_decoder(codec_id)?;
    let samples = decoder.decode(data, channels)?;

    Ok(DecodedAudio {
        samples,
        channels,
        sample_rate,
    })
}

pub struct DecodedAudio {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

impl DecodedAudio {
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }

    pub fn duration_secs(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.frame_count() as f64 / self.sample_rate as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pcm8_decode() {
        let data = vec![0, 128, 255];
        let decoder = Pcm8Decoder;
        let result = decoder.decode(&data, 1).unwrap();
        assert_eq!(result.len(), 3);
        assert!((result[0] - (-1.0)).abs() < 0.01);
        assert!(result[1].abs() < 0.01);
        assert!((result[2] - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_pcm16_decode() {
        let mut data = Vec::new();
        data.extend_from_slice(&(-32768i16).to_le_bytes());
        data.extend_from_slice(&0i16.to_le_bytes());
        data.extend_from_slice(&32767i16.to_le_bytes());
        let decoder = Pcm16Decoder;
        let result = decoder.decode(&data, 1).unwrap();
        assert_eq!(result.len(), 3);
        assert!((result[0] - (-1.0)).abs() < 0.001);
        assert!(result[1].abs() < 0.001);
        assert!((result[2] - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_pcm_float_decode() {
        let mut data = Vec::new();
        data.extend_from_slice(&(-1.0f32).to_le_bytes());
        data.extend_from_slice(&0.5f32.to_le_bytes());
        let decoder = PcmFloatDecoder;
        let result = decoder.decode(&data, 1).unwrap();
        assert_eq!(result.len(), 2);
        assert!((result[0] - (-1.0)).abs() < f32::EPSILON);
        assert!((result[1] - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_stereo_pcm16() {
        let mut data = Vec::new();
        data.extend_from_slice(&1000i16.to_le_bytes());
        data.extend_from_slice(&(-1000i16).to_le_bytes());
        let decoder = Pcm16Decoder;
        let result = decoder.decode(&data, 2).unwrap();
        assert_eq!(result.len(), 2);
    }
}
