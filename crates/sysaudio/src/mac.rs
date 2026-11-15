//! macOS: ScreenCaptureKit delivers system audio as part of a screen stream. We ask for a
//! tiny video size and ignore the pictures.

use std::sync::Mutex;

use screencapturekit::prelude::*;

use crate::{Chunker, CHANNELS, SAMPLE_RATE};

pub(crate) struct Backend {
    stream: SCStream,
}

struct AudioOut(Mutex<Chunker>);

impl SCStreamOutputTrait for AudioOut {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, _kind: SCStreamOutputType) {
        let Ok(list) = sample.audio_buffer_list() else { return };
        let mut chunker = self.0.lock().unwrap();
        let floats: Vec<Vec<f32>> = list.iter().map(|b| to_f32(b.data())).collect();
        match list.get(0).map(|b| b.number_channels as usize) {
            // Interleaved: one buffer holds every channel.
            Some(n) if n > 1 && floats.len() == 1 => {
                let samples = &floats[0];
                if n == CHANNELS {
                    chunker.push_f32(samples);
                } else {
                    // More than stereo: keep the first two channels of each frame.
                    let frames: Vec<f32> = samples.chunks_exact(n).flat_map(|f| [f[0], f[1]]).collect();
                    chunker.push_f32(&frames);
                }
            }
            // Planar: one buffer per channel.
            Some(_) => {
                let channels: Vec<&[f32]> = floats.iter().map(|c| c.as_slice()).collect();
                chunker.push_planar(&channels);
            }
            None => {}
        }
    }
}

/// Screen frames must have somewhere to go, or the stream logs an error for every one.
struct IgnoreVideo;

impl SCStreamOutputTrait for IgnoreVideo {
    fn did_output_sample_buffer(&self, _sample: CMSampleBuffer, _kind: SCStreamOutputType) {}
}

fn to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

impl Backend {
    pub(crate) fn start(chunker: Chunker) -> Result<Self, String> {
        let content = SCShareableContent::get().map_err(|e| {
            format!("Could not read the screen list ({e}). Allow Screen Recording for this app in System Settings.")
        })?;
        let display = content.displays().into_iter().next().ok_or("no display found")?;
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build()
            .map_err(|e| e.to_string())?;
        let config = SCStreamConfiguration::new()
            .with_width(2)
            .with_height(2)
            .with_captures_audio(true)
            .with_excludes_current_process_audio(true)
            .with_sample_rate(SAMPLE_RATE as i32)
            .with_channel_count(CHANNELS as i32);
        let mut stream = SCStream::new(&filter, &config).map_err(|e| e.to_string())?;
        stream.add_output_handler(IgnoreVideo, SCStreamOutputType::Screen).map_err(|e| e.to_string())?;
        stream
            .add_output_handler(AudioOut(Mutex::new(chunker)), SCStreamOutputType::Audio)
            .map_err(|e| e.to_string())?;
        stream.start_capture().map_err(|e| e.to_string())?;
        Ok(Self { stream })
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.stream.stop_capture();
    }
}
