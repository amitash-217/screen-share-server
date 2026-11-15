# Stage 6: Comprehension Report

Target: `docs/PLAN.md` (lines 1-83). Reviewer `egm-comprehension` read the document alone. It did not check whether referenced files exist.

## What this proposes (reviewer paraphrase)

- A native Rust voice and screen-share app for Windows and macOS. Rooms hold 2 to 4 people. Clients connect directly over WebRTC (full mesh). A Rust server handles accounts, rooms and signaling, and issues TURN credentials.
- Client: Slint UI plus `libwebrtc` bindings. Server: `axum`, WebSocket, Postgres, coturn.
- Username and password only. A user owns at most 3 rooms, at most 1 permanent. Joining needs host approval.
- 8 phases, all marked done. The notes record what was and was not verified. Most media features were not tested on real hardware or a real VPS.

## How the system works, per the document

- 5-letter room codes. Join requests wait for the host. A temporary room is destroyed when the host disconnects; a permanent room keeps running. One room per user at a time.
- Only the dialing side sends offers; the other side sends `Renegotiate`. An ended share is replaced by an idle black track. Each connection carries an extra "system" audio track captured by `sysaudio`.
- Deployment: Docker Compose (`db`, `server`, `coturn`, optional `tls` profile). `TRUST_PROXY` reads the last `X-Forwarded-For` entry.

## Findings

### Major

1. **Lines 3, 16: transport guarantee unclear.** The document does not say what the mesh carries, whether TURN is only a fallback, or what TLS covers (WebSocket, HTTP, or both) and what "allow unencrypted" is for.
2. **Lines 9-14: join and membership model has gaps.** Unstated: who can remove members of a permanent room while the host is offline; what a requester sees while pending; how long requests live; what happens when a user enters another room while hosting a permanent one; whether room codes are reused after delete and who deletes a permanent room.
3. **Line 35: undefined terms and a contradiction.** "Platform playout layer" and `NativeAudioStream` are never defined. Line 17 lists per-peer volume as a v1 feature; line 35 defers it. The document does not say whether v1 ships without it.
4. **Lines 29-38: "done" is undefined.** Phases are "done" while their notes say untested (no live mic, no real capture, Windows compile-only, CI and VPS never run). Line 58 says CI "needs checking in phase 5", but phase 5 is done and line 83 says CI never ran. There is no remaining-before-release list.
5. **Lines 37, 40-53, 72: spike section has no referent.** "This machine" and "the user" are undefined. The spike crate, its date and its relation to the client are not given. Line 43 leaves a 450 Hz vs 440 Hz peak "cause not traced" without saying if that is acceptable.
6. **Lines 62-68: protocol terms undefined.** "Side that dialed", `Renegotiate`, `ShareStarted`, `ShareEnded`, "idle black track", `Link`, "stream id". The document does not say where the messages are defined (line 21 names `crates/protocol` only) or that they travel over the signaling WebSocket. "Tried and dropped... one direction lost its video" is vague. "6 Mbps cap, resolution kept before frame rate" is ambiguous.
7. **Lines 72-74: system audio assumes background knowledge.** The echo loop (remote audio played locally, re-captured, re-shared) is not explained. "The answering side cannot add tracks later" is asserted without reason. "Levels ignore it" (speaking indicator) and "mixed by the platform playout" are unclear.
8. **Lines 33, 79-80: TURN and rate limits underspecified.** "HMAC of `TURN_SECRET`" lacks the shared-secret and time-limit explanation. Rate-limit values are not given. Caddy is not introduced as the TLS proxy before `TRUST_PROXY` is explained.

### Nit

9. **Lines 9, 32, 79:** the admin CLI invocation is never shown in one place. Session token lifetime and revocation are not stated.
10. **Lines 49-50, 3:** the `mac-arm64-release` layout is assumed; Intel macOS and Windows are not addressed.
11. **Lines 59, phase 7:** minimum OS versions (macOS 13+, Windows 10 2004+) are scattered; none consolidated.
12. **Lines 15, 35:** the hotkey is "a global hotkey" at line 15 and Ctrl+Alt+M at line 35. Not stated whether it can change or what the macOS equivalent is.

No section was unparaphrasable. No Blockers. Findings 3, 4 and 6 are closest: a new reader could not act on v1 scope or the protocol messages without outside context.
