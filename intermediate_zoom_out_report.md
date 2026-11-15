# Stage 8: Intermediate Zoom Out Report

Target: `docs/PLAN.md` (lines 1-83). Reviewer: `egm-zoom-out`, standard `zoom-out-standard.md`. Inputs: `zoom_out_report.md`, `comprehension_report.md`, `critic_report.md`. The document was not edited since Stage 5. The reviewer did not search `crates/`, `README.md` or `libwebrtc` for the claims.

Verdict: pass with Major findings. No Blockers under the zoom-out standard. The three Blockers in `critic_report.md` come from the feature-spec standard, a different rubric.

## What Stages 6 and 7 added

- **Stage 6:** adds almost nothing new in substance. It confirms the status-log problem, the per-peer volume contradiction, the stale CI line and the scattered unverified work. Its new items are requests for definitions, which are reader-clarity issues.
- **Stage 7:** the scope risk. Blockers 1-3 and finding 9 ask for Alternatives, Problem Statement, Goals & Non-Goals and Proposed Architecture sections. Findings 4, 11, 12 and 15 ask for security, scale and supply-chain analysis. Taken together this turns a status log into a long spec, which is scope creep for a project with all phases built. Only the cheap, high-value parts should be taken.

## Major

1. **Lines 1-83: still a status log.** Smallest fix:
   - one short "Remaining before release" list at the top (covers Stage 5 #1, Critic 4, 7, 8, Comprehension 4);
   - a two-sentence problem statement at line 3 (Critic Blocker 2, Comprehension 1);
   - move the spike (40-53) and phase notes (62-83) to a notes file.
   Do not add full Alternatives, Architecture or Goals sections. One rationale line each for mesh vs SFU, Slint, and `libwebrtc` without LiveKit is enough, with the uplink math (6 Mbps x 3 peers is about 18 Mbps up).
2. **Lines 15, 35: per-peer volume.** Flagged in all three stages. Decide now: move it to "Out of v1" (line 17) or mark it deferred. Do not build `NativeAudioStream` plus a mixer (AEC risk, large build).
3. **Lines 57, 64: mesh uplink has no number or trigger.** Add a one-line rule, for example "max N simultaneous sharers per room" or "measure with 4 peers, then decide". No SFU, no simulcast.
4. **Lines 37, 38, 47, 58, 75, 83: stale and contradictory facts.** CI "needs checking in phase 5" vs phase 5 done and CI never run; Windows system audio "tested by the user" vs compile-only; system audio "not yet checked" vs verified. Fix the facts and mark phases "implemented, unverified" where true.
5. **Lines 49, 83: build and CI scope.** Drop the best-effort Linux `client-net` job unless it is cheap. Do not add a libwebrtc mirror in CI. Do add a checksum pin for the 277 MB download (small step, real supply-chain gain).

## Nit

6. **Lines 64-67:** keep one line per workaround in the plan, with detail in code comments. One Risks line for the upstream crash. Reject server-assigned polite/impolite roles: it replaces working code with no evidence of need.
7. **Line 16:** add one sentence that "allow unencrypted" is for local development only. No loopback enforcement unless the owner wants it.
8. **Lines 10-14, 80:** state the existing rate-limit values and the pending-request limit. No new moderation features.
9. **Lines 43, 82:** close or drop the "cause not traced" note. Add Gatekeeper and SmartScreen to Risks in one line.
10. **Comprehension 3, 6, 7, 8 and Critic nits 18-22:** definitions of `NativeAudioStream`, `Renegotiate`, the echo loop and the TURN HMAC belong in the README or code docs. Fix only line 72's phrasing and add a pointer to `crates/protocol`.

## Declined as scope creep

- A full Proposed Architecture section (Critic 9). Add one sentence that the live-room registry is in memory, so there is a single server and no horizontal scaling.
- A scale and failure-mode analysis (Critic 4).
- Simulcast or SVC, an optional SFU, a stalled-renegotiation watchdog (Catalyst 1, 2).
- A "request waiting" badge for join requests (Catalyst 3).
- A loopback smoke test in CI (Catalyst 4).

## Not checked

- Whether `crates/` matches the plan's claims.
- Whether `libwebrtc` exposes per-stream gain or simulcast.
- `README.md`: several findings ask for details that may already be there.

Main risk named by the reviewer: accepting every Critic Blocker grows the document and delays the real work, which is the first CI run, Windows verification and live-mic testing.
