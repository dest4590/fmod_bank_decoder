# fmod_bank_decoder

Extract audio from FMOD `.bank` files. Pulls out PCM and Vorbis streams, dumps them as WAV.

Built primarily for extracting soundtracks and SFX from [Noita](https://www.nolla.fi/noita/) (2020), but works with any FMOD Studio game.

## Why?

Noita and many other games ship their audio in FMOD `.bank` files. These aren't standard formats - they're FMOD's proprietary RIFF/FEV containers wrapping FSB5 sample banks. Most existing tools either don't handle the `.bank` container at all (they expect raw FSB5), or they only support older FMOD versions.

[vgmstream](https://github.com/vgmstream/vgmstream) handles FSB5 but needs the FSB5 chunk extracted first. [fsbext](https://github.com/FFmpeg/FFmpeg) can pull raw samples but doesn't decode Vorbis. FMOD's own tools are closed-source and don't export audio.

This tool does the full pipeline: reads the `.bank` container, parses the FSB5 inside, decodes PCM natively, and shells out to vgmstream for FMOD's proprietary Vorbis. No manual extraction steps, no fiddling with hex editors.

## Features

- Parses RIFF/FEV containers and FSB5 sample banks
- Decodes PCM8, PCM16, and PCM Float natively in Rust
- Decodes FMOD's proprietary Vorbis via [vgmstream](https://github.com/vgmstream/vgmstream) (bundled)
- Multi-threaded decoding with progress bar (GUI)
- Both CLI and GUI available
- Memory-mapped I/O for fast bank loading

## Performance

Tested on Noita's `ambience.bank` (232 PCM16 samples, 21 minutes of audio, 12-core CPU):

| Mode             | Time  | Speed          |
| ---------------- | ----- | -------------- |
| Sequential       | 12.3s | 103x real-time |
| Parallel (rayon) | 9.0s  | 140x real-time |

Vorbis decoding is handled by vgmstream as a subprocess, so it's bound by process spawn overhead per sample. PCM formats are decoded entirely in Rust and are very fast.

## Requirements

- Windows (tested), macOS/Linux should work but untested
- Rust 1.75+ for building from source
- vgmstream bundled in `tools/vgmstream/` for Vorbis decoding

## Usage

### CLI

```bash
# Decode a single bank
fmod_bank_decoder.exe path/to/music01.bank output_dir

# Decode all banks in a folder
fmod_bank_decoder.exe path/to/audio/Desktop output_dir

# List samples without extracting
fmod_bank_decoder.exe path/to/bank.bank --list

# Verbose output
fmod_bank_decoder.exe path/to/bank.bank output_dir -v
```

### GUI

```bash
cargo run --release --bin ui
```

Open bank files or a folder, select samples, hit Decode Selected. Multi-threaded with a progress bar and cancel button.

### Benchmark

```bash
cargo run --release --bin benchmark -- path/to/bank.bank
```

Reports sequential vs parallel decode speed on a PCM bank.

## Noita

Noita stores its audio in `data/audio/Desktop/*.bank`. The tool handles all 25 bank files:

- **ambience.bank** - 232 PCM16 ambient sounds (21 min)
- **music.bank** through **music11.bank** - Vorbis music tracks
- **sfx banks** - explosions, UI, items, etc. (mix of PCM and Vorbis)

Total: 2,153 samples, ~2,040 successfully decoded to WAV.

## Building

```bash
git clone https://github.com/dest4590/fmod_bank_decoder.git
cd fmod_bank_decoder
cargo build --release
```

Binaries land in `target/release/`. Copy `tools/vgmstream/` next to the exe for Vorbis support.

## Project structure

```
src/
  lib.rs          - module declarations
  riff.rs         - RIFF/FEV parser, memory-mapped I/O
  fsb5.rs         - FSB5 header and sample parser
  decoder.rs      - PCM decoders (native)
  wav.rs          - WAV file writer
  main.rs         - CLI entry point
  bin/ui.rs       - egui GUI
  bin/benchmark.rs - sequential vs parallel benchmark
```

## How it works

FMOD `.bank` files are RIFF containers with an `FEV ` format tag. Inside you'll find FSB5 chunks (FMOD Sample Bank v5) that hold the actual audio. The parser reads the FSB5 headers, extracts sample metadata and raw data, then routes each sample through the appropriate decoder.

For PCM formats, decoding happens entirely in Rust. For Vorbis, the tool writes a temporary FSB5 file and hands it off to vgmstream, which understands FMOD's custom Vorbis framing.

## License

MIT
