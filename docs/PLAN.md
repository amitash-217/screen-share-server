# Implementation plan

Native Rust screen-share and voice app for Windows and macOS. Rooms of 2 to 4 people, full P2P mesh over WebRTC, signaling through a Rust server.

## Decisions (settled)

- Client: Rust, Slint UI, `libwebrtc` bindings (from `livekit/rust-sdks`, no LiveKit server). No Electron, no webview.
- Server: Rust, `axum` + WebSocket, `sqlx` on Postgres, coturn for TURN.
- Accounts: unique username and password, argon2. No email. Login rate-limited per IP and per username. `SIGNUP_ENABLED=false` closes signup; an admin CLI creates accounts and resets passwords.
- Rooms: code is 5 random lowercase letters, same format for temporary and permanent rooms, freed on delete. Each user has at most 3 rooms, permanent one included, and at most 1 permanent room.
- Join: anyone may request with a room code, the host accepts or denies. Accepted members never request again until the host removes them. Requests stay pending while the host is offline.
- Host disconnect: a temporary room is destroyed. A permanent room keeps running: members can enter and stay without the host, and nobody is kicked. Only accepting new people needs the host online; their requests wait.
- An empty temporary room is destroyed immediately.
- A user is in one room at a time. Entering another room leaves the current one, which destroys it if it was a temporary room that is now empty.
- Features: audio with mute and a global hotkey, screen share (displays and windows) with optional system audio, 1080p30 adaptive, several simultaneous sharers, per-peer volume, speaking indicator, mic and speaker pickers, echo cancellation and noise suppression.
- Transport: TLS by default, per-server "allow unencrypted" checkbox, no insecure badge.
- Out of v1: chat, push-to-talk, recording, code signing, auto-update.

## Layout

```
crates/protocol   wire types shared by both sides (done)
crates/server     API, WebSocket signaling, DB, admin CLI
crates/client     Slint app, WebRTC, capture
deploy/           Dockerfile, docker-compose, coturn config
.github/          CI building Windows and macOS artifacts
```

## Phases

1. **Scaffold** (done): workspace, protocol types, migration, deploy files, CI.
2. **Server accounts** (done): migrations run at startup, signup, login with argon2, session tokens, rate limiting, `create-user` and `reset-password` CLI. Tests against a real Postgres.
3. **Server rooms and signaling** (done): WebSocket auth, room create and delete with the 3-room and 1-permanent limits, join request flow, membership persistence, live-room registry, host-disconnect rules, signal relay, TURN credential issue (HMAC of `TURN_SECRET`), room-code and join-request rate limits.
4. **Client shell** (done): Slint screens for connect (host, TLS toggle), sign in and up, room list, pending requests, room view. Protocol client over WebSocket. No media yet.
5. **Client audio** (done, except volume): libwebrtc peer connections in a mesh, mic capture, device pickers, mute and global hotkey (Ctrl+Alt+M), speaking indicator. Per-participant volume is deferred: the platform playout layer has no per-track volume, so it needs a custom playout path (NativeAudioStream, own mixing and output). Not tested with a live microphone or two real machines.
6. **Client screen share** (done, untested with a real capture): libwebrtc desktop capture for displays and windows, adaptive bitrate at 1080p30, grid view and full-size tile, multiple sharers.
7. **System audio** (done on macOS and verified live; Windows compiled only): macOS ScreenCaptureKit (13+), Windows WASAPI loopback, mixed into the shared stream. Windows code compiled in CI, tested by the user on a real machine.
8. **Hardening and release** (done locally; CI and a real VPS not run): deploy end to end on a VPS, CI artifacts for both platforms, README with setup steps.

## libwebrtc spike results (macOS arm64, `libwebrtc` 0.4.0)

Verified on this machine with a scratch crate (two in-process peers):
- Builds and links. Two peers connect over loopback in about 140 ms, a data channel delivers a message, and 3 s of audio crosses the link (tone survives Opus; spectrum peak at 450 Hz for a 440 Hz input, 5 Hz scan resolution, cause not traced).
- The crate has a platform audio-device layer behind `PeerConnectionFactoryExt`: `acquire_platform_adm()` then list and pick input/output devices by stable GUID, `create_device_audio_track` for microphone capture, optional speaker playout. Devices listed correctly (MacBook microphone and speakers). No need for our own `cpal` code. The listing is empty until the platform layer is acquired.
- Built-in (hardware) AEC is not available on macOS, so echo cancellation relies on WebRTC's software processing. Not tested with a live microphone.
- `DesktopCapturer` works for windows. For screens with the ScreenCaptureKit picker disabled, `get_source_list` returns nothing (documented), so screens need either the system picker or display ids from CoreGraphics. No frames were captured; Screen Recording permission untested.

Build facts that affect the real client:
- The `webrtc-sys` build script downloads a 277 MB prebuilt zip from GitHub Releases (1.1 GB unpacked). On a slow link its HTTP request times out. Workaround: download the zip yourself, unzip it, and set `LK_CUSTOM_WEBRTC` to the `mac-arm64-release` folder.
- macOS binaries must be linked with `-ObjC`. The dependency's own link argument does not reach our binary; without it the app crashes at start with `+[NSString stringForStdString:] unrecognized selector`. Emit `cargo:rustc-link-arg-bins=-ObjC` from the client's `build.rs`.
- Handles matter: dropping a received `DataChannel` handle silently drops its message handler. Keep received channels and tracks alive.
- A data channel send before the channel is `Open` is lost without an error.
- Not yet checked: Windows build, system audio, the 1080p30 encoder, CPU use with 3 remote peers.

## Risks

- Mesh upload: each sharer sends one stream per peer. Four sharers at 1080p30 can exceed a home uplink. An SFU is the later fix.
- `libwebrtc` crate builds pull large prebuilt binaries; CI cache and build time need checking in phase 5.
- macOS needs Screen Recording and Microphone permission, and system audio needs macOS 13 or later.
- Windows paths are only compile-checked here.

## Screen share notes (phase 6)

- macOS uses the system picker (displays and windows); Windows lists displays and windows in the app. Capture is scaled down to fit 1080p, 30 fps, 6 Mbps cap, resolution kept before frame rate.
- Only the side that dialed ever sends offers. The other side sends `Renegotiate`, so offers never cross. Tried and dropped: sending offers from the sharer with rollback; one direction lost its video.
- A sender is never emptied: `RtpSender::set_track(None)` crashes webrtc-sys, so an idle black track is swapped in and `ShareEnded` tells viewers to hide the tile. `ShareStarted` re-announces a later share on the same connection.
- webrtc-sys can call a freed observer after a peer connection closes (crash in `on_ice_connection_change`). `Link::drop` closes the connection and keeps a handle alive for 3 s.
- Not yet verified: a real capture, Screen Recording permission flow, picker cancel, Windows.

## System audio notes (phase 7)

- New crate `sysaudio`: `start(callback)` delivers 48 kHz stereo `i16` in exact 10 ms chunks. macOS: ScreenCaptureKit with the current process excluded. Windows: `wasapi` crate (wraps the `windows` crate) process loopback in exclude mode, needs Windows 10 2004+. Excluding the app is what stops other people's voices coming back to them.
- The computer's sound is its own audio track ("system" stream id), present on every connection from the start, because the answering side cannot add tracks later. Listeners hear it mixed with the voice by the platform playout. Levels ignore it, so a video's sound does not light the speaking dot.
- ScreenCaptureKit's bindings need the Swift runtime: `build.rs` of `sysaudio` and `client` add `-rpath /usr/lib/swift` on macOS (without it the binary aborts at start).
- Verified: live capture on this Mac (300 chunks in 3 s, `cargo test -p sysaudio -- --ignored`); mesh test that computer sound arrives with sound and is not counted as speech; Windows crate type-checks with `cargo check -p sysaudio --target x86_64-pc-windows-msvc`. Not verified: the Windows capture itself, the checkbox in the running app, hearing it on a second machine.

## Release notes (phase 8)

- Verified locally: the deploy stack (`db`, `server`, `coturn`) builds and starts with Docker Compose on this Mac; signup, login, `/healthz`, and `create-user` (password piped on stdin) work against it; coturn accepts the config. `docker compose config` passes for the default and `tls` profiles. Not verified: a real VPS, public IP and firewall, the Caddy profile (needs a domain), TURN relaying across a real NAT.
- `TRUST_PROXY=true` reads the last `X-Forwarded-For` entry for rate limits; without it, everyone behind Caddy would share one login limit.
- coturn: private and loopback peer ranges are denied so relays cannot reach the server's own network; `--external-ip` takes `PUBLIC_IP` because it rejects domain names.
- macOS ships as an ad-hoc-signed `ScreenTalk.app` (`scripts/bundle-macos.sh`, `NSMicrophoneUsageDescription` in Info.plist); built and launched locally. Windows ships `client.exe` in a zip, with the console window hidden in release builds. Neither signed with a developer certificate.
- CI (`.github/workflows/ci.yml`) has not run: jobs are server tests, deploy build and compose check, client on Windows and macOS with packaged artifacts, a best-effort Linux client-net job (`continue-on-error`, libwebrtc's Linux system libraries unverified), and a tagged-release job (`v*`).
