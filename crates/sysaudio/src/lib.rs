//! Captures what the computer is playing, without the app's own sound.
//!
//! The app plays other people's voices through the same speakers, so the capture must leave
//! the app out or everyone would hear themselves come back.
//!
//! - macOS: ScreenCaptureKit audio, with the current process excluded (macOS 13 or newer,
//!   needs the Screen Recording permission).
//! - Windows: WASAPI process loopback, excluding this process (Windows 10 build 2004 or newer).

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "windows")]
mod win;

/// Samples per second, per channel, of everything delivered to the callback.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// 10 ms, the unit WebRTC consumes audio in.
pub const CHUNK_FRAMES: usize = SAMPLE_RATE as usize / 100;

/// A running capture. Dropping it stops the capture.
pub struct Capture {
    _inner: Backend,
}

#[cfg(target_os = "macos")]
type Backend = mac::Backend;
#[cfg(target_os = "windows")]
type Backend = win::Backend;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
type Backend = ();

/// Starts capturing. `on_chunk` receives interleaved stereo `i16` samples at 48 kHz, in chunks
/// of exactly [`CHUNK_FRAMES`] frames, from a thread owned by the capture.
pub fn start(on_chunk: impl FnMut(&[i16]) + Send + 'static) -> Result<Capture, String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        Backend::start(Chunker::new(on_chunk)).map(|inner| Capture { _inner: inner })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = on_chunk;
        Err("sharing system audio is not supported on this system".into())
    }
}

/// Gathers samples of any batch size into chunks of exactly [`CHUNK_FRAMES`] frames.
pub(crate) struct Chunker {
    pending: Vec<i16>,
    emit: Box<dyn FnMut(&[i16]) + Send>,
}

#[allow(dead_code)] // Unused on systems without a capture backend.
impl Chunker {
    pub(crate) fn new(emit: impl FnMut(&[i16]) + Send + 'static) -> Self {
        Self { pending: Vec::with_capacity(CHUNK_FRAMES * CHANNELS * 2), emit: Box::new(emit) }
    }

    /// Adds interleaved samples.
    pub(crate) fn push(&mut self, samples: &[i16]) {
        self.pending.extend_from_slice(samples);
        let size = CHUNK_FRAMES * CHANNELS;
        let whole = self.pending.len() / size * size;
        for chunk in self.pending[..whole].chunks_exact(size) {
            (self.emit)(chunk);
        }
        self.pending.drain(..whole);
    }

    /// Adds float samples, interleaved, in the range -1 to 1.
    pub(crate) fn push_f32(&mut self, samples: &[f32]) {
        let converted: Vec<i16> = samples.iter().map(|&s| to_i16(s)).collect();
        self.push(&converted);
    }

    /// Adds float samples with one slice per channel, as ScreenCaptureKit delivers them.
    /// A mono source is played on both sides; channels past the second are dropped.
    pub(crate) fn push_planar(&mut self, channels: &[&[f32]]) {
        let Some(first) = channels.first() else { return };
        let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0).min(first.len());
        let mut out = Vec::with_capacity(frames * CHANNELS);
        for i in 0..frames {
            let left = channels[0][i];
            let right = channels.get(1).map_or(left, |c| c[i]);
            out.push(to_i16(left));
            out.push(to_i16(right));
        }
        self.push(&out);
    }
}

fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn collecting() -> (Chunker, Arc<Mutex<Vec<Vec<i16>>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (Chunker::new(move |c| sink.lock().unwrap().push(c.to_vec())), seen)
    }

    #[test]
    fn chunks_are_exact_whatever_the_batch_size() {
        let (mut chunker, seen) = collecting();
        let size = CHUNK_FRAMES * CHANNELS;
        chunker.push(&vec![1; size / 2]);
        assert!(seen.lock().unwrap().is_empty());
        chunker.push(&vec![2; size * 2]);
        let chunks = seen.lock().unwrap();
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|c| c.len() == size));
        // The leftover half chunk of 1s came first, then 2s.
        assert_eq!(chunks[0][0], 1);
        assert_eq!(chunks[0][size - 1], 2);
    }

    #[test]
    fn floats_are_clamped_and_scaled() {
        assert_eq!(to_i16(0.0), 0);
        assert_eq!(to_i16(1.0), i16::MAX);
        assert_eq!(to_i16(5.0), i16::MAX);
        assert_eq!(to_i16(-5.0), -i16::MAX);
    }

    #[test]
    fn planar_channels_are_interleaved_and_mono_is_doubled() {
        let (mut chunker, seen) = collecting();
        let left = vec![0.5f32; CHUNK_FRAMES];
        let right = vec![-0.5f32; CHUNK_FRAMES];
        chunker.push_planar(&[&left, &right]);
        chunker.push_planar(&[&left]);
        let chunks = seen.lock().unwrap();
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0][0] > 0 && chunks[0][1] < 0);
        assert_eq!(chunks[1][0], chunks[1][1]);
    }
}
