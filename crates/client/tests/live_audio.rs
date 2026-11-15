//! Needs a real desktop session and microphone permission for the terminal.
//! Run with `cargo test -p client --test live_audio -- --ignored --nocapture`.

use std::time::Duration;

use client::audio::AudioEngine;

#[test]
#[ignore = "needs a microphone and permissions"]
fn the_real_microphone_opens_and_closes() {
    let engine = AudioEngine::start().expect("audio layer starts");
    println!("inputs: {:?}", engine.inputs());
    println!("outputs: {:?}", engine.outputs());
    let track = engine.local_track();
    println!("state: {}", engine.describe());
    assert!(engine.describe().contains("recording initialized=true"), "the microphone did not open");
    println!("microphone track created: {}", track.id());
    std::thread::sleep(Duration::from_secs(2));
    drop(track);
    drop(engine);
    println!("closed cleanly");
}
