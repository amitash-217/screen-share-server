//! Microphone and speakers, through libwebrtc's platform audio layer.
//!
//! Acquiring the layer only lists devices. The microphone opens when the
//! first local track is created, which is when the user enters a room.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use libwebrtc::audio_track::RtcAudioTrack;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::peer_connection_factory::PeerConnectionFactory;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Stable across plugging devices in and out; `default` is the system default.
    pub guid: String,
    pub name: String,
}

pub struct AudioEngine {
    factory: PeerConnectionFactory,
    track: Mutex<Option<RtcAudioTrack>>,
    muted: AtomicBool,
}

impl AudioEngine {
    pub fn start() -> Result<Self, String> {
        let factory = PeerConnectionFactory::default();
        if !factory.acquire_platform_adm() {
            return Err("the system audio layer could not be started".into());
        }
        // Without this, remote audio is only delivered to callbacks and never played.
        factory.set_adm_playout_enabled(true);
        // Off by default: WebRTC's recording calls are then no-ops, so the microphone
        // never opens, the OS never asks for permission and nothing is sent.
        factory.set_adm_recording_enabled(true);
        Ok(Self { factory, track: Mutex::new(None), muted: AtomicBool::new(false) })
    }

    pub fn factory(&self) -> PeerConnectionFactory {
        self.factory.clone()
    }

    pub fn inputs(&self) -> Vec<Device> {
        (0..self.factory.recording_devices().max(0) as u16)
            .map(|i| Device { guid: self.factory.recording_device_guid(i), name: self.factory.recording_device_name(i) })
            .collect()
    }

    pub fn outputs(&self) -> Vec<Device> {
        (0..self.factory.playout_devices().max(0) as u16)
            .map(|i| Device { guid: self.factory.playout_device_guid(i), name: self.factory.playout_device_name(i) })
            .collect()
    }

    /// One line of audio state for the log, to see why nothing is heard.
    pub fn describe(&self) -> String {
        format!(
            "playout enabled={} initialized={} outputs={:?}; recording initialized={} inputs={:?}",
            self.factory.adm_playout_enabled(),
            self.factory.playout_is_initialized(),
            self.outputs().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            self.factory.recording_is_initialized(),
            self.inputs().iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        )
    }

    /// Switches the microphone. Returns false if the device is unknown.
    pub fn select_input(&self, guid: &str) -> bool {
        let running = self.factory.recording_is_initialized();
        self.factory.stop_recording();
        let ok = self.factory.set_recording_device_by_guid(guid);
        if running {
            self.factory.init_recording();
            self.factory.start_recording();
        }
        ok
    }

    /// Switches the speakers. Returns false if the device is unknown.
    pub fn select_output(&self, guid: &str) -> bool {
        let running = self.factory.playout_is_initialized();
        self.factory.stop_playout();
        let ok = self.factory.set_playout_device_by_guid(guid);
        if running {
            self.factory.init_playout();
            self.factory.start_playout();
        }
        ok
    }

    /// The microphone track, created on first use. This is what opens the microphone.
    pub fn local_track(&self) -> RtcAudioTrack {
        let mut slot = self.track.lock().unwrap();
        slot.get_or_insert_with(|| {
            let track = self.factory.create_device_audio_track("microphone");
            track.set_enabled(!self.muted.load(Ordering::Relaxed));
            track
        })
        .clone()
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
        if let Some(track) = self.track.lock().unwrap().as_ref() {
            track.set_enabled(!muted);
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.factory.release_platform_adm();
    }
}
