//! Received screen frames, converted for the window.

use libwebrtc::video_frame::{VideoBuffer, VideoFormatType};
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Converts any received frame to RGBA pixels, or `None` for an empty frame.
pub fn to_rgba(buffer: &dyn VideoBuffer) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let (w, h) = (buffer.width(), buffer.height());
    if w == 0 || h == 0 {
        return None;
    }
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let bytes = pixels.make_mut_bytes();
    // libyuv names formats by word order, so the bytes in memory are the reverse of the name.
    buffer.to_argb(VideoFormatType::ABGR, bytes, w * 4, w as i32, h as i32);
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libwebrtc::video_frame::I420Buffer;

    #[test]
    fn red_stays_red() {
        let mut buf = I420Buffer::new(16, 16);
        let (sy, su, sv) = buf.strides();
        let (y, u, v) = buf.data_mut();
        // Full-range-ish BT.601 red.
        y[..(sy * 16) as usize].fill(82);
        u[..(su * 8) as usize].fill(90);
        v[..(sv * 8) as usize].fill(240);
        let rgba = to_rgba(&buf).unwrap();
        let p = rgba.as_slice()[0];
        assert!(p.r > 200 && p.g < 60 && p.b < 60 && p.a == 255, "got {p:?}");
    }
}
