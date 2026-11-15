# Implementation notes

Moved out of `PLAN.md`. These record what was found and verified while building. The plan holds decisions and open work.

## libwebrtc spike results (macOS arm64, `libwebrtc` 0.4.0)

Verified on the author's Mac with a scratch crate (two in-process peers):
- Builds and links. Two peers connect over loopback in about 140 ms, a data channel delivers a message, and 3 s of audio crosses the link. The spectrum peak was 450 Hz for a 440 Hz input at 5 Hz scan resolution; the 10 Hz offset was not traced, so "the tone survives Opus" is not proven.
- The crate has a platform audio-device layer behind `PeerConnectionFactoryExt`: `acquire_platform_adm()` then list and pick input/output devices by stable GUID, `create_device_audio_track` for microphone capture, optional speaker playout. Devices listed correctly (MacBook microphone and speakers). The client uses this layer instead of its own `cpal` code. The listing is empty until the platform layer is acquired.
- Built-in (hardware) AEC is not available on macOS, so echo cancellation relies on WebRTC's software processing. Not tested with a live microphone.
- `DesktopCapturer` works for windows. For screens with the ScreenCaptureKit picker disabled, `get_source_list` returns nothing (documented), so screens need either the system picker or display ids from CoreGraphics.

### Build facts that affect the real client

- The `webrtc-sys` build script downloads a 277 MB prebuilt zip from GitHub Releases (1.1 GB unpacked) with no checksum check. On a slow link its HTTP request times out. Workaround: download the zip yourself, unzip it, and set `LK_CUSTOM_WEBRTC` to the `mac-arm64-release` folder.
- macOS binaries must be linked with `-ObjC`. The dependency's own link argument does not reach our binary; without it the app crashes at start with `+[NSString stringForStdString:] unrecognized selector`. The client's `build.rs` emits `cargo:rustc-link-arg-bins=-ObjC`.
- Dropping a received `DataChannel` handle silently drops its message handler. Keep received channels and tracks alive.
- A data channel send before the channel is `Open` is lost without an error.

## Screen share notes (phase 6)

- macOS uses the system picker (displays and windows); Windows lists displays and windows in the app. Capture is scaled down to fit 1080p, 30 fps, 6 Mbps cap. Under congestion the encoder lowers frame rate before resolution (`DegradationPreference::MaintainResolution`).
- Only the side that dialed ever sends offers. The other side sends `Renegotiate`, so offers never cross. Tried and dropped: sending offers from the sharer with rollback; one direction lost its video. `Renegotiate`, `ShareStarted` and `ShareEnded` are `RtcSignal` variants in `crates/protocol`, relayed through the signaling WebSocket.
- A sender is never emptied: `RtpSender::set_track(None)` crashes webrtc-sys, so an idle black track is swapped in and `ShareEnded` tells viewers to hide the tile. `ShareStarted` re-announces a later share on the same connection.
- webrtc-sys can call a freed observer after a peer connection closes (crash in `on_ice_connection_change`). `Link::drop` closes the connection and keeps a handle alive for 3 s. The 3 s value is a heuristic, not derived from the library.
- Not yet verified: a real capture, Screen Recording permission flow, picker cancel, Windows.

## System audio notes (phase 7)

- New crate `sysaudio`: `start(callback)` delivers 48 kHz stereo `i16` in exact 10 ms chunks. macOS: ScreenCaptureKit with the current process excluded. Windows: `wasapi` crate (wraps the `windows` crate) process loopback in exclude mode, needs Windows 10 2004+. Excluding the app prevents remote voices from being re-captured and echoed back to the people who spoke.
- The computer's sound is its own audio track ("system" stream id), present on every connection from the start, because the answering side cannot add tracks later. Listeners hear it mixed with the voice by the platform playout. Level measurement for the speaking indicator ignores it, so a video's sound does not light the speaking dot.
- ScreenCaptureKit's bindings need the Swift runtime: `build.rs` of `sysaudio` and `client` add `-rpath /usr/lib/swift` on macOS (without it the binary aborts at start).
- Verified: live capture on the author's Mac (300 chunks in 3 s, `cargo test -p sysaudio -- --ignored`); mesh test that computer sound arrives with sound and is not counted as speech; Windows crate type-checks with `cargo check -p sysaudio --target x86_64-pc-windows-msvc`. Not verified: the Windows capture itself, the checkbox in the running app, hearing it on a second machine.

## Release notes (phase 8)

- Verified locally: the deploy stack (`db`, `server`, `coturn`) builds and starts with Docker Compose on the author's Mac; signup, login, `/healthz`, and `create-user` (password piped on stdin) work against it; coturn accepts the config. `docker compose config` passes for the default and `tls` profiles. Not verified: a real VPS, public IP and firewall, the Caddy profile (needs a domain), TURN relaying across a real NAT.
- `TRUST_PROXY=true` reads the last `X-Forwarded-For` entry for rate limits; without it, everyone behind Caddy would share one login limit. Use it only when the proxy is the sole way in, or clients can spoof the header.
- coturn: private and loopback peer ranges are denied so relays cannot reach the server's own network; `--external-ip` takes `PUBLIC_IP` because it rejects domain names.
- macOS ships as an ad-hoc-signed `ScreenTalk.app` (`scripts/bundle-macos.sh`, `NSMicrophoneUsageDescription` in Info.plist); built and launched locally. Windows ships `client.exe` in a zip, with the console window hidden in release builds. Neither is signed with a developer certificate, so macOS Gatekeeper and Windows SmartScreen warn on first run.
- CI (`.github/workflows/ci.yml`) has not run: jobs are server tests, deploy build and compose check, client on Windows and macOS with packaged artifacts, a Linux client-net job (`continue-on-error`, libwebrtc's Linux system libraries unverified), and a tagged-release job (`v*`).
