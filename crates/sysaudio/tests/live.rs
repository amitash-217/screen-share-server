//! Needs a real desktop session and, on macOS, Screen Recording permission for the terminal.
//! Run with `cargo test -p sysaudio -- --ignored --nocapture`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
#[ignore = "needs a desktop session and permissions"]
fn captures_chunks_of_the_expected_size() {
    let chunks = Arc::new(AtomicUsize::new(0));
    let counter = chunks.clone();
    let capture = sysaudio::start(move |c| {
        assert_eq!(c.len(), sysaudio::CHUNK_FRAMES * sysaudio::CHANNELS);
        counter.fetch_add(1, Ordering::Relaxed);
    })
    .expect("capture starts");
    std::thread::sleep(Duration::from_secs(3));
    drop(capture);
    let n = chunks.load(Ordering::Relaxed);
    println!("{n} chunks in 3 s (300 expected)");
    assert!(n > 100, "only {n} chunks arrived");
}
