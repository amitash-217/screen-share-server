# Stage 7: Critic Review Report

Target: `docs/PLAN.md`, FeatureRange lines 1-83. Reviewers: `egm-design`, `egm-structural`, `egm-style`, all against `feature-spec-standard.md`. None checked claims against the code. Findings from the three reviewers are merged and deduplicated; the reviewers that raised each are in brackets (D design, S structural, T style).

Verdict: FAIL against the standard. Three required sections are missing, and Risks is thin.

## Blockers

1. **No Alternatives Considered section** (lines 5-17, 57, 65) [D, S, T]. Every choice is "settled" with no rejected option and no reason: full mesh vs SFU or LiveKit, Slint, `libwebrtc` bindings, no webview, axum, Postgres, coturn. Line 57 ("SFU is the later fix") is a deferral, not a rejection; line 65 ("tried and dropped") is the only real rejection. Fix: add the section; for the mesh, give the uplink math.
2. **No Problem Statement** (line 3) [D, S, T]. The document states what the app is, not the problem it solves or why it should exist.
3. **No Goals & Non-Goals** (line 17) [D, S, T]. "Out of v1" is a bullet with no reasons; there are no goals or success criteria.

## Major

4. **Risks & Mitigations incomplete** (lines 55-60, also 53, 68, 75, 79) [D, S, T]. Only the mesh uplink has a mitigation, and it is vague. Missing: security (password and session-token storage, expiry, revocation, WebSocket auth, TURN abuse), failure modes (Postgres or server outage, TURN on symmetric NAT), scale limits (concurrent rooms, WebSocket connections, TURN relay bandwidth on the VPS). Unverified items are scattered over four places and should be one list with an owner or decision each.
5. **Mesh uplink is a hard limit, not a later concern** (lines 57, 64) [D]. At 6 Mbps with 3 peers one sharer uploads about 18 Mbps. A 4-person room with all four sharing is 3 viewers per sharer. No per-room sharer cap or bitrate-by-peer-count is stated. The 2-4 person limit is not tied to a bandwidth or CPU budget (line 3, 15) [D].
6. **Stale and contradictory statements** [S, T, D]:
   - Line 58 says CI needs checking "in phase 5"; phase 5 is done (line 35) and CI never ran (line 83).
   - Line 37 says Windows system audio was tested by the user on a real machine; lines 38 and 75 say Windows is unverified.
   - Line 47 says system audio is "not yet checked"; line 37 says it was verified.
   - Lines 15 and 35: per-peer volume is a feature and also deferred.
7. **Status labels outrun evidence** (lines 35-38) [D, T]. "Done" is used for work with no live mic, no real capture, Windows compile-only, CI and VPS never run. Use "implemented, unverified" plus acceptance criteria.
8. **Document is a status log, not a spec** (whole file; order L6-17, L29-38, L40-83) [D, S, T]. Phases, spike results and phase notes dominate. Risks should follow the architecture; phase notes belong beside Phases or in a notes file.
9. **No Proposed Architecture** (lines 19-27, 64-67, 72-74) [D, S]. Only a file tree. Missing: signaling and media topology, data flow, WebSocket protocol overview, membership persistence, how TURN credentials and the live-room registry work. The registry is in memory, so the server is a single point of failure and cannot scale horizontally; this is not stated.
10. **Transport security** (line 16) [D, T]. "Allow unencrypted" with "no insecure badge" sends passwords and tokens in the clear with no warning. Justify it, add a warning, or restrict it to loopback or LAN.
11. **Auth and rate limiting** (lines 9, 33, 80) [D]. No rate-limit numbers. Per-username limits allow lockout of a victim. `TRUST_PROXY=true` is safe only when the proxy is the sole ingress; document that.
12. **Room-semantics edge cases** (lines 10-14) [D]. 5 lowercase letters is about 11.9M codes, enumerable without strong limits. Pending join requests have no cap or expiry. A permanent room with an offline host has no moderation. A freed code can be reused and reach an unrelated room.
13. **Audio design** (lines 35, 45, 72-75) [D]. No verified echo-cancellation path on macOS; app-exclusion is untested on a second machine.
14. **Fragile workarounds** (lines 64-67) [D, T]. `set_track(None)` crash and the 3 s keep-alive are a race, not a fix. Link upstream issues or a reproduction; say how the timer will be removed; the 3 s value is unexplained. State the crash risk under Risks.
15. **Build supply chain** (lines 49, 58, 83) [D]. A 277 MB prebuilt binary from GitHub Releases with no stated integrity check. Pin the version and checksum. CI cost and time are unquantified.
16. **Spike claim outruns evidence** (line 43) [D, T]. A 450 Hz peak for a 440 Hz input is a 10 Hz shift, above the 5 Hz scan resolution. "Tone survives Opus" is unsupported until traced.

## Nit

17. **Line 82:** unsigned apps cause Gatekeeper and SmartScreen warnings; add to Risks [D].
18. **Line 6:** drop "(settled)" and add rationale per decision [S, T].
19. **Line 48:** plain-text lead-in "Build facts..." acts as a heading; use H3 or fold into the list [S].
20. **Heading and status format:** mixed "(phase N)" parentheticals, code spans in headings, and varied status wording on lines 31-38 [S].
21. **Long bullets:** lines 59 and 72 pack several facts per bullet; split [S].
22. **Wording** [T]: line 44 conclusion without trade-off; line 49 "times out" without a value; line 51 "Handles matter" is vague; line 64 "resolution kept before frame rate" is ambiguous about which drops first; line 72 rephrase to "prevents remote voices from being re-captured and echoed"; line 83 "best-effort" CI job has no stated release consequence.

## Overlap with earlier stages

Findings 4-8 and 16 repeat Stage 5 and Stage 6 items (status log, per-peer volume, stale CI line, unverified work). Blockers 1-3 are new.
