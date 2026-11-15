# Implementation plan

Native Rust screen-share and voice app for Windows and macOS. Rooms of 2 to 4 people, full P2P mesh over WebRTC, signaling through a Rust server.

Problem: small groups want voice and screen sharing without a hosted service, a browser or an Electron app. Anyone can run the server on a VPS, and the clients talk to each other directly.

Findings from building and testing, with the spike results, are in [NOTES.md](NOTES.md).

## Remaining before release

All eight phases are implemented. These are not verified:

1. First CI run (never executed). Release blocker.
2. Windows: build and run the client, screen capture, system audio (WASAPI loopback).
3. A live microphone and a call between two real machines, including echo cancellation and app-exclusion of system audio on a second machine.
4. A real VPS: public IP, firewall, TURN relaying across a real NAT, the Caddy `tls` profile.
5. Mesh load: measure CPU and uplink with 4 peers sharing, then decide on a per-room sharer cap. An SFU is not planned unless the numbers force it.
6. Pin a checksum for the 277 MB `libwebrtc` download.
7. Decide whether to keep the Linux `client-net` CI job (`continue-on-error`, never blocks a release). Drop it unless it is cheap.

## Decisions

Each line gives the choice and the reason.

- Client: Rust, Slint UI, `libwebrtc` bindings (from `livekit/rust-sdks`, no LiveKit server). No Electron, no webview: one native binary, no second runtime, and no media server to host.
- Mesh instead of an SFU: for 2 to 4 people it needs no media server. Cost: each sharer sends one stream per peer. At the 6 Mbps cap, 3 viewers means about 18 Mbps up per sharer. An SFU is the fix if measurement shows a home uplink is not enough (item 5 above).
- Server: Rust, `axum` + WebSocket, `sqlx` on Postgres, coturn for TURN. The live-room registry is in memory, so there is one server process and no horizontal scaling.
- Accounts: unique username and password, argon2. No email. `SIGNUP_ENABLED=false` closes signup; an admin CLI creates accounts and resets passwords. Rate limits: login 20 per IP and 5 per username per 15 minutes, signup 10 per IP per hour, join requests 10 per user per minute.
- Rooms: code is 5 random lowercase letters, same format for temporary and permanent rooms, freed on delete. Each user has at most 3 rooms, permanent one included, and at most 1 permanent room.
- Join: anyone may request with a room code, the host accepts or denies. Accepted members never request again until the host removes them. Requests stay pending while the host is offline, capped at 50 per room and 20 per user.
- Host disconnect: a temporary room is destroyed. A permanent room keeps running: members can enter and stay without the host, and nobody is kicked. Only accepting new people needs the host online; their requests wait.
- An empty temporary room is destroyed immediately.
- A user is in one room at a time. Entering another room leaves the current one, which destroys it if it was a temporary room that is now empty.
- Features: audio with mute and a global hotkey, screen share (displays and windows) with optional system audio, 1080p30 adaptive, several simultaneous sharers, speaking indicator, mic and speaker pickers, echo cancellation and noise suppression.
- Transport: TLS by default. The per-server "allow unencrypted" checkbox is for local development only; it has no insecure badge.
- Out of v1: per-peer volume (the platform playout layer has no per-track volume, and a custom mixer risks echo cancellation), chat, push-to-talk, recording, code signing, auto-update.

## Layout

```
crates/protocol   wire types shared by both sides, including RtcSignal
crates/server     API, WebSocket signaling, DB, admin CLI
crates/client     Slint app, WebRTC, capture
crates/sysaudio   system audio capture (ScreenCaptureKit, WASAPI loopback)
deploy/           Dockerfile, docker-compose, coturn config
.github/          CI building Windows and macOS artifacts
```

## Phases

1. **Scaffold** (implemented): workspace, protocol types, migration, deploy files, CI.
2. **Server accounts** (implemented, tested): migrations run at startup, signup, login with argon2, session tokens, rate limiting, `create-user` and `reset-password` CLI. Tests against a real Postgres.
3. **Server rooms and signaling** (implemented, tested): WebSocket auth, room create and delete with the 3-room and 1-permanent limits, join request flow, membership persistence, live-room registry, host-disconnect rules, signal relay, TURN credential issue (HMAC of `TURN_SECRET`, shared with coturn), room-code and join-request rate limits.
4. **Client shell** (implemented): Slint screens for connect (host, TLS toggle), sign in and up, room list, pending requests, room view. Protocol client over WebSocket.
5. **Client audio** (implemented, unverified with a live microphone or two real machines): libwebrtc peer connections in a mesh, mic capture, device pickers, mute and global hotkey (Ctrl+Alt+M), speaking indicator. Per-participant volume is out of v1.
6. **Client screen share** (implemented, unverified with a real capture): libwebrtc desktop capture for displays and windows, adaptive bitrate at 1080p30, grid view and full-size tile, multiple sharers.
7. **System audio** (implemented; verified live on macOS, Windows compile-checked only): macOS ScreenCaptureKit (13+), Windows WASAPI loopback (Windows 10 2004+), sent as its own track.
8. **Hardening and release** (implemented; deploy stack verified locally, CI and a real VPS not run): deploy stack, CI artifacts for both platforms, README with setup steps.

## Risks

- Mesh upload: see Decisions and item 5 above.
- `webrtc-sys` can crash or call freed memory in two known places (`RtpSender::set_track(None)`, observer after close). Both are worked around in `crates/client/src/rtc.rs`; details in [NOTES.md](NOTES.md).
- `libwebrtc` builds pull a large prebuilt binary with no checksum check; CI cache and build time are unmeasured.
- macOS needs Screen Recording and Microphone permission, and system audio needs macOS 13 or later.
- Unsigned apps: macOS Gatekeeper and Windows SmartScreen warn on first run.
- Windows paths are only compile-checked.
