# ScreenTalk

A native desktop app (Windows and macOS) for talking and sharing screens in small rooms of 2 to 4 people.
Everything is peer to peer over WebRTC; a small server you host yourself handles accounts, rooms and
signaling, and a TURN server (coturn) helps people behind strict networks connect.

- Voice with mute (global hotkey Ctrl+Alt+M), microphone and speaker pickers, echo cancellation and noise suppression, speaking indicators.
- Screen share of a display or a window, up to 1080p at 30 fps, several people at once, with the computer's sound if you want it. Click a screen to enlarge it.
- Accounts with a username and password only. No email.
- Rooms with a 5-letter code. Temporary rooms vanish when empty or when their host leaves. One permanent room per person stays until deleted. People ask to join with the code, and the host accepts or denies. Accepted members do not need to ask again.

Working name: "ScreenTalk" is a placeholder.

## Layout

| Path | What |
| --- | --- |
| `crates/protocol` | Messages and rules shared by client and server |
| `crates/server` | Accounts, rooms, signaling (axum, Postgres), plus the admin commands |
| `crates/client` | The desktop app (Slint UI, libwebrtc) |
| `crates/sysaudio` | Captures the computer's sound (ScreenCaptureKit on macOS, WASAPI on Windows) |
| `deploy/` | Docker Compose: server, Postgres, coturn, optional Caddy for HTTPS |
| `docs/PLAN.md` | Decisions, phases, findings, known risks |

## Run your own server

You need a Linux machine with Docker and a public IP address. A domain name is optional.

1. Copy the settings and edit them:

   ```sh
   cd deploy
   cp .env.example .env
   ```

   Set `POSTGRES_PASSWORD` and `TURN_SECRET` to long random values (`openssl rand -hex 24`), and `PUBLIC_HOST` and `PUBLIC_IP` to this machine's address.
   `PUBLIC_HOST` is what people type into the app (IP or domain); `PUBLIC_IP` must be the IP itself.

2. Open the firewall: `8080/tcp` (the app connects here), `3478/tcp+udp` (TURN), and `49152-65535/udp` (TURN relays).
   With HTTPS (below) open `80/tcp` and `443/tcp` instead of 8080.

3. Start it:

   ```sh
   docker compose up -d --build
   ```

   People can now connect with the server address `YOUR_IP:8080` and tick "Allow unencrypted connection" in the app.

### HTTPS with a domain name

Point a DNS record at the machine, set `PUBLIC_HOST` to the domain, and in `.env` set `SERVER_BIND=127.0.0.1` and `TRUST_PROXY=true`.
Then `docker compose --profile tls up -d --build`. Caddy gets and renews the certificate by itself.
People type the bare domain into the app and leave "Allow unencrypted" off.

`TRUST_PROXY=true` makes the server count login attempts per client address from `X-Forwarded-For`. Only set it when the proxy is the only way to reach the server (`SERVER_BIND=127.0.0.1`); otherwise anyone can fake their address.

### Accounts

Signup is open by default. To close it, set `SIGNUP_ENABLED=false` in `.env`, run `docker compose up -d`, and create accounts yourself:

```sh
docker compose exec server server create-user alice      # asks for the password twice
docker compose exec server server reset-password alice   # for a forgotten password; signs them out everywhere
```

Usernames are 3 to 24 characters of `a-z`, `0-9` and `_`. Passwords are 8 to 128 characters. There is no email, so there is no self-service reset: you are the reset.

### Operating notes

- Data lives in the `pgdata` Docker volume. Back it up with `docker compose exec db pg_dump -U app app > backup.sql`.
- Update with `git pull && docker compose up -d --build`. Migrations run at start. Temporary rooms are cleared at every start.
- Rate limits (login per address and per username, signup, join requests) are kept in memory and reset when the server restarts.
- Traffic between people goes directly between their computers when possible and through coturn otherwise, so the server's bandwidth is only used by people who cannot connect directly.

## Install the app

Download the zip for your system from the Releases page (or the CI run's artifacts).

- **macOS 13 or newer, Apple Silicon**: unzip, drag `ScreenTalk.app` to Applications. It is not signed with a developer certificate, so the first time, right-click it and choose Open. Allow Microphone when asked, and allow Screen Recording in System Settings > Privacy & Security when you first share (restart the app after allowing it).
- **Windows 10 (build 2004) or newer, 64-bit**: unzip and run `client.exe`. Windows SmartScreen may warn about an unknown publisher: choose "More info" then "Run anyway".

On first start, enter the server address and sign in or create an account.

## Build from source

Needs Rust (stable). The first build downloads libwebrtc's prebuilt binaries (about 280 MB, 1.1 GB unpacked) from GitHub.
If that download times out on a slow connection, fetch `webrtc-<platform>-release.zip` for the version named in the error yourself, unzip it, and point the build at it:

```sh
LK_CUSTOM_WEBRTC=/path/to/unzipped/mac-arm64-release cargo build --release -p client
scripts/bundle-macos.sh        # macOS: wraps the binary into target/bundle/ScreenTalk.app
```

Tests (the server and client network tests need a Postgres):

```sh
docker run -d --name ss-test-pg -e POSTGRES_USER=app -e POSTGRES_PASSWORD=test -e POSTGRES_DB=app -p 5433:5432 postgres:16
export DATABASE_URL=postgres://app:test@localhost:5433/app
cargo test --workspace -- --test-threads=4
cargo test -p sysaudio -- --ignored       # captures real computer sound; needs a desktop session
```

Without `DATABASE_URL`, the tests that need a database skip themselves.

## Known limits

- No per-person volume sliders (the platform audio layer has no per-track volume). Push-to-talk, chat and recording are not built.
- Rooms hold 4 people at most; everyone connects to everyone, so a share is uploaded once per viewer.
- Windows has only been compiled in CI, not run by the author. Please report what breaks.
- Not code-signed and no auto-update.
- A host cannot remove a member who is offline.
