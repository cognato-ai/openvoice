use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{WavSpec, WavWriter};
use parking_lot::Mutex;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Arc;

pub struct SendSyncStream(pub cpal::Stream);
unsafe impl Send for SendSyncStream {}
unsafe impl Sync for SendSyncStream {}

pub struct AudioRecorder {
    stream: Option<SendSyncStream>,
    writer: Arc<Mutex<Option<WavWriter<BufWriter<std::fs::File>>>>>,
    pub level: Arc<Mutex<f32>>,
}

impl AudioRecorder {
    pub fn new() -> Self {
        Self {
            stream: None,
            writer: Arc::new(Mutex::new(None)),
            level: Arc::new(Mutex::new(0.0)),
        }
    }

    pub fn start(&mut self, output_path: PathBuf) -> Result<(), String> {
        // Ensure any previous session is fully torn down
        let _ = self.stop();

        let host = cpal::default_host();
        let device = host.default_input_device().ok_or_else(|| {
            "No microphone found. Connect a mic or grant Microphone access in System Settings."
                .to_string()
        })?;

        let config = device
            .default_input_config()
            .map_err(|e| format!("Microphone config error: {e}"))?;

        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as u32;

        let spec = WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };

        let file = std::fs::File::create(&output_path)
            .map_err(|e| format!("Could not create recording file: {e}"))?;
        let writer: WavWriter<BufWriter<std::fs::File>> =
            WavWriter::new(BufWriter::new(file), spec).map_err(|e| e.to_string())?;

        let writer_arc = self.writer.clone();
        *writer_arc.lock() = Some(writer);

        let level_arc = self.level.clone();
        let writer_clone = self.writer.clone();

        let resample_ratio = 16000.0 / sample_rate as f64;
        let mut resample_buf: Vec<f64> = Vec::new();
        let mut input_pos: f64 = 0.0;

        let stream = device
            .build_input_stream(
                &config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let mono: Vec<f32> = data
                        .chunks(channels as usize)
                        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                        .collect();

                    let rms =
                        (mono.iter().map(|s| s * s).sum::<f32>() / mono.len().max(1) as f32).sqrt();
                    // Smooth level a bit so the HUD doesn't flicker
                    let prev = *level_arc.lock();
                    *level_arc.lock() = prev * 0.55 + rms * 0.45;

                    resample_buf.extend(mono.iter().map(|&s| s as f64));
                    let mut out_samples: Vec<f32> = Vec::new();

                    while input_pos + 1.0 < resample_buf.len() as f64 {
                        let idx = input_pos as usize;
                        let frac = input_pos - idx as f64;
                        let sample =
                            resample_buf[idx] * (1.0 - frac) + resample_buf[idx + 1] * frac;
                        out_samples.push(sample as f32);
                        input_pos += 1.0 / resample_ratio;
                    }

                    let consumed = input_pos as usize;
                    if consumed > 0 && consumed < resample_buf.len() {
                        resample_buf.drain(..consumed);
                        input_pos -= consumed as f64;
                    }

                    if let Some(ref mut w) = *writer_clone.lock() {
                        for sample in out_samples {
                            let _ = w.write_sample(sample);
                        }
                    }
                },
                |err| eprintln!("[audio] stream error: {err}"),
                None,
            )
            .map_err(|e| format!("Failed to open microphone: {e}"))?;

        stream
            .play()
            .map_err(|e| format!("Failed to start microphone: {e}"))?;
        self.stream = Some(SendSyncStream(stream));
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.stream = None;

        if let Some(w) = self.writer.lock().take() {
            w.finalize().map_err(|e| e.to_string())?;
        }

        *self.level.lock() = 0.0;
        Ok(())
    }

    pub fn get_level(&self) -> f32 {
        *self.level.lock()
    }
}

impl Default for AudioRecorder {
    fn default() -> Self {
        Self::new()
    }
}
