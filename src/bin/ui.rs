use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_title("FMOD Bank Decoder"),
        ..Default::default()
    };
    eframe::run_native(
        "FMOD Bank Decoder",
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(App::new()))
        }),
    )
}

struct App {
    bank_paths: Vec<PathBuf>,
    output_dir: Option<PathBuf>,
    samples: Vec<SampleInfo>,
    selected_samples: Vec<bool>,
    status: String,
    export_mode: ExportMode,
    filter_text: String,
    decode_state: Arc<Mutex<DecodeState>>,
    show_about: bool,
}

#[derive(PartialEq)]
enum ExportMode {
    Wav,
    Fsb5,
}

#[derive(Clone)]
struct SampleInfo {
    bank_index: usize,
    sample_index: usize,
    bank_name: String,
    name: String,
    codec: String,
    channels: u16,
    sample_rate: u32,
    duration_secs: f64,
}

struct DecodeState {
    progress: Option<DecodeProgress>,
    result: Option<DecodeResult>,
}

struct DecodeProgress {
    total: usize,
    completed: usize,
    current_file: String,
    cancel_flag: Arc<Mutex<bool>>,
}

struct DecodeResult {
    decoded_count: usize,
    skipped_count: usize,
}

impl App {
    fn new() -> Self {
        Self {
            bank_paths: Vec::new(),
            output_dir: None,
            samples: Vec::new(),
            selected_samples: Vec::new(),
            status: String::from("Ready. Open .bank files or a folder to begin."),
            export_mode: ExportMode::Wav,
            filter_text: String::new(),
            decode_state: Arc::new(Mutex::new(DecodeState {
                progress: None,
                result: None,
            })),
            show_about: false,
        }
    }

    fn load_banks(&mut self, paths: Vec<PathBuf>) {
        use fmod_bank_decoder::fsb5::parse_fsb5;
        use fmod_bank_decoder::riff::{find_fsb5_chunk, parse_riff, MappedBank};

        self.bank_paths.clear();
        self.samples.clear();
        self.selected_samples.clear();

        for path in &paths {
            match MappedBank::open(path) {
                Ok(mapped) => {
                    let data = mapped.as_slice();
                    if let Ok(riff) = parse_riff(data) {
                        if let Ok(fsb5_data) = find_fsb5_chunk(data, &riff) {
                            if let Ok(fsb5) = parse_fsb5(fsb5_data) {
                                let bank_name = path
                                    .file_stem()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string();
                                let bank_index = self.bank_paths.len();

                                for (i, sample) in fsb5.samples.iter().enumerate() {
                                    let name = fsb5.sample_name(i).to_string();
                                    let channels = sample.channels.count() as u16;
                                    let duration = if sample.sample_rate > 0 {
                                        sample.num_samples as f64 / sample.sample_rate as f64
                                    } else {
                                        0.0
                                    };

                                    self.samples.push(SampleInfo {
                                        bank_index,
                                        sample_index: i,
                                        bank_name: bank_name.clone(),
                                        name,
                                        codec: format!("{:?}", fsb5.header.codec),
                                        channels,
                                        sample_rate: sample.sample_rate,
                                        duration_secs: duration,
                                    });
                                    self.selected_samples.push(true);
                                }

                                self.bank_paths.push(path.clone());
                            }
                        }
                    }
                }
                Err(e) => {
                    self.status = format!("Error loading {}: {}", path.display(), e);
                }
            }
        }

        self.output_dir = Some(
            std::env::current_dir()
                .unwrap_or_default()
                .join("decoded_output"),
        );

        self.status = format!(
            "Loaded {} banks, {} samples",
            self.bank_paths.len(),
            self.samples.len()
        );
    }

    fn decode_selected(&mut self) {
        let bank_paths = self.bank_paths.clone();
        let output_dir = self.output_dir.clone();

        let selected_indices: Vec<usize> = self
            .selected_samples
            .iter()
            .enumerate()
            .filter(|(_, &selected)| selected)
            .map(|(i, _)| i)
            .collect();

        let samples = self.samples.clone();

        let cancel_flag = Arc::new(Mutex::new(false));
        let state = self.decode_state.clone();

        {
            let mut s = state.lock().unwrap();
            s.progress = Some(DecodeProgress {
                total: selected_indices.len(),
                completed: 0,
                current_file: String::new(),
                cancel_flag: cancel_flag.clone(),
            });
            s.result = None;
        }

        thread::spawn(move || {
            use fmod_bank_decoder::decoder::{
                decode_audio, CODEC_PCM16, CODEC_PCM8, CODEC_PCMFLOAT,
            };
            use fmod_bank_decoder::fsb5::parse_fsb5;
            use fmod_bank_decoder::riff::{find_fsb5_chunk, parse_riff, MappedBank};
            use fmod_bank_decoder::wav::write_wav;
            use rayon::prelude::*;
            use std::sync::atomic::{AtomicUsize, Ordering};

            let decoded_count = AtomicUsize::new(0);
            let skipped_count = AtomicUsize::new(0);
            let progress_counter = AtomicUsize::new(0);

            selected_indices.par_iter().for_each(|&sample_idx| {
                if *cancel_flag.lock().unwrap() {
                    return;
                }

                if sample_idx >= samples.len() {
                    return;
                }

                let sample_info = &samples[sample_idx];
                let bank_idx = sample_info.bank_index;
                let local_idx = sample_info.sample_index;

                if bank_idx >= bank_paths.len() {
                    return;
                }

                let bank_path = &bank_paths[bank_idx];

                let p = progress_counter.fetch_add(1, Ordering::Relaxed) + 1;
                if p.is_multiple_of(32) || p == selected_indices.len() {
                    let mut s = state.lock().unwrap();
                    if let Some(ref mut prog) = s.progress {
                        prog.completed = p;
                        prog.current_file = sample_info.name.clone();
                    }
                }

                let mapped = match MappedBank::open(bank_path) {
                    Ok(m) => m,
                    Err(_) => {
                        skipped_count.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                };

                let data = mapped.as_slice();
                let riff = match parse_riff(data) {
                    Ok(r) => r,
                    Err(_) => {
                        skipped_count.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                };

                let fsb5_data = match find_fsb5_chunk(data, &riff) {
                    Ok(d) => d,
                    Err(_) => {
                        skipped_count.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                };

                let fsb5 = match parse_fsb5(fsb5_data) {
                    Ok(f) => f,
                    Err(_) => {
                        skipped_count.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                };

                let bank_name = bank_path.file_stem().unwrap_or_default().to_string_lossy();
                let bank_output = output_dir.as_ref().map(|d| d.join(bank_name.to_string()));
                if let Some(ref out) = bank_output {
                    std::fs::create_dir_all(out).ok();
                }

                let codec_id = match fsb5.header.codec {
                    fmod_bank_decoder::fsb5::Fsb5Codec::Pcm8 => CODEC_PCM8,
                    fmod_bank_decoder::fsb5::Fsb5Codec::Pcm16 => CODEC_PCM16,
                    fmod_bank_decoder::fsb5::Fsb5Codec::PcMFLOAT => CODEC_PCMFLOAT,
                    fmod_bank_decoder::fsb5::Fsb5Codec::Vorbis => 0x0F,
                    _ => 0,
                };

                if local_idx >= fsb5.samples.len() {
                    skipped_count.fetch_add(1, Ordering::Relaxed);
                    return;
                }

                let sample = &fsb5.samples[local_idx];
                let name = fsb5.sample_name(local_idx);
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

                let raw_data = fsb5.sample_data(local_idx).unwrap_or(&[]);
                if raw_data.is_empty() {
                    skipped_count.fetch_add(1, Ordering::Relaxed);
                    return;
                }

                if let Some(ref out) = bank_output {
                    let channels = sample.channels.count() as u16;
                    let sample_rate = sample.sample_rate;

                    if codec_id == 0x0F {
                        let fsb5_path = std::env::temp_dir()
                            .join(format!("{}_{}_{}.fsb", bank_name, local_idx, safe_name));
                        if std::fs::write(&fsb5_path, fsb5_data).is_ok() {
                            let wav_path = out.join(format!("{}.wav", safe_name));
                            let vgmstream_name =
                                format!("vgmstream-cli{}", std::env::consts::EXE_SUFFIX);
                            let vgmstream_path = std::env::current_exe()
                                .ok()
                                .and_then(|p| {
                                    p.parent().map(|d| {
                                        d.join("tools").join("vgmstream").join(&vgmstream_name)
                                    })
                                })
                                .unwrap_or_else(|| std::path::PathBuf::from(&vgmstream_name));
                            let status = std::process::Command::new(&vgmstream_path)
                                .args([
                                    "-s",
                                    &(local_idx + 1).to_string(),
                                    "-o",
                                    wav_path.to_str().unwrap(),
                                    fsb5_path.to_str().unwrap(),
                                ])
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .status();
                            std::fs::remove_file(&fsb5_path).ok();
                            if status.map(|s| s.success()).unwrap_or(false) {
                                decoded_count.fetch_add(1, Ordering::Relaxed);
                            } else {
                                skipped_count.fetch_add(1, Ordering::Relaxed);
                            }
                        } else {
                            skipped_count.fetch_add(1, Ordering::Relaxed);
                        }
                    } else if let Ok(decoded) =
                        decode_audio(codec_id, raw_data, channels, sample_rate)
                    {
                        if write_wav(
                            &decoded.samples,
                            decoded.sample_rate,
                            decoded.channels,
                            out,
                            &safe_name,
                        )
                        .is_ok()
                        {
                            decoded_count.fetch_add(1, Ordering::Relaxed);
                        }
                    } else {
                        skipped_count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });

            {
                let mut s = state.lock().unwrap();
                s.progress = None;
                s.result = Some(DecodeResult {
                    decoded_count: decoded_count.load(Ordering::Relaxed),
                    skipped_count: skipped_count.load(Ordering::Relaxed),
                });
            }
        });

        self.status = String::from("Decoding...");
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        let is_decoding = {
            let s = self.decode_state.lock().unwrap();
            s.progress.is_some()
        };

        if is_decoding {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        {
            let mut s = self.decode_state.lock().unwrap();
            if let Some(result) = s.result.take() {
                self.status = format!(
                    "Decoded {} samples, skipped {} (unsupported codec or error)",
                    result.decoded_count, result.skipped_count
                );
            }
        }

        egui::Panel::top("menu_bar").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open Bank Files...").clicked() {
                        if let Some(paths) = rfd::FileDialog::new()
                            .add_filter("FMOD Bank", &["bank"])
                            .pick_files()
                        {
                            self.load_banks(paths);
                        }
                        ui.close();
                    }
                    if ui.button("Open Bank Folder...").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            let banks: Vec<PathBuf> = std::fs::read_dir(&dir)
                                .into_iter()
                                .flatten()
                                .filter_map(|e| e.ok())
                                .filter(|e| {
                                    e.path()
                                        .extension()
                                        .map(|ext| ext == "bank")
                                        .unwrap_or(false)
                                })
                                .map(|e| e.path())
                                .collect();
                            if !banks.is_empty() {
                                self.load_banks(banks);
                            } else {
                                self.status = format!("No .bank files found in {}", dir.display());
                            }
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Set Output Dir...").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.output_dir = Some(dir);
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        std::process::exit(0);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Select All").clicked() {
                        self.selected_samples.iter_mut().for_each(|s| *s = true);
                        ui.close();
                    }
                    if ui.button("Deselect All").clicked() {
                        self.selected_samples.iter_mut().for_each(|s| *s = false);
                        ui.close();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });
            });
        });

        egui::Panel::left("side_panel")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| {
                ui.heading("Controls");
                ui.separator();

                ui.label("Export Mode:");
                ui.radio_value(&mut self.export_mode, ExportMode::Wav, "WAV (PCM only)");
                ui.radio_value(
                    &mut self.export_mode,
                    ExportMode::Fsb5,
                    "FSB5 (for vgmstream)",
                );
                ui.separator();

                ui.label("Filter:");
                ui.text_edit_singleline(&mut self.filter_text);
                ui.separator();

                ui.label("Quick Select:");
                ui.horizontal(|ui| {
                    if ui.button("PCM16").clicked() {
                        for (i, s) in self.samples.iter().enumerate() {
                            self.selected_samples[i] = s.codec.contains("Pcm16");
                        }
                    }
                    if ui.button("Vorbis").clicked() {
                        for (i, s) in self.samples.iter().enumerate() {
                            self.selected_samples[i] = s.codec.contains("Vorbis");
                        }
                    }
                });
                ui.separator();

                let selected_count = self.selected_samples.iter().filter(|&&s| s).count();
                ui.label(format!(
                    "Selected: {} / {}",
                    selected_count,
                    self.samples.len()
                ));
                ui.separator();

                let is_decoding = self.decode_state.lock().unwrap().progress.is_some();

                if is_decoding {
                    if ui.button("Cancel").clicked() {
                        let mut s = self.decode_state.lock().unwrap();
                        if let Some(ref mut p) = s.progress {
                            *p.cancel_flag.lock().unwrap() = true;
                        }
                    }
                } else {
                    let enabled = !self.samples.is_empty();
                    ui.add_enabled_ui(enabled, |ui| {
                        if ui.button("Decode Selected").clicked() {
                            self.decode_selected();
                        }
                    });
                }

                ui.separator();

                if let Some(ref dir) = self.output_dir {
                    ui.label("Output:");
                    ui.label(egui::RichText::new(dir.display().to_string()).small());
                }

                ui.separator();
                ui.label("Status:");
                ui.label(egui::RichText::new(&self.status).small().italics());
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if !self.bank_paths.is_empty() {
                ui.label(format!("Banks ({}):", self.bank_paths.len()));
                egui::ScrollArea::horizontal()
                    .id_salt("bank_list")
                    .max_height(60.0)
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for path in &self.bank_paths {
                                let name = path.file_stem().unwrap_or_default().to_string_lossy();
                                ui.label(egui::RichText::new(name.as_ref()).small().weak());
                            }
                        });
                    });
                if let Some(ref dir) = self.output_dir {
                    ui.label(format!("Output: {}", dir.display()));
                }
                ui.separator();

                let filtered: Vec<(usize, &SampleInfo)> = self
                    .samples
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| {
                        if self.filter_text.is_empty() {
                            true
                        } else {
                            let q = self.filter_text.to_lowercase();
                            s.name.to_lowercase().contains(&q)
                                || s.codec.to_lowercase().contains(&q)
                                || s.bank_name.to_lowercase().contains(&q)
                        }
                    })
                    .collect();

                ui.label(format!(
                    "Samples: {} / {}",
                    filtered.len(),
                    self.samples.len()
                ));
                ui.separator();

                let available = ui.available_height() - 10.0;
                egui::ScrollArea::vertical()
                    .id_salt("samples_table")
                    .max_height(available)
                    .show(ui, |ui| {
                        egui::Grid::new("samples_grid")
                            .striped(true)
                            .num_columns(7)
                            .spacing([10.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(egui::RichText::new("").strong());
                                ui.label(egui::RichText::new("Bank").strong());
                                ui.label(egui::RichText::new("Name").strong());
                                ui.label(egui::RichText::new("Codec").strong());
                                ui.label(egui::RichText::new("Ch").strong());
                                ui.label(egui::RichText::new("Rate").strong());
                                ui.label(egui::RichText::new("Duration").strong());
                                ui.end_row();

                                for (orig_idx, sample) in &filtered {
                                    let idx = *orig_idx;
                                    if idx < self.selected_samples.len() {
                                        ui.checkbox(&mut self.selected_samples[idx], "");
                                    } else {
                                        ui.label("");
                                    }
                                    ui.label(&sample.bank_name);
                                    ui.label(&sample.name);
                                    ui.label(&sample.codec);
                                    ui.label(sample.channels.to_string());
                                    ui.label(format!("{}Hz", sample.sample_rate));
                                    ui.label(format!("{:.1}s", sample.duration_secs));
                                    ui.end_row();
                                }
                            });
                    });
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space(100.0);
                    ui.heading("FMOD Bank Decoder");
                    ui.add_space(20.0);
                    ui.label("Click File > Open Bank Files or Open Bank Folder to begin");
                });
            }

            {
                let s = self.decode_state.lock().unwrap();
                if let Some(ref progress) = s.progress {
                    egui::Window::new("Decoding")
                        .collapsible(false)
                        .resizable(false)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(&ctx, |ui| {
                            ui.label("Decoding samples...");
                            ui.label(&progress.current_file);
                            let progress_bar = if progress.total > 0 {
                                progress.completed as f32 / progress.total as f32
                            } else {
                                0.0
                            };
                            ui.add(
                                egui::ProgressBar::new(progress_bar)
                                    .text(format!("{}/{}", progress.completed, progress.total)),
                            );
                        });
                }
            }
        });

        if self.show_about {
            egui::Window::new("About")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(&ctx, |ui| {
                    ui.heading("FMOD Bank Decoder");
                    ui.label("v0.1.0");
                    ui.separator();
                    ui.label("Extract audio from FMOD .bank files");
                    ui.separator();
                    ui.label("Supported formats:");
                    ui.label("  - PCM8, PCM16, PCM Float -> WAV");
                    ui.label("  - Vorbis -> vgmstream decode");
                    ui.separator();
                    ui.label("Vorbis: FMOD uses a proprietary Vorbis codec.");
                    ui.label("Decoded via bundled vgmstream.");
                    ui.separator();
                    if ui.button("Close").clicked() {
                        self.show_about = false;
                    }
                });
        }
    }
}
