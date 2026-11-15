//! Audio mesh: one WebRTC peer connection to every other person in the room.
//!
//! Whoever enters a room dials the people already inside, so two people never
//! send offers to each other at the same time. Offers, answers and ICE
//! candidates travel as `Signal` messages through the server.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use libwebrtc::audio_source::native::NativeAudioSource;
use libwebrtc::audio_source::AudioSourceOptions;
use libwebrtc::audio_track::RtcAudioTrack;
use libwebrtc::ice_candidate::IceCandidate;
use libwebrtc::media_stream_track::MediaStreamTrack;
use libwebrtc::peer_connection::{PeerConnection, PeerConnectionState, SignalingState};
use libwebrtc::peer_connection_factory::{IceServer as RtcIceServer, PeerConnectionFactory, RtcConfiguration};
use libwebrtc::rtp_parameters::DegradationPreference;
use libwebrtc::rtp_sender::RtpSender;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::video_source::native::NativeVideoSource;
use libwebrtc::video_source::VideoResolution;
use libwebrtc::video_track::RtcVideoTrack;
use libwebrtc::session_description::{SdpType, SessionDescription};
use libwebrtc::stats::RtcStats;
use libwebrtc::peer_connection::{AnswerOptions, OfferOptions};
use protocol::{ClientMsg, IceServer, RtcSignal, UserId};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::model::LinkState;

const LEVEL_POLL: Duration = Duration::from_millis(250);
/// Screen shares are capped at 1080p30; WebRTC adapts below this when the link is slow.
const SHARE_MAX_BITRATE: u64 = 6_000_000;
const SHARE_MAX_FPS: f64 = 30.0;

#[derive(Debug)]
pub enum MeshEvent {
    /// The media connection to `user` changed state.
    Link { user: UserId, state: LinkState },
    /// Audio level (0 to 1) of `user`, or of the local microphone when `user` is the local user.
    Level { user: UserId, level: f32 },
    /// Audio from `user` arrived. The platform audio layer plays it by itself;
    /// tests attach a stream to read it.
    /// `system` is true for the computer sound someone shares, false for their voice.
    RemoteTrack { user: UserId, track: RtcAudioTrack, system: bool },
    /// `user` started sharing their screen; read frames from `track`.
    RemoteVideo { user: UserId, track: RtcVideoTrack },
    /// `user` stopped sharing.
    ShareEnded { user: UserId },
}

enum Cmd {
    Dial(UserId),
    Signal { from: UserId, payload: serde_json::Value },
    PeerLeft(UserId),
    StartShare(RtcVideoTrack),
    StopShare,
}

/// Handle to the mesh. Dropping the last clone closes every connection.
#[derive(Clone)]
pub struct MeshHandle {
    tx: UnboundedSender<Cmd>,
    system: NativeAudioSource,
}

impl MeshHandle {
    /// Start a connection to someone who was already in the room when we entered.
    pub fn dial(&self, user: UserId) {
        let _ = self.tx.send(Cmd::Dial(user));
    }

    /// Feed in a `Signal` payload received from `from`.
    pub fn signal(&self, from: UserId, payload: serde_json::Value) {
        let _ = self.tx.send(Cmd::Signal { from, payload });
    }

    pub fn peer_left(&self, user: UserId) {
        let _ = self.tx.send(Cmd::PeerLeft(user));
    }

    /// Sends `track` to everyone in the room, and to anyone who enters while it lasts.
    pub fn start_share(&self, track: RtcVideoTrack) {
        let _ = self.tx.send(Cmd::StartShare(track));
    }

    /// Where the computer's sound goes while sharing: 48 kHz stereo. It travels as its own audio
    /// track, so listeners hear it next to the voice and it never trips the speaking indicator.
    pub fn system_audio(&self) -> NativeAudioSource {
        self.system.clone()
    }

    pub fn stop_share(&self) {
        let _ = self.tx.send(Cmd::StopShare);
    }
}

/// Starts the mesh actor on `rt`. `signal_out` receives the `Signal` messages to send to the server.
pub fn spawn_mesh(
    rt: &tokio::runtime::Handle,
    factory: PeerConnectionFactory,
    local: RtcAudioTrack,
    ice: &[IceServer],
    me: UserId,
    signal_out: UnboundedSender<ClientMsg>,
    events: UnboundedSender<MeshEvent>,
) -> MeshHandle {
    // libwebrtc's sources spawn tokio tasks when created, and the UI thread is not in the runtime.
    let _runtime = rt.enter();
    let (tx, cmds) = unbounded_channel();
    let ice = ice
        .iter()
        .map(|s| RtcIceServer {
            urls: s.urls.clone(),
            username: s.username.clone().unwrap_or_default(),
            password: s.credential.clone().unwrap_or_default(),
        })
        .collect();
    let (inner_tx, inner_rx) = unbounded_channel();
    let idle = factory.create_video_track("idle", NativeVideoSource::new(VideoResolution { width: 320, height: 180 }, true));
    let system_source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 2, 1000);
    let system = factory.create_audio_track("system", system_source.clone());
    let mesh = Mesh { factory, local, ice, me, signal_out, events, inner_tx, links: HashMap::new(), share: None, idle, system, system_ids: HashSet::new() };
    rt.spawn(mesh.run(cmds, inner_rx));
    MeshHandle { tx, system: system_source }
}

/// Things libwebrtc's callback threads report back to the actor.
enum Inner {
    Track(UserId, RtcAudioTrack, bool),
    Video(UserId, RtcVideoTrack),
    State(UserId, PeerConnectionState),
}

struct Link {
    pc: PeerConnection,
    /// Candidates that arrived before the remote description was set.
    pending: Vec<IceCandidate>,
    remote_set: bool,
    /// Kept so the receiver is not dropped.
    _remote: Option<RtcAudioTrack>,
    /// Our screen's sender. Kept after a share ends and reused by the next one: WebRTC will not
    /// reuse a removed sender's slot, and the side that answered cannot add a new one.
    video: Option<RtpSender>,
    /// Their screen's track, kept for when they share again.
    remote_video: Option<RtcVideoTrack>,
    /// Whether their screen is currently shown.
    remote_sharing: bool,
    /// A change was wanted while an offer was already in flight; offer again once stable.
    dirty: bool,
    /// We sent the first offer. Only that side offers afterwards, so two offers never cross.
    dialed: bool,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.pc.close();
        // webrtc-sys can still deliver connection-state callbacks to an observer that is freed with
        // the last handle, which crashes the process. Keep a handle alive until they have drained.
        let pc = self.pc.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            drop(pc);
        });
    }
}

struct Mesh {
    factory: PeerConnectionFactory,
    local: RtcAudioTrack,
    ice: Vec<RtcIceServer>,
    me: UserId,
    signal_out: UnboundedSender<ClientMsg>,
    events: UnboundedSender<MeshEvent>,
    inner_tx: UnboundedSender<Inner>,
    links: HashMap<UserId, Link>,
    /// Our screen, while we share it.
    share: Option<RtcVideoTrack>,
    /// What an idle sender carries: a sender cannot be emptied (`set_track(None)` crashes webrtc-sys).
    idle: RtcVideoTrack,
    /// Carries the computer's sound. Present on every connection from the start, because the
    /// side that answered cannot add tracks later.
    system: RtcAudioTrack,
    /// Ids of the other people's system-sound tracks, to keep them out of the speaking levels.
    system_ids: HashSet<String>,
}

fn link_state(s: PeerConnectionState) -> LinkState {
    match s {
        PeerConnectionState::New | PeerConnectionState::Connecting => LinkState::Connecting,
        PeerConnectionState::Connected => LinkState::Connected,
        PeerConnectionState::Disconnected | PeerConnectionState::Failed | PeerConnectionState::Closed => LinkState::Lost,
    }
}

impl Mesh {
    async fn run(mut self, mut cmds: UnboundedReceiver<Cmd>, mut inner: UnboundedReceiver<Inner>) {
        let mut poll = tokio::time::interval(LEVEL_POLL);
        loop {
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    None => break,
                    Some(Cmd::Dial(user)) => {
                        if let Err(e) = self.dial(user).await {
                            tracing::warn!(user, "could not start a connection: {e}");
                            self.emit(MeshEvent::Link { user, state: LinkState::Lost });
                        }
                    }
                    Some(Cmd::Signal { from, payload }) => {
                        if let Err(e) = self.on_signal(from, payload).await {
                            tracing::warn!(user = from, "signal failed: {e}");
                            self.emit(MeshEvent::Link { user: from, state: LinkState::Lost });
                        }
                    }
                    Some(Cmd::PeerLeft(user)) => {
                        self.links.remove(&user);
                    }
                    Some(Cmd::StartShare(track)) => self.start_share(track).await,
                    Some(Cmd::StopShare) => self.stop_share().await,
                },
                Some(msg) = inner.recv() => match msg {
                    Inner::Track(user, track, system) => {
                        if system {
                            self.system_ids.insert(track.id());
                        } else if let Some(link) = self.links.get_mut(&user) {
                            link._remote = Some(track.clone());
                        }
                        self.emit(MeshEvent::RemoteTrack { user, track, system });
                    }
                    Inner::Video(user, track) => {
                        if let Some(link) = self.links.get_mut(&user) {
                            link.remote_video = Some(track.clone());
                            link.remote_sharing = true;
                        }
                        self.emit(MeshEvent::RemoteVideo { user, track });
                    }
                    Inner::State(user, state) => self.emit(MeshEvent::Link { user, state: link_state(state) }),
                },
                _ = poll.tick() => self.poll_levels().await,
            }
        }
    }

    fn emit(&self, event: MeshEvent) {
        let _ = self.events.send(event);
    }

    fn send_signal(&self, to: UserId, signal: RtcSignal) {
        let payload = serde_json::to_value(signal).unwrap_or_default();
        let _ = self.signal_out.send(ClientMsg::Signal { to, payload });
    }

    /// Replaces any existing connection to `peer` with a fresh one.
    fn create_link(&mut self, peer: UserId, dialed: bool) -> Result<(), String> {
        self.links.remove(&peer);
        let mut config = RtcConfiguration::default();
        config.ice_servers = self.ice.clone();
        let pc = self.factory.create_peer_connection(config).map_err(|e| e.to_string())?;
        // Added before any description is set, so an incoming offer reuses this transceiver.
        pc.add_track(MediaStreamTrack::Audio(self.local.clone()), &["voice"]).map_err(|e| e.to_string())?;
        pc.add_track(MediaStreamTrack::Audio(self.system.clone()), &["system"]).map_err(|e| e.to_string())?;

        let out = self.signal_out.clone();
        pc.on_ice_candidate(Some(Box::new(move |c: IceCandidate| {
            let signal = RtcSignal::Candidate { mid: c.sdp_mid(), index: c.sdp_mline_index(), sdp: c.candidate() };
            let payload = serde_json::to_value(signal).unwrap_or_default();
            let _ = out.send(ClientMsg::Signal { to: peer, payload });
        })));
        let tx = self.inner_tx.clone();
        pc.on_track(Some(Box::new(move |ev| {
            match ev.track {
                MediaStreamTrack::Audio(track) => {
                    let system = ev.streams.iter().any(|s| s.id() == "system");
                    let _ = tx.send(Inner::Track(peer, track, system));
                }
                MediaStreamTrack::Video(track) => {
                    let _ = tx.send(Inner::Video(peer, track));
                }
            }
        })));
        let tx = self.inner_tx.clone();
        pc.on_connection_state_change(Some(Box::new(move |state| {
            let _ = tx.send(Inner::State(peer, state));
        })));

        let video = match &self.share {
            Some(track) => Some(pc.add_track(MediaStreamTrack::Video(track.clone()), &["screen"]).map_err(|e| e.to_string())?),
            None => None,
        };
        self.links.insert(peer, Link { pc, pending: Vec::new(), remote_set: false, _remote: None, video, remote_video: None, remote_sharing: false, dirty: false, dialed });
        Ok(())
    }

    async fn dial(&mut self, peer: UserId) -> Result<(), String> {
        self.create_link(peer, true)?;
        let pc = &self.links[&peer].pc;
        let options = OfferOptions { offer_to_receive_audio: Some(true), offer_to_receive_video: Some(true), ..Default::default() };
        let offer = pc.create_offer(options).await.map_err(|e| e.to_string())?;
        pc.set_local_description(offer.clone()).await.map_err(|e| e.to_string())?;
        self.send_signal(peer, RtcSignal::Offer { sdp: offer.to_string(), renegotiate: false });
        Ok(())
    }

    /// Gets `peer`'s connection renegotiated after a share started or stopped. The side that
    /// dialed offers; the other side asks it to. If an offer is already in flight, the change
    /// goes out once that exchange is done.
    async fn renegotiate(&mut self, peer: UserId) -> Result<(), String> {
        let Some(link) = self.links.get_mut(&peer) else { return Ok(()) };
        if !link.dialed {
            self.send_signal(peer, RtcSignal::Renegotiate);
            return Ok(());
        }
        if link.pc.signaling_state() != SignalingState::Stable {
            link.dirty = true;
            return Ok(());
        }
        link.dirty = false;
        let offer = link.pc.create_offer(OfferOptions { offer_to_receive_audio: Some(true), offer_to_receive_video: Some(true), ..Default::default() }).await.map_err(|e| e.to_string())?;
        link.pc.set_local_description(offer.clone()).await.map_err(|e| e.to_string())?;
        self.send_signal(peer, RtcSignal::Offer { sdp: offer.to_string(), renegotiate: true });
        Ok(())
    }

    async fn start_share(&mut self, track: RtcVideoTrack) {
        self.share = Some(track.clone());
        let peers: Vec<UserId> = self.links.keys().copied().collect();
        for peer in peers {
            let link = self.links.get_mut(&peer).expect("listed above");
            if let Some(sender) = &link.video {
                // The connection carried a share before: no new negotiation, just a new picture.
                if let Err(e) = sender.set_track(Some(MediaStreamTrack::Video(track.clone()))) {
                    tracing::warn!(peer, "could not send the screen: {e}");
                    continue;
                }
                self.send_signal(peer, RtcSignal::ShareStarted);
                continue;
            }
            match link.pc.add_track(MediaStreamTrack::Video(track.clone()), &["screen"]) {
                Ok(sender) => link.video = Some(sender),
                Err(e) => {
                    tracing::warn!(peer, "could not send the screen: {e}");
                    continue;
                }
            }
            if let Err(e) = self.renegotiate(peer).await {
                tracing::warn!(peer, "could not offer the screen: {e}");
            }
        }
    }

    async fn stop_share(&mut self) {
        self.share = None;
        let peers: Vec<UserId> = self.links.keys().copied().collect();
        for peer in peers {
            let Some(sender) = self.links.get(&peer).and_then(|l| l.video.as_ref()) else { continue };
            if let Err(e) = sender.set_track(Some(MediaStreamTrack::Video(self.idle.clone()))) {
                tracing::warn!(peer, "could not stop the screen: {e}");
                continue;
            }
            self.send_signal(peer, RtcSignal::ShareEnded);
        }
    }

    async fn on_signal(&mut self, from: UserId, payload: serde_json::Value) -> Result<(), String> {
        let signal: RtcSignal = serde_json::from_value(payload).map_err(|e| format!("unreadable signal: {e}"))?;
        match signal {
            RtcSignal::Offer { sdp, renegotiate } => {
                tracing::info!(user = from, renegotiate, codecs = %video_codecs(&sdp), "offer received");
                if !(renegotiate && self.links.contains_key(&from)) {
                    self.create_link(from, false)?;
                }
                let desc = SessionDescription::parse(&sdp, SdpType::Offer).map_err(|e| e.to_string())?;
                let link = self.links.get_mut(&from).expect("exists or just created");
                link.pc.set_remote_description(desc).await.map_err(|e| e.to_string())?;
                link.remote_set = true;
                flush_pending(link).await;
                let answer = link.pc.create_answer(AnswerOptions::default()).await.map_err(|e| e.to_string())?;
                link.pc.set_local_description(answer.clone()).await.map_err(|e| e.to_string())?;
                tune_video(link);
                self.send_signal(from, RtcSignal::Answer { sdp: answer.to_string() });
            }
            RtcSignal::Renegotiate => {
                if self.links.get(&from).is_some_and(|l| l.dialed) {
                    self.renegotiate(from).await?;
                }
            }
            RtcSignal::Answer { sdp } => {
                let Some(link) = self.links.get_mut(&from) else { return Ok(()) };
                let desc = SessionDescription::parse(&sdp, SdpType::Answer).map_err(|e| e.to_string())?;
                link.pc.set_remote_description(desc).await.map_err(|e| e.to_string())?;
                link.remote_set = true;
                flush_pending(link).await;
                tune_video(link);
                if link.dirty {
                    self.renegotiate(from).await?;
                }
            }
            RtcSignal::ShareEnded => {
                if let Some(link) = self.links.get_mut(&from) {
                    link.remote_sharing = false;
                }
                self.emit(MeshEvent::ShareEnded { user: from });
            }
            RtcSignal::ShareStarted => {
                // The first share announces itself by its video arriving; this is for later ones.
                if let Some(link) = self.links.get_mut(&from) {
                    if !link.remote_sharing {
                        if let Some(track) = link.remote_video.clone() {
                            link.remote_sharing = true;
                            self.emit(MeshEvent::RemoteVideo { user: from, track });
                        }
                    }
                }
            }
            RtcSignal::Candidate { mid, index, sdp } => {
                // A candidate for someone we no longer track is stale; drop it.
                let Some(link) = self.links.get_mut(&from) else { return Ok(()) };
                let candidate = IceCandidate::parse(&mid, index, &sdp).map_err(|e| e.to_string())?;
                if link.remote_set {
                    link.pc.add_ice_candidate(candidate).await.map_err(|e| e.to_string())?;
                } else {
                    link.pending.push(candidate);
                }
            }
        }
        Ok(())
    }

    /// Reads WebRTC's audio levels for everyone, and for the local microphone.
    async fn poll_levels(&self) {
        let mut local = None;
        let mic_id = self.local.id();
        for (&user, link) in &self.links {
            let Ok(stats) = link.pc.get_stats().await else { continue };
            let mut remote = 0f32;
            for stat in stats {
                match stat {
                    RtcStats::InboundRtp(s) if s.stream.kind == "audio" && !self.system_ids.contains(&s.inbound.track_identifier) => {
                        remote = remote.max(s.inbound.audio_level as f32)
                    }
                    RtcStats::MediaSource(s) if s.source.kind == "audio" && s.source.track_identifier == mic_id => {
                        local = Some(s.audio.audio_level as f32)
                    }
                    _ => {}
                }
            }
            self.emit(MeshEvent::Level { user, level: remote });
        }
        // Every 20th poll, so the log shows whether the microphone delivers any sound.
        static POLLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        if POLLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 20 == 0 {
            tracing::info!(level = ?local, enabled = self.local.enabled(), "microphone level");
        }
        if let Some(level) = local {
            self.emit(MeshEvent::Level { user: self.me, level });
        }
    }
}

/// The video codecs an SDP lists, for the log: a mismatch between platforms shows up here.
fn video_codecs(sdp: &str) -> String {
    let mut in_video = false;
    let mut out = Vec::new();
    for line in sdp.lines() {
        if line.starts_with("m=") {
            in_video = line.starts_with("m=video");
        } else if in_video && (line.starts_with("a=rtpmap:") || line.starts_with("a=fmtp:")) {
            out.push(line.trim_start_matches("a="));
        }
    }
    out.join(" | ")
}

async fn flush_pending(link: &mut Link) {
    for candidate in link.pending.drain(..) {
        if let Err(e) = link.pc.add_ice_candidate(candidate).await {
            tracing::warn!("queued candidate rejected: {e}");
        }
    }
}

/// Caps the outgoing screen at 1080p30 and keeps text sharp by dropping frames before resolution.
/// Needs the connection negotiated, which is when WebRTC creates the encoding it adjusts.
fn tune_video(link: &Link) {
    let Some(sender) = &link.video else { return };
    let mut params = sender.parameters();
    for encoding in &mut params.encodings {
        encoding.max_bitrate = Some(SHARE_MAX_BITRATE);
        encoding.max_framerate = Some(SHARE_MAX_FPS);
    }
    params.set_degradation_preference(DegradationPreference::MaintainResolution);
    if let Err(e) = sender.set_parameters(params) {
        tracing::debug!("could not tune the screen encoder: {e}");
    }
}
