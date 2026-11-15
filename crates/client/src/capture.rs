//! Screen capture: turns a display or window into a WebRTC video track.
//!
//! On macOS the system picker chooses what to share (the app cannot list displays
//! itself). On Windows the app lists displays and windows and the user picks one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use libwebrtc::desktop_capturer::{CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions};
use libwebrtc::native::yuv_helper::argb_to_i420;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::peer_connection_factory::PeerConnectionFactory;
use libwebrtc::video_frame::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};
use libwebrtc::video_source::native::NativeVideoSource;
use libwebrtc::video_source::VideoResolution;
use libwebrtc::video_track::RtcVideoTrack;

const MAX_WIDTH: u32 = 1920;
const MAX_HEIGHT: u32 = 1080;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// True where the operating system, not the app, offers the list of things to share.
pub const USES_SYSTEM_PICKER: bool = cfg!(target_os = "macos");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub id: u64,
    pub title: String,
    pub window: bool,
}

/// Displays and windows the user can share. Empty where the system picker is used.
pub fn list_sources() -> Vec<Source> {
    if USES_SYSTEM_PICKER {
        return Vec::new();
    }
    let mut sources = Vec::new();
    for (kind, window) in [(DesktopCaptureSourceType::Screen, false), (DesktopCaptureSourceType::Window, true)] {
        let Some(capturer) = DesktopCapturer::new(DesktopCapturerOptions::new(kind)) else { continue };
        for s in capturer.get_source_list() {
            let title = if s.title().is_empty() { format!("Display {}", s.id()) } else { s.title() };
            sources.push(Source { id: s.id(), title, window });
        }
    }
    sources
}

/// A running capture. Dropping it stops the capture.
pub struct Share {
    pub track: RtcVideoTrack,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Share {
    /// Starts capturing `source` (`None` opens the system picker). `on_first_frame` runs once,
    /// from the capture thread, when the first frame is ready: until then nothing is worth sending.
    /// `on_end` runs once, from the capture thread, if the capture stops by itself: the picker was
    /// cancelled, the window closed, or permission is missing. It does not run when the `Share` is dropped.
    pub fn start(
        factory: &PeerConnectionFactory,
        source: Option<Source>,
        on_first_frame: impl FnOnce() + Send + 'static,
        on_end: impl FnOnce(String) + Send + 'static,
    ) -> Result<Self, String> {
        let video = NativeVideoSource::new(VideoResolution { width: MAX_WIDTH, height: MAX_HEIGHT }, true);
        let track = factory.create_video_track("screen", video.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("screen-capture".into())
                .spawn(move || run(video, source, stop, Box::new(on_first_frame), on_end))
                .map_err(|e| e.to_string())?
        };
        Ok(Self { track, stop, thread: Some(thread) })
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    video: NativeVideoSource,
    source: Option<Source>,
    stop: Arc<AtomicBool>,
    on_first_frame: Box<dyn FnOnce() + Send>,
    on_end: impl FnOnce(String),
) {
    let kind = match &source {
        Some(s) if s.window => DesktopCaptureSourceType::Window,
        Some(_) => DesktopCaptureSourceType::Screen,
        None => generic_kind(),
    };
    let mut options = DesktopCapturerOptions::new(kind);
    options.set_include_cursor(true);
    let Some(mut capturer) = DesktopCapturer::new(options) else {
        return on_end("The screen could not be captured on this system".into());
    };
    let chosen = match source {
        Some(s) => match capturer.get_source_list().into_iter().find(|c| c.id() == s.id) {
            Some(found) => Some(found),
            None => return on_end("That window or display is gone".into()),
        },
        None => None,
    };

    // The callback runs inside `capture_frame`, so a plain cell is enough to carry its verdict out.
    let failed = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let failed_in_callback = failed.clone();
    let mut on_first_frame = Some(on_first_frame);
    capturer.start_capture(chosen, move |result| match result {
        Ok(frame) => {
            let timestamp_us = started.elapsed().as_micros() as i64;
            if let Some(buffer) = to_i420(frame.data(), frame.stride(), frame.width(), frame.height()) {
                let mut out = VideoFrame::new(VideoRotation::VideoRotation0, Box::new(buffer) as Box<dyn VideoBuffer>);
                out.timestamp_us = timestamp_us;
                video.capture_frame(&out);
                if let Some(first) = on_first_frame.take() {
                    first();
                }
            }
        }
        Err(CaptureError::Temporary) => {}
        Err(CaptureError::Permanent) => failed_in_callback.store(true, Ordering::Relaxed),
    });

    while !stop.load(Ordering::Relaxed) {
        capturer.capture_frame();
        if failed.load(Ordering::Relaxed) {
            return on_end(
                "Screen capture stopped. Check that the app is allowed to record the screen in system settings.".into(),
            );
        }
        std::thread::sleep(FRAME_INTERVAL);
    }
}

#[cfg(target_os = "macos")]
fn generic_kind() -> DesktopCaptureSourceType {
    DesktopCaptureSourceType::Generic
}

#[cfg(not(target_os = "macos"))]
fn generic_kind() -> DesktopCaptureSourceType {
    DesktopCaptureSourceType::Screen
}

/// The largest size that fits in 1080p with the same shape, rounded down to even numbers
/// because I420 stores colour at half resolution.
pub fn fit(width: u32, height: u32) -> (u32, u32) {
    let (mut w, mut h) = (width, height);
    if w > MAX_WIDTH || h > MAX_HEIGHT {
        let scale = f64::min(MAX_WIDTH as f64 / w as f64, MAX_HEIGHT as f64 / h as f64);
        w = (w as f64 * scale) as u32;
        h = (h as f64 * scale) as u32;
    }
    (w.max(2) & !1, h.max(2) & !1)
}

/// Converts a captured frame (BGRA bytes in memory) to I420, scaled down to fit 1080p.
fn to_i420(data: &[u8], stride: u32, width: i32, height: i32) -> Option<I420Buffer> {
    if width < 2 || height < 2 || (data.len() as u64) < stride as u64 * height as u64 {
        return None;
    }
    let mut buffer = I420Buffer::new(width as u32, height as u32);
    let (sy, su, sv) = buffer.strides();
    let (y, u, v) = buffer.data_mut();
    argb_to_i420(data, stride, y, sy, u, su, v, sv, width, height);
    let (w, h) = fit(width as u32, height as u32);
    if (w, h) == (width as u32, height as u32) {
        Some(buffer)
    } else {
        Some(buffer.scale(w as i32, h as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_small_sizes_and_shrinks_big_ones_to_1080p() {
        assert_eq!(fit(1280, 720), (1280, 720));
        assert_eq!(fit(1920, 1080), (1920, 1080));
        assert_eq!(fit(3840, 2160), (1920, 1080));
        // A retina laptop screen keeps its shape.
        assert_eq!(fit(3024, 1964), (1662, 1080));
        // A tall window is limited by height.
        assert_eq!(fit(1000, 2000), (540, 1080));
        // Odd sizes become even.
        assert_eq!(fit(1001, 601), (1000, 600));
    }

    #[test]
    fn a_solid_frame_converts_and_scales() {
        let (w, h) = (3840, 2160);
        let data = vec![200u8; (w * h * 4) as usize];
        let out = to_i420(&data, w as u32 * 4, w, h).unwrap();
        assert_eq!((out.width(), out.height()), (1920, 1080));
        assert!(to_i420(&data[..100], w as u32 * 4, w, h).is_none());
    }
}
