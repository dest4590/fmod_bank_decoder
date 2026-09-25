use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{bail, Context, Result};
use clap::Parser as _;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;

use fmod_bank_decoder::decoder::{decode_audio, CODEC_PCM16, CODEC_PCM8, CODEC_PCMFLOAT};
use fmod_bank_decoder::fsb5::parse_fsb5;
use fmod_bank_decoder::riff::{find_fsb5_chunk, parse_riff, MappedBank};
use fmod_bank_decoder::wav::write_wav;

#[derive(clap::Parser)]
#[command(
    name = "fmod_bank_decoder",
    about = "Extract audio from FMOD .bank files (e.g. Noita)",
    version
)]
struct Cli {
    /// Input .bank file or directory containing .bank files
    input: PathBuf,

    /// Output directory (default: <input_name>_decoded)
    output_dir: Option<PathBuf>,

    /// List all samples without extracting
    #[arg(short, long)]
    list: bool,

    /// Filter samples by name pattern
    #[arg(short, long)]
    filter: Option<String>,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,

    /// Export raw FSB5 files for Vorbis (use with vgmstream)
    #[arg(short, long)]
    export_fsb5: bool,
}

fn find_bank_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut banks = Vec::new();
    for entry in
        fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            banks.extend(find_bank_files(&path)?);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("bank"))
        {
            banks.push(path);
        }
    }
    banks.sort();
    Ok(banks)
}

fn default_output_dir(input: &Path) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    PathBuf::from(format!("{stem}_decoded"))
}

fn codec_to_u32(codec: &fmod_bank_decoder::fsb5::Fsb5Codec) -> u32 {
    match codec {
        fmod_bank_decoder::fsb5::Fsb5Codec::Pcm8 => CODEC_PCM8,
        fmod_bank_decoder::fsb5::Fsb5Codec::Pcm16 => CODEC_PCM16,
        fmod_bank_decoder::fsb5::Fsb5Codec::PcMFLOAT => CODEC_PCMFLOAT,
        fmod_bank_decoder::fsb5::Fsb5Codec::Vorbis => 0x0F,
        fmod_bank_decoder::fsb5::Fsb5Codec::Mpeg => 0x0B,
        fmod_bank_decoder::fsb5::Fsb5Codec::Unknown(v) => *v,
    }
}

fn channel_count(channels: &fmod_bank_decoder::fsb5::Fsb5Channels) -> u16 {
    channels.count() as u16
}

struct SampleInfo {
    name: String,
    codec: String,
    channels: u16,
    sample_rate: u32,
    duration_secs: f64,
}

fn process_bank(
    bank_path: &Path,
    output_dir: &Path,
    filter: &Option<String>,
    list_mode: bool,
    export_fsb5: bool,
    verbose: bool,
    pb: &ProgressBar,
) -> Result<(Vec<PathBuf>, Vec<SampleInfo>)> {
    let bank_name = bank_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    if verbose {
        pb.println(format!("Parsing {bank_name}..."));
    }

    let mapped = MappedBank::open(bank_path).with_context(|| format!("opening {bank_name}"))?;
    let data = mapped.as_slice();
    let riff = parse_riff(data).with_context(|| format!("parsing RIFF in {bank_name}"))?;

    if verbose {
        pb.println(format!("  RIFF format: {}", riff.format_str()));
    }

    let fsb5_data = find_fsb5_chunk(data, &riff)?;
    let fsb5_data_owned = fsb5_data.to_vec(); // Clone for potential FSB5 export

    let fsb5 = parse_fsb5(fsb5_data).with_context(|| format!("parsing FSB5 in {bank_name}"))?;

    if verbose {
        pb.println(format!(
            "  FSB5 version: {}, samples: {}, codec: {:?}",
            fsb5.header.version, fsb5.header.num_samples, fsb5.header.codec
        ));
    }

    let mut sample_infos = Vec::new();
    let mut written = Vec::new();

    if list_mode {
        for (i, sample) in fsb5.samples.iter().enumerate() {
            let name = fsb5.sample_name(i);
            if let Some(ref pat) = filter {
                if !name.to_lowercase().contains(&pat.to_lowercase()) {
                    continue;
                }
            }
            let channels = channel_count(&sample.channels);
            let duration = if sample.sample_rate > 0 {
                sample.num_samples as f64 / sample.sample_rate as f64
            } else {
                0.0
            };
            sample_infos.push(SampleInfo {
                name: name.to_string(),
                codec: format!("{:?}", fsb5.header.codec),
                channels,
                sample_rate: sample.sample_rate,
                duration_secs: duration,
            });
        }
        return Ok((written, sample_infos));
    }

    let bank_output = output_dir.join(bank_path.file_stem().unwrap_or_default());
    fs::create_dir_all(&bank_output)
        .with_context(|| format!("creating output dir {}", bank_output.display()))?;

    for (i, sample) in fsb5.samples.iter().enumerate() {
        let name = fsb5.sample_name(i);

        if let Some(ref pat) = filter {
            if !name.to_lowercase().contains(&pat.to_lowercase()) {
                continue;
            }
        }

        let codec_id = codec_to_u32(&fsb5.header.codec);
        let channels = channel_count(&sample.channels);
        let sample_rate = sample.sample_rate;

        let raw_data = fsb5.sample_data(i).unwrap_or(&[]);

        if raw_data.is_empty() {
            if verbose {
                pb.println(format!("  Skipping {name}: no data"));
            }
            continue;
        }

        let safe_name: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();

        // Check if this is Vorbis and we should export FSB5 instead
        if codec_id == 0x0F && export_fsb5 {
            let fsb5_path = bank_output.join(format!("{safe_name}.fsb"));
            // Export the complete FSB5 chunk (with header) for vgmstream
            match std::fs::write(&fsb5_path, &fsb5_data_owned) {
                Ok(_) => {
                    written.push(fsb5_path.clone());
                    if verbose {
                        pb.println(format!("  ✓ {name}: exported FSB5 for vgmstream"));
                    }
                }
                Err(e) => {
                    pb.println(format!("  ✗ {name}: FSB5 export error: {e:#}"));
                }
            }
            continue;
        }

        // Vorbis: use vgmstream to decode FSB5 to WAV
        if codec_id == 0x0F {
            let fsb5_path = std::env::temp_dir().join(format!("{safe_name}.fsb"));
            match std::fs::write(&fsb5_path, &fsb5_data_owned) {
                Ok(_) => {
                    let wav_path = bank_output.join(format!("{safe_name}.wav"));
                    let vgmstream_path = std::env::current_exe()?
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .join("tools")
                        .join("vgmstream")
                        .join(format!("vgmstream-cli{}", std::env::consts::EXE_SUFFIX));
                    let status = std::process::Command::new(&vgmstream_path)
                        .args([
                            "-o",
                            wav_path.to_str().unwrap(),
                            fsb5_path.to_str().unwrap(),
                        ])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                    std::fs::remove_file(&fsb5_path).ok();
                    if status.map(|s| s.success()).unwrap_or(false) {
                        written.push(wav_path);
                        if verbose {
                            pb.println(format!("  ✓ {name}: decoded via vgmstream"));
                        }
                    } else {
                        pb.println(format!("  ✗ {name}: vgmstream decode failed"));
                    }
                }
                Err(e) => {
                    pb.println(format!("  ✗ {name}: FSB5 write error: {e:#}"));
                }
            }
            continue;
        }

        match decode_audio(codec_id, raw_data, channels, sample_rate) {
            Ok(decoded) => {
                match write_wav(
                    &decoded.samples,
                    decoded.sample_rate,
                    decoded.channels,
                    &bank_output,
                    &safe_name,
                ) {
                    Ok(path) => {
                        written.push(path);
                        if verbose {
                            let dur = decoded.duration_secs();
                            pb.println(format!(
                                "  ✓ {name}: {:.1}s, {}ch, {}Hz",
                                dur, decoded.channels, decoded.sample_rate
                            ));
                        }
                    }
                    Err(e) => {
                        pb.println(format!("  ✗ {name}: write error: {e:#}"));
                    }
                }
            }
            Err(e) => {
                if verbose {
                    pb.println(format!("  ✗ {name}: {e:#}"));
                }
            }
        }
    }

    Ok((written, sample_infos))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if !cli.input.exists() {
        bail!("input path does not exist: {}", cli.input.display());
    }

    let bank_files = if cli.input.is_dir() {
        find_bank_files(&cli.input).with_context(|| "searching for .bank files")?
    } else {
        vec![cli.input.clone()]
    };

    if bank_files.is_empty() {
        bail!("no .bank files found in {}", cli.input.display());
    }

    let output_dir = cli
        .output_dir
        .unwrap_or_else(|| default_output_dir(&cli.input));

    let pb = ProgressBar::new(bank_files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:30.cyan/blue}] {pos}/{len} banks")
            .unwrap()
            .progress_chars("=> "),
    );

    let total_files = AtomicUsize::new(0);

    if cli.list {
        let all_infos: Vec<Vec<SampleInfo>> = if bank_files.len() == 1 {
            let (_, infos) = process_bank(
                &bank_files[0],
                &output_dir,
                &cli.filter,
                true,
                false,
                cli.verbose,
                &pb,
            )?;
            pb.inc(1);
            vec![infos]
        } else {
            bank_files
                .par_iter()
                .map(|path| {
                    let (_, infos) = process_bank(
                        path,
                        &output_dir,
                        &cli.filter,
                        true,
                        false,
                        cli.verbose,
                        &pb,
                    )
                    .unwrap_or_else(|e| {
                        pb.println(format!("Error processing {}: {e:#}", path.display()));
                        (Vec::new(), Vec::new())
                    });
                    pb.inc(1);
                    infos
                })
                .collect()
        };

        pb.finish_and_clear();

        let all_infos: Vec<SampleInfo> = all_infos.into_iter().flatten().collect();
        println!("Samples found in {} bank(s):", bank_files.len());
        println!();
        for info in &all_infos {
            println!(
                "  {:<30} {:>8.1}s  {}ch  {}Hz  [{}]",
                info.name, info.duration_secs, info.channels, info.sample_rate, info.codec
            );
        }
        println!("\nTotal: {} sample(s)", all_infos.len());
    } else {
        let all_written: Vec<Vec<PathBuf>> = if bank_files.len() == 1 {
            let (written, _) = process_bank(
                &bank_files[0],
                &output_dir,
                &cli.filter,
                false,
                cli.export_fsb5,
                cli.verbose,
                &pb,
            )?;
            total_files.fetch_add(written.len(), Ordering::Relaxed);
            pb.inc(1);
            vec![written]
        } else {
            bank_files
                .par_iter()
                .map(|path| {
                    let (written, _) = process_bank(
                        path,
                        &output_dir,
                        &cli.filter,
                        false,
                        cli.export_fsb5,
                        cli.verbose,
                        &pb,
                    )
                    .unwrap_or_else(|e| {
                        pb.println(format!("Error processing {}: {e:#}", path.display()));
                        (Vec::new(), Vec::new())
                    });
                    total_files.fetch_add(written.len(), Ordering::Relaxed);
                    pb.inc(1);
                    written
                })
                .collect()
        };

        pb.finish_and_clear();

        let file_count: Vec<PathBuf> = all_written.into_iter().flatten().collect();
        println!(
            "Extracted {} sample(s) from {} bank(s) to {}",
            file_count.len(),
            bank_files.len(),
            output_dir.display()
        );
    }

    Ok(())
}
