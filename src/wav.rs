use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};

pub fn write_wav(
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
    output_dir: &Path,
    name: &str,
) -> Result<PathBuf> {
    std::fs::create_dir_all(output_dir).context("failed to create output directory")?;

    let filename = format!("{}.wav", sanitize_filename(name));
    let path = output_dir.join(&filename);

    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };

    let mut writer = WavWriter::create(&path, spec)
        .with_context(|| format!("failed to create WAV file {}", path.display()))?;

    for &sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        let i16_sample = (clamped * i16::MAX as f32) as i16;
        writer
            .write_sample(i16_sample)
            .context("failed to write sample")?;
    }

    writer
        .finalize()
        .with_context(|| format!("failed to finalize WAV file {}", path.display()))?;

    Ok(path)
}

pub fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|&s| s as f32 / i16::MAX as f32)
        .collect()
}

pub fn pcm8_to_f32(samples: &[u8]) -> Vec<f32> {
    samples.iter().map(|&s| (s as f32 / 127.5) - 1.0).collect()
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
