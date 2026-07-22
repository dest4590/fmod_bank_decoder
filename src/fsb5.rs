use std::io::{Cursor, Read};

use anyhow::{bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt};

const FSB5_MAGIC: &[u8; 4] = b"FSB5";
const FSB5_HEADER_SIZE_V0: usize = 0x40;
const FSB5_HEADER_SIZE_V1: usize = 0x3C;

const SAMPLE_RATE_TABLE: [u32; 11] = [
    4000, 8000, 11000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 96000,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fsb5Codec {
    Pcm8,
    Pcm16,
    PcMFLOAT,
    Mpeg,
    Vorbis,
    Unknown(u32),
}

impl Fsb5Codec {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::Pcm8,
            2 => Self::Pcm16,
            5 => Self::PcMFLOAT,
            11 => Self::Mpeg,
            15 => Self::Vorbis,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fsb5Channels {
    Mono,
    Stereo,
    Surround5_1,
    Surround7_1,
    Unknown(u8),
}

impl Fsb5Channels {
    pub fn from_bits(bits: u8) -> Self {
        match bits {
            0 => Self::Mono,
            1 => Self::Stereo,
            2 => Self::Surround5_1,
            3 => Self::Surround7_1,
            other => Self::Unknown(other),
        }
    }

    pub fn count(&self) -> u32 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Surround5_1 => 6,
            Self::Surround7_1 => 8,
            Self::Unknown(_) => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Fsb5LoopInfo {
    pub loop_start: u32,
    pub loop_end: u32,
}

#[derive(Debug, Clone)]
pub struct Fsb5VorbisInfo {
    pub setup_id: u32,
    pub seek_table: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Fsb5ExtraData {
    Channels(u8),
    SampleRate(u32),
    LoopInfo(Fsb5LoopInfo),
    VorbisSetup(Fsb5VorbisInfo),
    Unknown { chunk_type: u8, data: Vec<u8> },
}

#[derive(Debug, Clone)]
pub struct Fsb5SampleHeader {
    pub num_samples: u32,
    pub data_offset: u32,
    pub channels: Fsb5Channels,
    pub sample_rate: u32,
    pub has_extra_data: bool,
    pub extra_data: Vec<Fsb5ExtraData>,
}

#[derive(Debug, Clone)]
pub struct Fsb5NameEntry {
    pub index: u32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Fsb5Header {
    pub version: u32,
    pub num_samples: u32,
    pub sample_header_size: u32,
    pub name_table_size: u32,
    pub data_size: u32,
    pub codec: Fsb5Codec,
    pub flags: u32,
    pub hash: [u8; 16],
    pub unknown: [u8; 8],
}

#[derive(Debug, Clone)]
pub struct Fsb5 {
    pub header: Fsb5Header,
    pub samples: Vec<Fsb5SampleHeader>,
    pub names: Vec<Fsb5NameEntry>,
    pub data: Vec<u8>,
}

pub fn parse_fsb5(input: &[u8]) -> Result<Fsb5> {
    if input.len() < 8 {
        bail!("Input too small for FSB5 header: {} < 8", input.len());
    }

    let magic = &input[0..4];
    if magic != FSB5_MAGIC {
        bail!(
            "Invalid FSB5 magic: expected {:?}, got {:?}",
            FSB5_MAGIC,
            magic
        );
    }

    let mut cursor = Cursor::new(input);
    cursor.set_position(0);

    cursor.read_exact(&mut [0u8; 4])?; // magic
    let version = cursor.read_u32::<LittleEndian>()?;

    let base_header_size = if version == 1 {
        FSB5_HEADER_SIZE_V1
    } else {
        FSB5_HEADER_SIZE_V0
    };

    if input.len() < base_header_size {
        bail!(
            "Input too small for FSB5 v{} header: {} < {}",
            version,
            input.len(),
            base_header_size
        );
    }

    let num_samples = cursor.read_u32::<LittleEndian>()?;
    let sample_header_size = cursor.read_u32::<LittleEndian>()?;
    let name_table_size = cursor.read_u32::<LittleEndian>()?;
    let data_size = cursor.read_u32::<LittleEndian>()?;
    let codec_raw = cursor.read_u32::<LittleEndian>()?;
    let _zero = cursor.read_u32::<LittleEndian>()?;
    let flags = if version == 1 {
        cursor.read_u32::<LittleEndian>()?
    } else {
        0
    };

    let mut hash = [0u8; 16];
    cursor.read_exact(&mut hash)?;

    let mut unknown = [0u8; 8];
    cursor.read_exact(&mut unknown)?;

    let header = Fsb5Header {
        version,
        num_samples,
        sample_header_size,
        name_table_size,
        data_size,
        codec: Fsb5Codec::from_u32(codec_raw),
        flags,
        hash,
        unknown,
    };

    let mut samples = Vec::with_capacity(num_samples as usize);
    let _offset = base_header_size;

    for i in 0..num_samples {
        let packed = cursor
            .read_u64::<LittleEndian>()
            .with_context(|| format!("Failed to read sample header for sample {}", i))?;

        // Bit extraction matching vgmstream's fsb5.c
        let num_sample_frames = ((packed >> 34) & 0x3FFF_FFFF) as u32;
        let data_offset_shifted = ((packed >> 7) & 0x07FFFFFF) as u32;
        let data_offset = data_offset_shifted << 5;
        let channel_bits = ((packed >> 5) & 0x3) as u8;
        let sample_rate_idx = ((packed >> 1) & 0x0F) as usize;
        let has_extra_data = (packed & 0x1) == 1;

        let sample_rate = if sample_rate_idx < SAMPLE_RATE_TABLE.len() {
            SAMPLE_RATE_TABLE[sample_rate_idx]
        } else {
            0
        };

        let mut extra_data = Vec::new();

        if has_extra_data {
            loop {
                let chunk_header = cursor
                    .read_u32::<LittleEndian>()
                    .context("Failed to read extra data chunk header")?;

                let continue_flag = (chunk_header & 0x1) == 1;
                let chunk_size = ((chunk_header >> 1) & 0x00FF_FFFF) as usize;
                let chunk_type = ((chunk_header >> 25) & 0x7F) as u8;

                let mut chunk_data = vec![0u8; chunk_size];
                cursor.read_exact(&mut chunk_data).with_context(|| {
                    format!("Failed to read extra data chunk type 0x{:02X}", chunk_type)
                })?;

                match chunk_type {
                    0x01 => {
                        if !chunk_data.is_empty() {
                            extra_data.push(Fsb5ExtraData::Channels(chunk_data[0]));
                        }
                    }
                    0x02 => {
                        if chunk_data.len() >= 4 {
                            let mut c = Cursor::new(&chunk_data);
                            let sr = c.read_u32::<LittleEndian>()?;
                            extra_data.push(Fsb5ExtraData::SampleRate(sr));
                        }
                    }
                    0x03 => {
                        if chunk_data.len() >= 8 {
                            let mut c = Cursor::new(&chunk_data);
                            let loop_start = c.read_u32::<LittleEndian>()?;
                            let loop_end = c.read_u32::<LittleEndian>()?;
                            extra_data.push(Fsb5ExtraData::LoopInfo(Fsb5LoopInfo {
                                loop_start,
                                loop_end,
                            }));
                        }
                    }
                    0x0B => {
                        if chunk_data.len() >= 4 {
                            let mut c = Cursor::new(&chunk_data);
                            let setup_id = c.read_u32::<LittleEndian>()?;
                            let seek_table = chunk_data[4..].to_vec();
                            extra_data.push(Fsb5ExtraData::VorbisSetup(Fsb5VorbisInfo {
                                setup_id,
                                seek_table,
                            }));
                        }
                    }
                    _ => {
                        extra_data.push(Fsb5ExtraData::Unknown {
                            chunk_type,
                            data: chunk_data,
                        });
                    }
                }

                if !continue_flag {
                    break;
                }
            }
        }

        samples.push(Fsb5SampleHeader {
            num_samples: num_sample_frames,
            data_offset,
            channels: Fsb5Channels::from_bits(channel_bits),
            sample_rate,
            has_extra_data,
            extra_data,
        });
    }

    let mut names = Vec::new();

    if name_table_size > 0 {
        let name_table_start = cursor.position() as usize;
        let name_table_end = name_table_start + name_table_size as usize;

        if name_table_end > input.len() {
            bail!(
                "Name table extends past end of input: {} > {}",
                name_table_end,
                input.len()
            );
        }

        let num_entries = num_samples as usize;
        let mut offsets = Vec::with_capacity(num_entries);

        for _ in 0..num_entries {
            let offset = cursor.read_u32::<LittleEndian>()?;
            offsets.push(offset as usize);
        }

        let name_table = &input[name_table_start..name_table_end];

        for (i, &offset) in offsets.iter().enumerate() {
            if offset >= name_table.len() {
                names.push(Fsb5NameEntry {
                    index: i as u32,
                    name: format!("sample_{}", i),
                });
                continue;
            }

            let name_bytes = &name_table[offset..];
            let end = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_bytes.len());

            let name = String::from_utf8_lossy(&name_bytes[..end]).to_string();
            names.push(Fsb5NameEntry {
                index: i as u32,
                name,
            });
        }
    }

    let data_offset = (base_header_size as u32 + sample_header_size + name_table_size) as usize;
    let data = if data_offset < input.len() {
        input[data_offset..].to_vec()
    } else {
        Vec::new()
    };

    Ok(Fsb5 {
        header,
        samples,
        names,
        data,
    })
}

impl Fsb5 {
    pub fn sample_data(&self, sample_index: usize) -> Result<&[u8]> {
        let sample = self
            .samples
            .get(sample_index)
            .with_context(|| format!("Sample index {} out of range", sample_index))?;

        let start = sample.data_offset as usize;
        if start >= self.data.len() {
            bail!(
                "Sample data offset {} exceeds data length {}",
                start,
                self.data.len()
            );
        }

        Ok(&self.data[start..])
    }

    pub fn sample_name(&self, sample_index: usize) -> &str {
        self.names
            .get(sample_index)
            .map(|n| n.name.as_str())
            .unwrap_or("unknown")
    }
}
