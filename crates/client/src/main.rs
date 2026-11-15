// A release build on Windows is a window app; without this it also opens a console.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use client::addr::ServerAddr;
use client::audio::{AudioEngine, Device};
use client::capture::{self, Share, Source};
use client::config::{save_password, saved_password, Config};
use client::model::AppModel;
use client::net::{self, NetEvent};
use client::rtc::{spawn_mesh, MeshEvent, MeshHandle};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use futures_util::StreamExt;
use libwebrtc::audio_frame::AudioFrame;
use libwebrtc::audio_source::native::NativeAudioSource;
use libwebrtc::video_stream::native::NativeVideoStream;
use protocol::{ClientMsg, Credentials, RoomCode, ServerMsg, Session, UserId};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel, Weak};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

slint::include_modules!();

#[derive(Default)]
struct State {
    addr: Option<ServerAddr>,
    session: Option<Session>,
    ws: Option<UnboundedSender<ClientMsg>>,
    model: AppModel,
    config: Config,
    /// Set at launch when a server, user and saved password exist: sign in without asking.
    auto_login: bool,
    /// Bumped on every sign-in and sign-out so late events from an old link are ignored.
    generation: u64,
    /// Started once at launch; `None` until it is ready, or when the system has no usable audio.
    engine: Option<Arc<AudioEngine>>,
    inputs: Vec<Device>,
    outputs: Vec<Device>,
    /// Exists only while we are inside a room.
    mesh: Option<MeshHandle>,
    /// Bumped on every mesh start so late events from an old mesh are ignored.
    mesh_generation: u64,
    /// Our own screen capture, while we share.
    share: Option<Share>,
    /// Whether this share also sends the computer's sound.
    share_with_audio: bool,
    /// The computer's sound on its way to the room, while we share it.
    system_audio: Option<SystemAudio>,
    /// Displays and windows to pick from where the system has no picker of its own.
    sources: Vec<Source>,
    /// The newest frame of each shared screen we are watching.
    frames: HashMap<UserId, SharedPixelBuffer<Rgba8Pixel>>,
    /// One reader per screen we are watching.
    watchers: HashMap<UserId, tokio::task::JoinHandle<()>>,
}

/// The running capture of the computer's sound and the task that feeds it to the room.
struct SystemAudio {
    _capture: sysaudio::Capture,
    feeder: tokio::task::JoinHandle<()>,
}

impl Drop for SystemAudio {
    fn drop(&mut self) {
        self.feeder.abort();
    }
}

#[derive(Clone)]
struct App {
    rt: tokio::runtime::Handle,
    ui: Weak<MainWindow>,
    state: Arc<Mutex<State>>,
}

/// A window app started from Finder has no terminal, so log to a file next to the settings.
/// If the app dies, the last line shows how far it got; panics are recorded there too.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("client=info,warn"));
    let file = dirs::config_dir().map(|d| d.join("screentalk")).and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("client.log")).ok()
    });
    match file {
        Some(file) => {
            tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(Mutex::new(file)).init();
            std::panic::set_hook(Box::new(|info| {
                tracing::error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
            }));
        }
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    tracing::info!("client started");
}

fn main() -> Result<(), slint::PlatformError> {
    init_logging();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("start tokio runtime");

    let ui = MainWindow::new()?;
    let config = Config::load();
    ui.set_server_input(config.server.clone().into());
    ui.set_insecure_input(config.insecure);
    ui.set_username_input(config.username.clone().into());
    ui.set_use_picker(capture::USES_SYSTEM_PICKER);

    let app = App {
        rt: rt.handle().clone(),
        ui: ui.as_weak(),
        state: Arc::new(Mutex::new(State { config, ..State::default() })),
    };
    wire(&ui, &app);
    let (server, insecure, username) = {
        let st = app.state.lock().unwrap();
        (st.config.server.clone(), st.config.insecure, st.config.username.clone())
    };
    if !server.is_empty() && !username.is_empty() && !saved_password(&server, &username).is_empty() {
        app.state.lock().unwrap().auto_login = true;
        app.connect(server, insecure);
    }
    app.start_audio();
    // The manager must live as long as the window, and on macOS be created on the main thread.
    let _hotkey = register_mute_hotkey(&app);
    ui.run()
}

/// Ctrl+Alt+M toggles mute from anywhere. Failure only costs the shortcut, so it is logged.
fn register_mute_hotkey(app: &App) -> Option<GlobalHotKeyManager> {
    let manager = GlobalHotKeyManager::new().map_err(|e| tracing::warn!("no global hotkeys: {e}")).ok()?;
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyM);
    if let Err(e) = manager.register(hotkey) {
        tracing::warn!("could not register the mute hotkey: {e}");
        return None;
    }
    let app = app.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.id == hotkey.id() && event.state == HotKeyState::Pressed {
            let handler = app.clone();
            app.with_ui(move |_| handler.toggle_mute());
        }
    }));
    Some(manager)
}

fn wire(ui: &MainWindow, app: &App) {
    ui.on_connect({
        let app = app.clone();
        move |server, insecure| app.connect(server.to_string(), insecure)
    });
    ui.on_authenticate({
        let app = app.clone();
        move |user, password, signup| app.authenticate(user.to_string(), password.to_string(), signup)
    });
    ui.on_change_server({
        let app = app.clone();
        move || app.ui().set_screen(0)
    });
    ui.on_create_room({
        let app = app.clone();
        move |permanent| app.send(ClientMsg::CreateRoom { permanent })
    });
    ui.on_enter_room({
        let app = app.clone();
        move |code| {
            app.with_code(&code, |code| ClientMsg::EnterRoom { code });
        }
    });
    ui.on_delete_room({
        let app = app.clone();
        move |code| {
            app.with_code(&code, |code| ClientMsg::DeleteRoom { code });
        }
    });
    ui.on_request_join({
        let app = app.clone();
        move |code| {
            if app.with_code(&code, |code| ClientMsg::RequestJoin { code }) {
                app.ui().set_status("Request sent. The host has to accept it.".into());
            }
        }
    });
    ui.on_decide({
        let app = app.clone();
        move |index, accept| app.decide(index, accept)
    });
    ui.on_leave_room({
        let app = app.clone();
        move || app.leave_room()
    });
    ui.on_remove_member({
        let app = app.clone();
        move |index| app.remove_member(index)
    });
    ui.on_toggle_mute({
        let app = app.clone();
        move || app.toggle_mute()
    });
    ui.on_toggle_share({
        let app = app.clone();
        move |index, with_audio| app.toggle_share(index, with_audio)
    });
    ui.on_input_selected({
        let app = app.clone();
        move |index| app.select_device(index, true)
    });
    ui.on_output_selected({
        let app = app.clone();
        move |index| app.select_device(index, false)
    });
    ui.on_logout({
        let app = app.clone();
        move || app.logout()
    });
}

impl App {
    /// Only call from the UI thread (inside a Slint callback).
    fn ui(&self) -> MainWindow {
        self.ui.upgrade().expect("callbacks run while the window exists")
    }

    /// Runs `f` on the UI thread; safe to call from any thread.
    fn with_ui(&self, f: impl FnOnce(&MainWindow) + Send + 'static) {
        let _ = self.ui.upgrade_in_event_loop(move |ui| f(&ui));
    }

    fn send(&self, msg: ClientMsg) {
        if let Some(ws) = &self.state.lock().unwrap().ws {
            let _ = ws.send(msg);
        }
    }

    /// Parses a typed room code and sends the message built from it. Returns false
    /// (and tells the user) when the code is malformed.
    fn with_code(&self, text: &str, build: impl FnOnce(RoomCode) -> ClientMsg) -> bool {
        match RoomCode::parse(text.trim()) {
            Some(code) => {
                self.send(build(code));
                true
            }
            None => {
                self.ui().set_status("Room codes are 5 lowercase letters".into());
                false
            }
        }
    }

    /// Starts the audio layer off the UI thread: listing devices can take a moment.
    fn start_audio(&self) {
        let app = self.clone();
        self.rt.spawn_blocking(move || match AudioEngine::start() {
            Ok(engine) => {
                let (inputs, outputs) = (engine.inputs(), engine.outputs());
                let names = |d: &[Device]| d.iter().map(|d| d.name.clone()).collect::<Vec<_>>();
                let (in_names, out_names) = (names(&inputs), names(&outputs));
                {
                    let mut st = app.state.lock().unwrap();
                    st.engine = Some(Arc::new(engine));
                    st.inputs = inputs;
                    st.outputs = outputs;
                }
                app.with_ui(move |ui| {
                    ui.set_input_devices(string_model(in_names));
                    ui.set_output_devices(string_model(out_names));
                });
            }
            Err(e) => {
                tracing::warn!("audio unavailable: {e}");
                app.with_ui(move |ui| ui.set_status(format!("Audio is unavailable: {e}").into()));
            }
        });
    }

    /// Only call on the UI thread.
    fn toggle_mute(&self) {
        let mut st = self.state.lock().unwrap();
        st.model.muted = !st.model.muted;
        if let Some(engine) = &st.engine {
            engine.set_muted(st.model.muted);
        }
        render(&self.ui(), &mut st);
    }

    fn select_device(&self, index: i32, input: bool) {
        let st = self.state.lock().unwrap();
        let Some(engine) = st.engine.clone() else { return };
        let list = if input { &st.inputs } else { &st.outputs };
        let Some(guid) = usize::try_from(index).ok().and_then(|i| list.get(i)).map(|d| d.guid.clone()) else { return };
        drop(st);
        let app = self.clone();
        self.rt.spawn_blocking(move || {
            let ok = if input { engine.select_input(&guid) } else { engine.select_output(&guid) };
            if !ok {
                app.with_ui(|ui| ui.set_status("Could not switch to that device".into()));
            }
        });
    }

    /// Keeps the audio mesh in step with the room we are in. Call after the model has applied `event`.
    fn sync_mesh(&self, st: &mut State, action: MeshAction) {
        if st.model.current.is_none() {
            stop_media(st);
            return;
        }
        match action {
            MeshAction::Entered => self.start_mesh(st),
            MeshAction::Signal(from, payload) => {
                if let Some(mesh) = &st.mesh {
                    mesh.signal(from, payload);
                }
            }
            MeshAction::PeerLeft(user) => {
                if let Some(mesh) = &st.mesh {
                    mesh.peer_left(user);
                }
            }
            MeshAction::None => {}
        }
    }

    /// Joins the audio of everyone already in the room by dialing them.
    fn start_mesh(&self, st: &mut State) {
        stop_media(st);
        let (Some(engine), Some(ws), Some(room)) = (st.engine.clone(), st.ws.clone(), st.model.current.as_ref()) else {
            tracing::warn!("entered a room but audio or the connection is not ready: no voice");
            return;
        };
        tracing::info!(room = room.code.as_str(), peers = room.peers.len(), "entered a room: opening the microphone");
        st.mesh_generation += 1;
        let (generation, session_generation) = (st.mesh_generation, st.generation);
        let (events, mut inbox) = unbounded_channel::<MeshEvent>();
        let track = engine.local_track();
        tracing::info!(state = engine.describe(), "microphone open: starting the audio mesh");
        let mesh = spawn_mesh(&self.rt, engine.factory(), track, &room.ice, st.model.me, ws, events);
        for peer in &room.peers {
            mesh.dial(peer.user_id);
        }
        st.mesh = Some(mesh);
        if !capture::USES_SYSTEM_PICKER {
            self.refresh_sources();
        }

        let app = self.clone();
        self.rt.spawn(async move {
            while let Some(event) = inbox.recv().await {
                let handler = app.clone();
                app.with_ui(move |ui| handler.handle_mesh_event(ui, session_generation, generation, event));
            }
        });
    }

    fn handle_mesh_event(&self, ui: &MainWindow, session: u64, mesh: u64, event: MeshEvent) {
        let mut st = self.state.lock().unwrap();
        if st.generation != session || st.mesh_generation != mesh || st.mesh.is_none() {
            return;
        }
        match event {
            MeshEvent::Link { user, state } => st.model.set_link(user, state),
            MeshEvent::Level { user, level } => st.model.note_level(user, level, Instant::now()),
            // The platform audio layer already plays remote audio.
            MeshEvent::RemoteTrack { user, system, .. } => {
                if let Some(engine) = st.engine.as_ref() {
                    tracing::info!(user, system, state = engine.describe(), "remote audio track arrived");
                }
                return;
            }
            MeshEvent::RemoteVideo { user, track } => {
                st.model.set_sharer(user, true);
                if let Some(old) = st.watchers.remove(&user) {
                    old.abort();
                }
                let watcher = self.watch_screen(user, track, session, mesh);
                st.watchers.insert(user, watcher);
            }
            MeshEvent::ShareEnded { user } => {
                st.model.set_sharer(user, false);
                if let Some(watcher) = st.watchers.remove(&user) {
                    watcher.abort();
                }
                st.frames.remove(&user);
            }
        }
        render(ui, &mut st);
    }

    /// Reads one person's screen frames and hands the newest to the window.
    fn watch_screen(
        &self,
        user: UserId,
        track: libwebrtc::video_track::RtcVideoTrack,
        session: u64,
        mesh: u64,
    ) -> tokio::task::JoinHandle<()> {
        let app = self.clone();
        self.rt.spawn(async move {
            let mut stream = NativeVideoStream::new(track);
            // The window may fall behind; skip frames rather than queue them.
            let waiting = Arc::new(AtomicBool::new(false));
            while let Some(frame) = stream.next().await {
                if waiting.swap(true, Ordering::AcqRel) {
                    continue;
                }
                let Some(pixels) = client::video::to_rgba(frame.buffer.as_ref()) else {
                    waiting.store(false, Ordering::Release);
                    continue;
                };
                let (handler, waiting) = (app.clone(), waiting.clone());
                app.with_ui(move |ui| {
                    waiting.store(false, Ordering::Release);
                    let mut st = handler.state.lock().unwrap();
                    if st.generation != session || st.mesh_generation != mesh || !st.model.is_sharer(user) {
                        return;
                    }
                    st.frames.insert(user, pixels);
                    render(ui, &mut st);
                });
            }
        })
    }

    fn refresh_sources(&self) {
        let app = self.clone();
        self.rt.spawn_blocking(move || {
            let sources = capture::list_sources();
            let names: Vec<String> =
                sources.iter().map(|s| format!("{}: {}", if s.window { "Window" } else { "Screen" }, s.title)).collect();
            app.state.lock().unwrap().sources = sources;
            app.with_ui(move |ui| ui.set_sources(string_model(names)));
        });
    }

    /// Only call on the UI thread.
    fn toggle_share(&self, source_index: i32, with_audio: bool) {
        // Creating the video source spawns a tokio task, and the UI thread is not in the runtime.
        let _runtime = self.rt.enter();
        let mut st = self.state.lock().unwrap();
        if st.share.take().is_some() {
            st.system_audio = None;
            if let Some(mesh) = &st.mesh {
                mesh.stop_share();
            }
            st.model.sharing = false;
            render(&self.ui(), &mut st);
            return;
        }
        let (Some(engine), Some(_), Some(session)) = (st.engine.clone(), st.mesh.as_ref(), st.session.as_ref()) else {
            return;
        };
        let _ = session;
        let source = if capture::USES_SYSTEM_PICKER {
            None
        } else {
            match usize::try_from(source_index).ok().and_then(|i| st.sources.get(i)).cloned() {
                Some(source) => Some(source),
                None => return self.ui().set_status("Pick a screen or window to share first".into()),
            }
        };
        let (generation, mesh_generation) = (st.generation, st.mesh_generation);
        let (started, ended) = (self.clone(), self.clone());
        let share = Share::start(
            &engine.factory(),
            source,
            // Nothing is sent to the room until there is something to see.
            move || started.with_ui({
                let app = started.clone();
                move |_| app.begin_sending(generation, mesh_generation)
            }),
            move |reason| ended.with_ui({
                let app = ended.clone();
                move |ui| app.share_ended(ui, generation, mesh_generation, reason)
            }),
        );
        match share {
            Ok(share) => {
                st.share = Some(share);
                st.share_with_audio = with_audio;
                st.model.sharing = true;
                render(&self.ui(), &mut st);
            }
            Err(e) => self.ui().set_status(format!("Could not start sharing: {e}").into()),
        }
    }

    fn begin_sending(&self, generation: u64, mesh_generation: u64) {
        let st = self.state.lock().unwrap();
        if st.generation != generation || st.mesh_generation != mesh_generation {
            return;
        }
        let (Some(share), Some(mesh)) = (&st.share, &st.mesh) else { return };
        mesh.start_share(share.track.clone());
        if st.share_with_audio {
            self.start_system_audio(generation, mesh_generation, mesh.system_audio());
        }
    }

    /// Starts capturing the computer's sound. Opening the capture can take a moment, so it
    /// happens off the UI thread; if the share ended meanwhile the capture is dropped again.
    fn start_system_audio(&self, generation: u64, mesh_generation: u64, target: NativeAudioSource) {
        let app = self.clone();
        self.rt.spawn_blocking(move || {
            let (tx, mut chunks) = unbounded_channel::<Vec<i16>>();
            let capture = sysaudio::start(move |chunk| {
                let _ = tx.send(chunk.to_vec());
            });
            let handler = app.clone();
            app.with_ui(move |ui| {
                let mut st = handler.state.lock().unwrap();
                if st.generation != generation || st.mesh_generation != mesh_generation || st.share.is_none() {
                    return;
                }
                match capture {
                    Ok(capture) => {
                        let feeder = handler.rt.spawn(async move {
                            while let Some(samples) = chunks.recv().await {
                                let frame = AudioFrame {
                                    data: Cow::Owned(samples),
                                    sample_rate: sysaudio::SAMPLE_RATE,
                                    num_channels: sysaudio::CHANNELS as u32,
                                    samples_per_channel: sysaudio::CHUNK_FRAMES as u32,
                                };
                                if target.capture_frame(&frame).await.is_err() {
                                    break;
                                }
                            }
                        });
                        st.system_audio = Some(SystemAudio { _capture: capture, feeder });
                    }
                    Err(e) => ui.set_status(format!("Your screen is shared, but not the computer's sound: {e}").into()),
                }
            });
        });
    }

    /// The capture stopped on its own (picker cancelled, window closed, permission missing).
    fn share_ended(&self, ui: &MainWindow, generation: u64, mesh_generation: u64, reason: String) {
        let mut st = self.state.lock().unwrap();
        if st.generation != generation || st.mesh_generation != mesh_generation || st.share.is_none() {
            return;
        }
        st.share = None;
        st.system_audio = None;
        if let Some(mesh) = &st.mesh {
            mesh.stop_share();
        }
        st.model.sharing = false;
        ui.set_status(reason.into());
        render(ui, &mut st);
    }

    fn connect(&self, server: String, insecure: bool) {
        let ui = self.ui();
        ui.set_busy(true);
        ui.set_status("".into());
        let app = self.clone();
        self.rt.spawn(async move {
            let result = async {
                let addr = ServerAddr::parse(&server, !insecure)?;
                net::ping(&addr).await?;
                Ok::<_, String>(addr)
            }
            .await;
            let state = app.state.clone();
            let again = app.clone();
            app.with_ui(move |ui| {
                ui.set_busy(false);
                match result {
                    Ok(addr) => {
                        let mut st = state.lock().unwrap();
                        st.config.server = server;
                        st.config.insecure = insecure;
                        st.config.save();
                        ui.set_server_label(addr.display().into());
                        let password = saved_password(&st.config.server, &st.config.username);
                        ui.set_password_input(password.clone().into());
                        st.addr = Some(addr);
                        ui.set_screen(1);
                        let auto = std::mem::take(&mut st.auto_login).then(|| st.config.username.clone());
                        drop(st);
                        if let Some(username) = auto {
                            again.authenticate(username, password, false);
                        }
                    }
                    Err(e) => {
                        state.lock().unwrap().auto_login = false;
                        ui.set_status(e.into());
                    }
                }
            });
        });
    }

    fn authenticate(&self, username: String, password: String, signup: bool) {
        let ui = self.ui();
        let username = match protocol::validate_username(&username) {
            Ok(u) => u,
            Err(e) => return ui.set_status(e.into()),
        };
        if signup {
            if let Err(e) = protocol::validate_password(&password) {
                return ui.set_status(e.into());
            }
        }
        let Some(addr) = self.state.lock().unwrap().addr.clone() else { return };
        ui.set_busy(true);
        ui.set_status("".into());
        let server = self.state.lock().unwrap().config.server.clone();
        let app = self.clone();
        self.rt.spawn(async move {
            match net::authenticate(&addr, signup, &Credentials { username: username.clone(), password: password.clone() }).await {
                Ok(session) => {
                    save_password(&server, &username, &password);
                    app.start_session(addr, session)
                }
                Err(e) => app.with_ui(move |ui| {
                    ui.set_busy(false);
                    ui.set_status(e.into());
                }),
            }
        });
    }

    /// Runs on the tokio runtime: `spawn_ws` needs it.
    fn start_session(&self, addr: ServerAddr, session: Session) {
        let (events, mut inbox) = unbounded_channel::<NetEvent>();
        let ws = net::spawn_ws(addr, session.token.clone(), events);
        let generation = {
            let mut st = self.state.lock().unwrap();
            st.generation += 1;
            st.model = AppModel::new(session.user_id, session.username.clone());
            st.config.username = session.username.clone();
            st.config.save();
            st.session = Some(session.clone());
            st.ws = Some(ws);
            st.generation
        };

        let app = self.clone();
        self.rt.spawn(async move {
            while let Some(event) = inbox.recv().await {
                let handler = app.clone();
                app.with_ui(move |ui| handler.handle_event(ui, generation, event));
            }
        });

        let name = session.username;
        self.with_ui(move |ui| {
            ui.set_busy(false);
            ui.set_username(name.into());
            ui.set_screen(2);
        });
    }

    fn handle_event(&self, ui: &MainWindow, generation: u64, event: NetEvent) {
        let mut st = self.state.lock().unwrap();
        if st.generation != generation || st.session.is_none() {
            return;
        }
        if matches!(event, NetEvent::AuthExpired) {
            st.session = None;
            st.ws = None;
            ui.set_screen(1);
            ui.set_status("Your session expired. Please sign in again.".into());
            return;
        }
        let action = match &event {
            NetEvent::Server(ServerMsg::Entered { .. }) => MeshAction::Entered,
            NetEvent::Server(ServerMsg::Signal { from, payload }) => MeshAction::Signal(*from, payload.clone()),
            NetEvent::Server(ServerMsg::PeerLeft { user_id }) => MeshAction::PeerLeft(*user_id),
            _ => MeshAction::None,
        };
        let replies = st.model.apply(event);
        if let Some(ws) = &st.ws {
            for msg in replies {
                let _ = ws.send(msg);
            }
        }
        self.sync_mesh(&mut st, action);
        render(ui, &mut st);
    }

    fn decide(&self, index: i32, accept: bool) {
        let mut st = self.state.lock().unwrap();
        let Some((code, peer)) = usize::try_from(index).ok().and_then(|i| st.model.pending.get(i).cloned()) else {
            return;
        };
        if let Some(ws) = &st.ws {
            let _ = ws.send(ClientMsg::DecideJoin { code: code.clone(), user_id: peer.user_id, accept });
        }
        // The server does not echo a decision back to the host.
        st.model.resolve_pending(&code, peer.user_id);
        render(&self.ui(), &mut st);
    }

    fn leave_room(&self) {
        let mut st = self.state.lock().unwrap();
        if let Some(ws) = &st.ws {
            let _ = ws.send(ClientMsg::LeaveRoom);
        }
        st.model.left_room();
        stop_media(&mut st);
        render(&self.ui(), &mut st);
    }

    fn remove_member(&self, index: i32) {
        let st = self.state.lock().unwrap();
        let Some(cur) = &st.model.current else { return };
        let Some(peer) = usize::try_from(index).ok().and_then(|i| cur.peers.get(i)) else { return };
        if let Some(ws) = &st.ws {
            let _ = ws.send(ClientMsg::RemoveMember { code: cur.code.clone(), user_id: peer.user_id });
        }
    }

    fn logout(&self) {
        let (addr, token) = {
            let mut st = self.state.lock().unwrap();
            st.generation += 1;
            // Dropping the sender closes the WebSocket and stops reconnecting.
            st.ws = None;
            st.model.left_room();
            stop_media(&mut st);
            let token = st.session.take().map(|s| s.token);
            (st.addr.clone(), token)
        };
        if let (Some(addr), Some(token)) = (addr, token) {
            self.rt.spawn(async move { net::logout(&addr, &token).await });
        }
        let ui = self.ui();
        ui.set_status("".into());
        ui.set_screen(1);
    }
}

/// What the audio mesh must do after a server message.
enum MeshAction {
    None,
    Entered,
    Signal(UserId, serde_json::Value),
    PeerLeft(UserId),
}

/// Ends everything tied to being in a room: the audio mesh, our own share, and the screens we watch.
/// Dropping the mesh handle closes every connection.
fn stop_media(st: &mut State) {
    st.mesh = None;
    st.share = None;
    st.system_audio = None;
    st.model.sharing = false;
    for (_, watcher) in st.watchers.drain() {
        watcher.abort();
    }
    st.frames.clear();
}

fn string_model(items: Vec<String>) -> ModelRc<slint::SharedString> {
    ModelRc::new(VecModel::from(items.into_iter().map(Into::into).collect::<Vec<_>>()))
}

/// Copies the model into the window's properties.
fn render(ui: &MainWindow, st: &mut State) {
    let State { model, frames, .. } = st;
    let now = Instant::now();
    ui.set_connected(model.connected);

    let rooms: Vec<RoomItem> = model
        .rooms
        .iter()
        .map(|r| RoomItem {
            code: r.code.as_str().into(),
            host: r.host.as_str().into(),
            permanent: r.permanent,
            mine: model.is_mine(r),
        })
        .collect();
    ui.set_rooms(ModelRc::new(VecModel::from(rooms)));

    let pending: Vec<PendingItem> = model
        .pending
        .iter()
        .map(|(code, peer)| PendingItem { code: code.as_str().into(), username: peer.username.as_str().into() })
        .collect();
    ui.set_pending(ModelRc::new(VecModel::from(pending)));

    let peers: Vec<PeerItem> = model
        .current
        .iter()
        .flat_map(|c| c.peers.iter())
        .map(|p| PeerItem {
            username: p.username.as_str().into(),
            speaking: model.is_speaking(p.user_id, now),
            state: model.link_state(p.user_id).map(|s| s.label()).unwrap_or_default().into(),
            sharing: model.is_sharer(p.user_id),
        })
        .collect();
    ui.set_peers(ModelRc::new(VecModel::from(peers)));

    let shares: Vec<ShareItem> = model
        .current
        .iter()
        .flat_map(|c| c.peers.iter())
        .filter(|p| model.is_sharer(p.user_id))
        .map(|p| ShareItem {
            username: p.username.as_str().into(),
            video: frames.get(&p.user_id).map(|f| Image::from_rgba8(f.clone())).unwrap_or_default(),
        })
        .collect();
    ui.set_shares(ModelRc::new(VecModel::from(shares)));

    ui.set_muted(model.muted);
    ui.set_sharing(model.sharing);
    ui.set_me_speaking(model.is_speaking(model.me, now));
    ui.set_room_code(model.current.as_ref().map(|c| c.code.as_str()).unwrap_or_default().into());
    ui.set_is_host(model.in_own_room());
    ui.set_screen(if model.current.is_some() { 3 } else { 2 });

    if let Some(notice) = model.take_notice() {
        ui.set_status(notice.into());
    }
}
