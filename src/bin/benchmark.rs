use std::path::PathBuf;
use std::time::Instant;

use fmod_bank_decoder::decoder::{decode_audio, CODEC_PCM16, CODEC_PCM8, CODEC_PCMFLOAT};
use fmod_bank_decoder::fsb5::parse_fsb5;
use fmod_bank_decoder::riff::{find_fsb5_chunk, parse_riff, MappedBank};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bank_path = args.get(1).map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("Usage: benchmark <bank_file>");
        eprintln!("  e.g. benchmark \"C:\\...\\ambience.bank\"");
        std::process::exit(1);
    });

    let mapped = MappedBank::open(&bank_path).expect("failed to open bank");
    let data = mapped.as_slice();
    let riff = parse_riff(data).expect("failed to parse RIFF");
    let fsb5_data = find_fsb5_chunk(data, &riff).expect("failed to find FSB5");
    let fsb5 = parse_fsb5(fsb5_data).expect("failed to parse FSB5");

    println!("Bank: {}", bank_path.display());
    println!("Samples: {}", fsb5.samples.len());
    println!("Codec: {:?}", fsb5.header.codec);
    println!();

    let codec_id = match fsb5.header.codec {
        fmod_bank_decoder::fsb5::Fsb5Codec::Pcm8 => CODEC_PCM8,
        fmod_bank_decoder::fsb5::Fsb5Codec::Pcm16 => CODEC_PCM16,
        fmod_bank_decoder::fsb5::Fsb5Codec::PcMFLOAT => CODEC_PCMFLOAT,
        _ => {
            eprintln!("Benchmark only supports PCM codecs (PCM8, PCM16, PCMFLOAT).");
            eprintln!("This bank uses {:?}.", fsb5.header.codec);
            std::process::exit(1);
        }
    };

    let _ = fsb5
        .samples
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let raw = fsb5.sample_data(i).unwrap_or(&[]);
            let ch = s.channels.count() as u16;
            decode_audio(codec_id, raw, ch, s.sample_rate)
        })
        .collect::<Vec<_>>();

    let start = Instant::now();
    let mut decoded = 0u64;
    let mut total_samples = 0u64;
    for (i, sample) in fsb5.samples.iter().enumerate() {
        let raw = fsb5.sample_data(i).unwrap_or(&[]);
        if raw.is_empty() {
            continue;
        }
        let ch = sample.channels.count() as u16;
        if let Ok(result) = decode_audio(codec_id, raw, ch, sample_rate_from_idx(&fsb5, i)) {
            total_samples += result.samples.len() as u64;
            decoded += 1;
        }
    }
    let seq_time = start.elapsed();

    use rayon::prelude::*;
    let start = Instant::now();
    let mut par_decoded = 0u64;
    let mut par_total_samples = 0u64;
    let results: Vec<_> = fsb5
        .samples
        .par_iter()
        .enumerate()
        .filter_map(|(i, sample)| {
            let raw = fsb5.sample_data(i).unwrap_or(&[]);
            if raw.is_empty() {
                return None;
            }
            let ch = sample.channels.count() as u16;
            decode_audio(codec_id, raw, ch, sample_rate_from_idx(&fsb5, i)).ok()
        })
        .collect();
    for r in &results {
        par_total_samples += r.samples.len() as u64;
        par_decoded += 1;
    }
    let par_time = start.elapsed();

    let total_duration: f64 = fsb5
        .samples
        .iter()
        .map(|s| {
            if s.sample_rate > 0 {
                s.num_samples as f64 / s.sample_rate as f64
            } else {
                0.0
            }
        })
        .sum();

    println!("=== Results ===");
    println!("Audio duration:  {:.1}s", total_duration);
    println!();
    println!("Sequential:");
    println!("  Decoded:       {}/{}", decoded, fsb5.samples.len());
    println!("  Samples out:   {}", total_samples);
    println!("  Time:          {:.2?}", seq_time);
    println!(
        "  Speed:         {:.1}x real-time",
        total_duration / seq_time.as_secs_f64()
    );
    println!();
    println!("Parallel (rayon):");
    println!("  Decoded:       {}/{}", par_decoded, fsb5.samples.len());
    println!("  Samples out:   {}", par_total_samples);
    println!("  Time:          {:.2?}", par_time);
    println!(
        "  Speed:         {:.1}x real-time",
        total_duration / par_time.as_secs_f64()
    );
    println!();
    println!("Cores: {}", num_cpus::get());
}

fn sample_rate_from_idx(fsb5: &fmod_bank_decoder::fsb5::Fsb5, idx: usize) -> u32 {
    fsb5.samples.get(idx).map(|s| s.sample_rate).unwrap_or(0)
}
