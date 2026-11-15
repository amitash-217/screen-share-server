# Stage 5: Initial Zoom Out Report

Target: `docs/PLAN.md` (lines 1-83). Reviewers: `egm-zoom-out`, `egm-inventive-catalyst`. Neither reviewer read the source code; both checked the plan text (zoom-out also checked the file tree).

## Zoom Out (simplicity and scope)

Verdict: pass with Major and Nit findings. Context is sufficient. Reuse and crate placement are sound: the plan reuses the libwebrtc platform audio layer instead of custom `cpal` code, and `sysaudio` has no existing equivalent.

### Major

1. **Lines 1-83: the document is a status log, not a plan.** Every phase is "done". Decisions, spike logs, workarounds and release notes are mixed. The open work is scattered over "Not yet verified" bullets at lines 35, 36, 37, 38, 53, 68, 75 and 79 (Windows system audio and capture, live mic, two real machines, real VPS, CI never run, per-peer volume). Fix: add one "Remaining / unverified" list at the top; move spike and phase notes to an appendix or a separate notes file.
2. **Lines 15 and 35: per-peer volume is promised, then deferred.** The proposed fix (NativeAudioStream, own mixer, own output) is large and risky for 2-4 person rooms. Either move the feature to "Out of v1" (line 17) or say plainly that it is deferred. Reconcile the two lines.
3. **Lines 57 and 64: mesh upload risk is unmeasured, with no trigger.** Four sharers at 1080p30 with a 6 Mbps cap is up to 18 Mbps up per sharer with 3 peers. Line 53 says CPU with 3 peers is unchecked. Do not build an SFU now. Record a trigger instead, such as "measure CPU and uplink with 4 peers; switch to an SFU above X".
4. **Lines 55-60, 79-83: items unverified, CI risk stale.** Line 58 says CI cache and build time "need checking in phase 5", but phase 8 is done and line 83 says CI never ran. The best-effort Linux `client-net` job (`continue-on-error`) is speculative scope for a Windows/macOS app. Treat the first CI run as a release blocker.

### Nit

5. **Lines 40-53, 62-82: too much detail for a plan.** `RtpSender::set_track(None)` crash, `Link::drop` 3 s keep-alive, `-ObjC` and `-rpath` flags, `LK_CUSTOM_WEBRTC` belong in code comments, `build.rs` or the README. Keep one line per decision.
6. **Line 43: "cause not traced" for the 450 Hz peak on a 440 Hz input.** Close it (likely 5 Hz scan resolution) or drop it.
7. **Line 16: "allow unencrypted" checkbox with no insecure warning.** Say it is for local development only, or add a visible warning.
8. **Line 82: ad-hoc macOS signing, unsigned Windows.** Scoped out of v1 (line 17). Note the Gatekeeper and SmartScreen cost in the README.

## Inventive Catalyst (false compromises)

### Major

1. **Mesh upload ceiling accepted as fixed** (Risks "Mesh upload", Decisions: full mesh, 1080p30, multiple sharers). The 6 Mbps cap is the same for every room, whatever the peer count or uplink.
   - Resolution options: scale per-peer bitrate and resolution with viewer count and measured uplink (divide a total uplink budget across peers; lower frame rate or resolution for non-enlarged tiles). Keep an optional stateless SFU process, enabled per room when estimated uplink is too high; signaling and protocol stay the same. Simulcast or SVC layers let a viewer pick a layer.
   - Liabilities: bitrate behavior near the edges needs tuning and tests on real uplinks; an SFU adds a second media path, server bandwidth cost and ends pure P2P; simulcast adds sharer encoder CPU.
2. **Negotiation and libwebrtc workarounds accepted as "how it is"** (Screen Share notes, System audio notes, spike results). Dialer-only offers, an always-swapped idle black track, a permanent "system" track on every connection, a 3 s keep-alive against a use-after-free. The idle tracks and keep-alive are standing overhead.
   - Resolution options: server assigns polite/impolite roles per pair and clients run perfect negotiation (replaces the dial-order rule); make the fixed transceiver set (mic, screen, system audio) the explicit design instead of a patch; add a watchdog that renegotiates when a direction stalls (the plan has only a "tried and dropped" note).
   - Liabilities: more server logic and signaling dependence; idle m-lines and a fixed upper bound on tracks; a flawed health check can loop.

### Nit

3. **Two small unexamined trade-offs** (Decisions "Host disconnect", "Join"; per-participant volume). Join requests wait only while the host is offline, and the host learns of them on return only: reuse the pending queue for a "request waiting" badge on the room list. Per-peer volume: route each remote track through its own `NativeAudioStream` into a small mixer with per-stream gain. The plan does not consider receiver-side gain. Liabilities: bypassing platform playout may degrade echo cancellation (AEC reference signal); a notification path adds server state.
4. **"Done" phases never verified** (CI never ran, Windows compile-only, no real VPS, no live mic or capture). The 277 MB libwebrtc download is handled by a manual workaround. Resolution: cache or mirror the prebuilt libwebrtc artifact in CI with a build-script fallback; add a loopback media smoke test using the spike's in-process peers; mark phases done only when their verification has run. Liabilities: mirror upkeep, CI minutes, possible flakiness on shared runners.

Not flagged by the Inventive Catalyst: no email accounts, TLS plus the unencrypted checkbox, one room at a time, the 2-4 person bound.

## Not checked

- Whether `crates/` matches the plan's claims.
- Whether the libwebrtc crate exposes per-stream gain or simulcast.
- `README.md`.
