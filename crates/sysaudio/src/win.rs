//! Windows: WASAPI process loopback in "exclude" mode captures everything being played except
//! this process (Windows 10 build 2004 or newer).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;

use wasapi::{initialize_mta, AudioClient, Direction, SampleType, StreamMode, WaveFormat};

use crate::{Chunker, CHANNELS, SAMPLE_RATE};

pub(crate) struct Backend {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Backend {
    pub(crate) fn start(chunker: Chunker) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("system-audio".into())
                .spawn(move || capture(chunker, stop, ready_tx))
                .map_err(|e| e.to_string())?
        };
        // The thread reports whether the capture could start, so errors reach the caller.
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the system audio thread stopped before it started".into()),
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn capture(mut chunker: Chunker, stop: Arc<AtomicBool>, ready: mpsc::Sender<Result<(), String>>) {
    let started = (|| -> Result<_, String> {
        initialize_mta().ok().map_err(|e| e.to_string())?;
        let format = WaveFormat::new(32, 32, SampleType::Float, SAMPLE_RATE as usize, CHANNELS, None);
        // `include_tree: false` means everything except this process.
        let mut client = AudioClient::new_application_loopback_client(std::process::id(), false)
            .map_err(|e| format!("system audio needs Windows 10 version 2004 or newer ({e})"))?;
        let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
        client.initialize_client(&format, Direction::Capture, mode, None).map_err(|e| e.to_string())?;
        let event = client.set_get_eventhandle().map_err(|e| e.to_string())?;
        let reader = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
        client.start_stream().map_err(|e| e.to_string())?;
        Ok((client, event, reader, format.get_blockalign() as usize))
    })();
    let (client, event, reader, block) = match started {
        Ok(parts) => {
            let _ = ready.send(Ok(()));
            parts
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    let mut queue: VecDeque<u8> = VecDeque::new();
    while !stop.load(Ordering::Relaxed) {
        // No sound playing means no packets, so a timeout only gives the stop flag a look.
        if event.wait_for_event(100).is_err() {
            continue;
        }
        while reader.get_next_packet_size().ok().flatten().unwrap_or(0) > 0 {
            if reader.read_from_device_to_deque(&mut queue).is_err() {
                break;
            }
        }
        let whole = queue.len() / block * block;
        if whole == 0 {
            continue;
        }
        let bytes: Vec<u8> = queue.drain(..whole).collect();
        let samples: Vec<f32> =
            bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        chunker.push_f32(&samples);
    }
    let _ = client.stop_stream();
}
