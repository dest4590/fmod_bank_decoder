use std::fs::File;
use std::path::Path;

use anyhow::{bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt};
use memmap2::Mmap;

const RIFF_MAGIC: &[u8; 4] = b"RIFF";
const FEV_FORMAT: &[u8; 4] = b"FEV ";
const LIST_MAGIC: &[u8; 4] = b"LIST";
const FSB5_MAGIC: &[u8; 4] = b"FSB5";

#[derive(Debug)]
pub struct RiffChunk<'a> {
    pub id: [u8; 4],
    pub size: u32,
    pub data: &'a [u8],
}

impl<'a> RiffChunk<'a> {
    pub fn id_str(&self) -> &str {
        std::str::from_utf8(&self.id).unwrap_or("???")
    }
}

#[derive(Debug)]
pub struct RiffFile<'a> {
    pub format: [u8; 4],
    pub chunks: Vec<RiffChunk<'a>>,
}

impl<'a> RiffFile<'a> {
    pub fn format_str(&self) -> &str {
        std::str::from_utf8(&self.format).unwrap_or("???")
    }
}

pub struct MappedBank {
    #[allow(dead_code)]
    file: File,
    mmap: Mmap,
}

impl MappedBank {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path.as_ref())
            .with_context(|| format!("failed to open {}", path.as_ref().display()))?;
        let mmap = unsafe { Mmap::map(&file) }
            .with_context(|| format!("failed to mmap {}", path.as_ref().display()))?;
        Ok(Self { file, mmap })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.mmap
    }
}

pub fn parse_riff(data: &[u8]) -> Result<RiffFile<'_>> {
    if data.len() < 12 {
        bail!("file too small for RIFF header ({} bytes)", data.len());
    }

    if &data[0..4] != RIFF_MAGIC {
        bail!(
            "invalid RIFF magic: expected {:?}, got {:?}",
            RIFF_MAGIC,
            &data[0..4]
        );
    }

    let file_size = (&data[4..8]).read_u32::<LittleEndian>()?;
    if file_size as usize + 8 > data.len() {
        bail!("RIFF size {} exceeds file length {}", file_size, data.len());
    }

    let mut format = [0u8; 4];
    format.copy_from_slice(&data[8..12]);

    if &format != FEV_FORMAT {
        bail!(
            "expected FEV format, got {:?}",
            std::str::from_utf8(&format).unwrap_or("???")
        );
    }

    let mut chunks = Vec::new();
    let mut pos = 12usize;

    while pos + 8 <= data.len() {
        let mut chunk_id = [0u8; 4];
        chunk_id.copy_from_slice(&data[pos..pos + 4]);
        let chunk_size = (&data[pos + 4..pos + 8]).read_u32::<LittleEndian>()?;
        pos += 8;

        if pos + chunk_size as usize > data.len() {
            bail!(
                "chunk {:?} size {} exceeds data bounds",
                std::str::from_utf8(&chunk_id).unwrap_or("???"),
                chunk_size
            );
        }

        let chunk_data = &data[pos..pos + chunk_size as usize];
        pos += chunk_size as usize;

        // RIFF chunks are padded to even boundaries
        if chunk_size % 2 != 0 {
            pos += 1;
        }

        chunks.push(RiffChunk {
            id: chunk_id,
            size: chunk_size,
            data: chunk_data,
        });
    }

    Ok(RiffFile { format, chunks })
}

pub fn parse_list_chunk<'a>(chunk: &RiffChunk<'a>) -> Result<Vec<RiffChunk<'a>>> {
    if &chunk.id != LIST_MAGIC {
        bail!(
            "expected LIST chunk, got {:?}",
            std::str::from_utf8(&chunk.id).unwrap_or("???")
        );
    }

    if chunk.data.len() < 4 {
        bail!("LIST chunk too small for sub-chunk type");
    }

    let mut pos = 4usize;
    let mut sub_chunks = Vec::new();

    while pos + 8 <= chunk.data.len() {
        let mut sub_id = [0u8; 4];
        sub_id.copy_from_slice(&chunk.data[pos..pos + 4]);
        let sub_size = (&chunk.data[pos + 4..pos + 8]).read_u32::<LittleEndian>()?;
        pos += 8;

        if pos + sub_size as usize > chunk.data.len() {
            bail!(
                "sub-chunk {:?} size {} exceeds LIST bounds",
                std::str::from_utf8(&sub_id).unwrap_or("???"),
                sub_size
            );
        }

        let sub_data = &chunk.data[pos..pos + sub_size as usize];
        pos += sub_size as usize;

        if sub_size % 2 != 0 {
            pos += 1;
        }

        sub_chunks.push(RiffChunk {
            id: sub_id,
            size: sub_size,
            data: sub_data,
        });
    }

    Ok(sub_chunks)
}

pub fn find_fsb5_chunk<'a>(data: &'a [u8], riff: &'a RiffFile) -> Result<&'a [u8]> {
    // First try to find FSB5 in RIFF chunks
    for chunk in &riff.chunks {
        if &chunk.id == FSB5_MAGIC {
            return Ok(chunk.data);
        }

        if &chunk.id == LIST_MAGIC {
            if let Ok(sub_chunks) = parse_list_chunk(chunk) {
                for sub in &sub_chunks {
                    if &sub.id == FSB5_MAGIC {
                        return Ok(sub.data);
                    }
                }
            }
        }
    }

    // Fallback: scan for FSB5 magic in raw data (FMOD bank format)
    let magic_offset = data.windows(4).position(|w| w == FSB5_MAGIC);
    if let Some(offset) = magic_offset {
        // FSB5 header is 0x40 bytes, then read the header to get sizes
        if offset + 0x40 <= data.len() {
            use byteorder::{LittleEndian, ReadBytesExt};
            use std::io::Cursor;

            let mut cursor = Cursor::new(&data[offset..]);
            let _magic = cursor.read_u32::<LittleEndian>()?; // FSB5
            let _version = cursor.read_u32::<LittleEndian>()?;
            let _num_samples = cursor.read_u32::<LittleEndian>()?;
            let sample_header_size = cursor.read_u32::<LittleEndian>()?;
            let name_table_size = cursor.read_u32::<LittleEndian>()?;
            let data_size = cursor.read_u32::<LittleEndian>()?;

            let total_size = 0x40 + sample_header_size + name_table_size + data_size;
            let end = (offset + total_size as usize).min(data.len());

            return Ok(&data[offset..end]);
        }
    }

    bail!("FSB5 chunk not found in bank file");
}
