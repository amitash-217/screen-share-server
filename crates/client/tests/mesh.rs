//! The audio mesh against itself: real WebRTC peers in one process, a switchboard
//! standing in for the server, and tones standing in for microphones.
//! No database and no audio hardware needed.

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use client::model::LinkState;
use client::rtc::{spawn_mesh, MeshEvent, MeshHandle};
use futures_util::StreamExt;
use libwebrtc::audio_frame::AudioFrame;
use libwebrtc::audio_source::native::NativeAudioSource;
use libwebrtc::audio_source::AudioSourceOptions;
use libwebrtc::audio_stream::native::NativeAudioStream;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::video_frame::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};
use libwebrtc::video_source::native::NativeVideoSource;
use libwebrtc::video_stream::native::NativeVideoStream;
use libwebrtc::video_source::VideoResolution;
use libwebrtc::video_track::RtcVideoTrack;
use libwebrtc::peer_connection_factory::PeerConnectionFactory;
use protocol::{ClientMsg, UserId};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

const RATE: u32 = 48_000;

struct Node {
    factory: PeerConnectionFactory,
    handle: MeshHandle,
    events: UnboundedReceiver<MeshEvent>,
    /// Keeps the tone running while the test holds the node.
    _tone: tokio::task::JoinHandle<()>,
}

/// Sends a steady tone into `source` until aborted.
fn play_tone(source: NativeAudioSource, hz: f32) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut phase = 0f32;
        loop {
            let samples: Vec<i16> = (0..480)
                .map(|_| {
                    phase += 2.0 * std::f32::consts::PI * hz / RATE as f32;
                    (phase.sin() * 12_000.0) as i16
                })
                .collect();
            let frame = AudioFrame { data: Cow::Owned(samples), sample_rate: RATE, num_channels: 1, samples_per_channel: 480 };
            if source.capture_frame(&frame).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
}

/// Builds nodes whose signals reach each other, like the server would relay them.
fn network(ids: &[UserId]) -> HashMap<UserId, Node> {
    network_with(ids, true)
}

/// Like `network`; with `voices` false the microphones are silent.
fn network_with(ids: &[UserId], voices: bool) -> HashMap<UserId, Node> {
    let factory = PeerConnectionFactory::default();
    let mut outs = Vec::new();
    let mut nodes = HashMap::new();
    for &id in ids {
        let source = NativeAudioSource::new(AudioSourceOptions::default(), RATE, 1, 1000);
        let track = factory.create_audio_track("mic", source.clone());
        let (sig_tx, sig_rx) = unbounded_channel();
        let (ev_tx, ev_rx) = unbounded_channel();
        let handle = spawn_mesh(&tokio::runtime::Handle::current(), factory.clone(), track, &[], id, sig_tx, ev_tx);
        outs.push((id, sig_rx));
        nodes.insert(id, Node { factory: factory.clone(), handle, events: ev_rx, _tone: play_tone(source, if voices { 300.0 + 100.0 * id as f32 } else { 0.0 }) });
    }
    let handles: HashMap<UserId, MeshHandle> = nodes.iter().map(|(id, n)| (*id, n.handle.clone())).collect();
    for (from, mut rx) in outs {
        let handles = handles.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let ClientMsg::Signal { to, payload } = msg {
                    if let Some(h) = handles.get(&to) {
                        h.signal(from, payload);
                    }
                }
            }
        });
    }
    nodes
}

/// Reads events until `done` says so, or fails after `secs`.
async fn until(node: &mut Node, secs: u64, mut done: impl FnMut(&MeshEvent) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, node.events.recv()).await {
            Ok(Some(ev)) => {
                if done(&ev) {
                    return;
                }
            }
            _ => panic!("timed out waiting for a mesh event"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_people_connect_and_hear_each_other() {
    let mut nodes = network(&[1, 2]);
    // Person 2 enters after person 1, so person 2 dials.
    nodes[&2].handle.dial(1);

    let mut b = nodes.remove(&2).unwrap();
    let mut a = nodes.remove(&1).unwrap();

    // B hears A's tone: pull the decoded audio from the remote track.
    let mut heard = None;
    until(&mut b, 15, |ev| match ev {
        MeshEvent::RemoteTrack { user: 1, track, system: false } => {
            heard = Some(NativeAudioStream::new(track.clone(), RATE as i32, 1));
            true
        }
        _ => false,
    })
    .await;
    let mut stream = heard.expect("remote track");
    let mut samples: Vec<i16> = Vec::new();
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        if let Ok(Some(frame)) = tokio::time::timeout(Duration::from_millis(300), stream.next()).await {
            samples.extend_from_slice(&frame.data);
        }
    }
    let rms = (samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt();
    assert!(samples.len() > 48_000, "only {} samples arrived", samples.len());
    assert!(rms > 1_000.0, "audio arrived but is nearly silent: rms {rms}");

    // A, who was dialed, also connects and sees B's audio arrive.
    until(&mut a, 15, |ev| matches!(ev, MeshEvent::RemoteTrack { user: 2, .. })).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn levels_show_who_is_talking() {
    let mut nodes = network(&[1, 2]);
    nodes[&2].handle.dial(1);
    let mut b = nodes.remove(&2).unwrap();

    // Drain the remote audio so WebRTC decodes it; levels come from what was played out.
    let mut drains = Vec::new();
    let mut loud_remote = false;
    let mut loud_local = false;
    until(&mut b, 20, |ev| {
        match ev {
            MeshEvent::RemoteTrack { track, .. } => {
                let mut stream = NativeAudioStream::new(track.clone(), RATE as i32, 1);
                drains.push(tokio::spawn(async move { while stream.next().await.is_some() {} }));
            }
            MeshEvent::Level { user: 1, level } if *level > 0.01 => loud_remote = true,
            MeshEvent::Level { user: 2, level } if *level > 0.01 => loud_local = true,
            _ => {}
        }
        loud_remote && loud_local
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_third_person_connects_to_both() {
    let mut nodes = network(&[1, 2, 3]);
    nodes[&2].handle.dial(1);
    // Wait for 2 and 1 to settle before 3 enters and dials both, as the server would order it.
    let mut n2 = nodes.remove(&2).unwrap();
    until(&mut n2, 20, |ev| matches!(ev, MeshEvent::Link { user: 1, state: LinkState::Connected })).await;

    nodes[&3].handle.dial(1);
    nodes[&3].handle.dial(2);
    let mut n3 = nodes.remove(&3).unwrap();
    let mut connected = std::collections::HashSet::new();
    until(&mut n3, 20, |ev| {
        if let MeshEvent::Link { user, state: LinkState::Connected } = ev {
            connected.insert(*user);
        }
        connected.len() == 2
    })
    .await;

    // Person 2 now has a link to person 3 as well.
    until(&mut n2, 20, |ev| matches!(ev, MeshEvent::Link { user: 3, state: LinkState::Connected })).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_signals_do_not_crash_the_mesh() {
    let mut nodes = network(&[1]);
    let node = nodes.get_mut(&1).unwrap();
    node.handle.signal(9, serde_json::json!({ "kind": "nonsense" }));
    node.handle.signal(9, serde_json::json!("not even an object"));
    node.handle.signal(9, serde_json::json!({ "kind": "candidate", "mid": "0", "index": 0, "sdp": "candidate:x" }));
    node.handle.signal(9, serde_json::json!({ "kind": "answer", "sdp": "v=0" }));
    node.handle.peer_left(9);
    // The mesh survives all of that: the first two fail to parse and report the sender as lost.
    until(node, 10, |ev| matches!(ev, MeshEvent::Link { user: 9, state: LinkState::Lost })).await;
}

/// A 640x360 video track fed with frames until the returned task is aborted.
fn fake_screen(factory: &PeerConnectionFactory) -> (RtcVideoTrack, tokio::task::JoinHandle<()>) {
    let source = NativeVideoSource::new(VideoResolution { width: 640, height: 360 }, true);
    let track = factory.create_video_track("screen", source.clone());
    let feeder = tokio::spawn(async move {
        loop {
            let frame = VideoFrame::new(VideoRotation::VideoRotation0, Box::new(I420Buffer::new_black(640, 360)) as Box<dyn VideoBuffer>);
            source.capture_frame(&frame);
            tokio::time::sleep(Duration::from_millis(33)).await;
        }
    });
    (track, feeder)
}

/// Waits for a frame of `user`'s screen to arrive at `node` and returns its size. After a
/// renegotiation the track can be replaced, so every track announced is tried.
async fn watch(node: &mut Node, user: UserId) -> (u32, u32) {
    let (found, mut frames) = unbounded_channel();
    let mut readers = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        tokio::select! {
            Some(size) = frames.recv() => return size,
            event = tokio::time::timeout_at(deadline.into(), node.events.recv()) => match event {
                Ok(Some(MeshEvent::RemoteVideo { user: u, track })) if u == user => {
                    let found = found.clone();
                    readers.push(tokio::spawn(async move {
                        let mut stream = NativeVideoStream::new(track);
                        if let Some(frame) = stream.next().await {
                            let _ = found.send((frame.buffer.width(), frame.buffer.height()));
                        }
                    }));
                }
                Ok(Some(_)) => {}
                _ => panic!("no frame of user {user}'s screen arrived"),
            },
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_share_starts_arrives_and_ends() {
    let mut nodes = network(&[1, 2]);
    nodes[&2].handle.dial(1);
    let mut b = nodes.remove(&2).unwrap();
    let a = nodes.remove(&1).unwrap();
    until(&mut b, 20, |ev| matches!(ev, MeshEvent::Link { user: 1, state: LinkState::Connected })).await;

    let (track, _feeder) = fake_screen(&a.factory);
    a.handle.start_share(track);
    let (w, h) = watch(&mut b, 1).await;
    assert!(w > 0 && h > 0, "empty frame {w}x{h}");

    a.handle.stop_share();
    until(&mut b, 20, |ev| matches!(ev, MeshEvent::ShareEnded { user: 1 })).await;

    // The same connection carries a second share afterwards.
    let (track, _feeder2) = fake_screen(&a.factory);
    a.handle.start_share(track);
    watch(&mut b, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn someone_who_enters_during_a_share_sees_it() {
    let mut nodes = network(&[1, 2]);
    let a = nodes.remove(&1).unwrap();
    let (track, _feeder) = fake_screen(&a.factory);
    a.handle.start_share(track);

    // Person 2 enters later and dials the sharer.
    nodes[&2].handle.dial(1);
    let mut b = nodes.remove(&2).unwrap();
    watch(&mut b, 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_people_can_start_sharing_at_the_same_moment() {
    let mut nodes = network(&[1, 2]);
    nodes[&2].handle.dial(1);
    let mut b = nodes.remove(&2).unwrap();
    let mut a = nodes.remove(&1).unwrap();
    until(&mut b, 20, |ev| matches!(ev, MeshEvent::Link { user: 1, state: LinkState::Connected })).await;

    let (ta, _fa) = fake_screen(&a.factory);
    let (tb, _fb) = fake_screen(&b.factory);
    a.handle.start_share(ta);
    b.handle.start_share(tb);
    let (seen_by_b, seen_by_a) = tokio::join!(watch(&mut b, 1), watch(&mut a, 2));
    assert!(seen_by_b.0 > 0 && seen_by_a.0 > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn computer_sound_arrives_as_its_own_track_and_is_not_speech() {
    let mut nodes = network_with(&[1, 2], false);
    nodes[&2].handle.dial(1);
    let a = nodes.remove(&1).unwrap();
    let mut b = nodes.remove(&2).unwrap();
    // 2 s of a 700 Hz tone as the computer's sound, in 10 ms chunks.
    let system = a.handle.system_audio();
    let _feeder = tokio::spawn(async move {
        let mut phase = 0f32;
        loop {
            let mut samples = Vec::with_capacity(960);
            for _ in 0..480 {
                phase += 2.0 * std::f32::consts::PI * 700.0 / RATE as f32;
                let v = (phase.sin() * 12_000.0) as i16;
                samples.extend([v, v]);
            }
            let frame = AudioFrame { data: Cow::Owned(samples), sample_rate: RATE, num_channels: 2, samples_per_channel: 480 };
            if system.capture_frame(&frame).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    let mut heard = None;
    let mut loudest_voice = 0f32;
    until(&mut b, 20, |ev| {
        match ev {
            MeshEvent::RemoteTrack { user: 1, track, system: true } => {
                heard = Some(NativeAudioStream::new(track.clone(), RATE as i32, 2));
            }
            MeshEvent::Level { user: 1, level } => loudest_voice = loudest_voice.max(*level),
            _ => {}
        }
        heard.is_some()
    })
    .await;
    let mut stream = heard.expect("system track announced");
    let mut samples: Vec<i16> = Vec::new();
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        tokio::select! {
            frame = stream.next() => if let Some(frame) = frame { samples.extend_from_slice(&frame.data); },
            // Keep reading levels while the audio plays.
            Some(ev) = b.events.recv() => if let MeshEvent::Level { user: 1, level } = ev { loudest_voice = loudest_voice.max(level) },
            _ = tokio::time::sleep_until(end.into()) => {}
        }
    }
    let rms = (samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt();
    assert!(rms > 1_000.0, "computer sound did not arrive: rms {rms} over {} samples", samples.len());
    assert!(loudest_voice < 0.01, "computer sound counted as speech: level {loudest_voice}");
}

/// The app starts the mesh from the window's thread, which is not inside the tokio runtime.
#[test]
fn the_mesh_starts_from_a_thread_outside_the_runtime() {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let factory = PeerConnectionFactory::default();
    let source = NativeAudioSource::new(AudioSourceOptions::default(), RATE, 1, 1000);
    let track = factory.create_audio_track("mic", source);
    let (sig_tx, _sig_rx) = unbounded_channel();
    let (ev_tx, _ev_rx) = unbounded_channel();
    let handle = std::thread::spawn({
        let rt = runtime.handle().clone();
        move || spawn_mesh(&rt, factory, track, &[], 1, sig_tx, ev_tx)
    })
    .join()
    .expect("starting the mesh must not panic");
    handle.stop_share();
}
