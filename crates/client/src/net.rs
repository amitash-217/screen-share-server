//! HTTP sign-in and the WebSocket link to the server, with automatic reconnect.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMsg, Credentials, ServerMsg, Session};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::addr::ServerAddr;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Keeps NAT and proxy idle timers from dropping a quiet connection.
const PING_EVERY: Duration = Duration::from_secs(25);
const MAX_BACKOFF_SECS: u64 = 8;

#[derive(Debug)]
pub enum NetEvent {
    /// The WebSocket is up. The server sends the room list next.
    Connected,
    /// The WebSocket dropped; the link retries on its own.
    Disconnected,
    /// The server rejected the token. Reconnecting will not help; sign in again.
    AuthExpired,
    Server(ServerMsg),
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder().timeout(HTTP_TIMEOUT).build().expect("build http client")
}

fn describe(e: &reqwest::Error, addr: &ServerAddr) -> String {
    if e.is_timeout() {
        "the server did not answer in time".into()
    } else if e.is_connect() && addr.is_secure() {
        format!("could not connect to {} (no TLS there? tick \"allow unencrypted connection\" to use http)", addr.display())
    } else if e.is_connect() {
        format!("could not connect to {}", addr.display())
    } else {
        format!("request failed: {e}")
    }
}

/// Checks that something answers at the address.
pub async fn ping(addr: &ServerAddr) -> Result<(), String> {
    let resp = http_client().get(addr.http_url("/healthz")).send().await.map_err(|e| describe(&e, addr))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("that address answered {}; is it the right server?", resp.status()))
    }
}

/// Signs in or creates an account. The error text is meant for the user.
pub async fn authenticate(addr: &ServerAddr, signup: bool, creds: &Credentials) -> Result<Session, String> {
    let path = if signup { "/api/signup" } else { "/api/login" };
    let resp = http_client().post(addr.http_url(path)).json(creds).send().await.map_err(|e| describe(&e, addr))?;
    let status = resp.status();
    if status.is_success() {
        return resp.json::<Session>().await.map_err(|e| format!("unexpected reply from the server: {e}"));
    }
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    Err(body["error"].as_str().map(str::to_owned).unwrap_or_else(|| format!("the server answered {status}")))
}

/// Ends the session on the server. Failures do not matter: the token expires anyway.
pub async fn logout(addr: &ServerAddr, token: &str) {
    let _ = http_client().post(addr.http_url("/api/logout")).bearer_auth(token).send().await;
}

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

enum ConnectError {
    Unauthorized,
    Other,
}

async fn connect_once(addr: &ServerAddr, token: &str) -> Result<Ws, ConnectError> {
    let mut req = addr.ws_url().into_client_request().map_err(|_| ConnectError::Other)?;
    let auth = format!("Bearer {token}").parse().map_err(|_| ConnectError::Other)?;
    req.headers_mut().insert("authorization", auth);
    match tokio::time::timeout(HTTP_TIMEOUT, tokio_tungstenite::connect_async(req)).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(tungstenite::Error::Http(resp))) if resp.status() == 401 => Err(ConnectError::Unauthorized),
        _ => Err(ConnectError::Other),
    }
}

enum Outcome {
    /// The connection broke; try again.
    Lost,
    /// The caller is gone; stop for good.
    Stopped,
}

async fn pump(mut ws: Ws, cmds: &mut UnboundedReceiver<ClientMsg>, events: &UnboundedSender<NetEvent>) -> Outcome {
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        tokio::select! {
            frame = ws.next() => match frame {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerMsg>(text.as_str()) {
                    Ok(msg) => {
                        if events.send(NetEvent::Server(msg)).is_err() {
                            return Outcome::Stopped;
                        }
                    }
                    Err(e) => tracing::warn!("ignoring unreadable server message: {e}"),
                },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return Outcome::Lost,
                Some(Ok(_)) => {}
            },
            cmd = cmds.recv() => match cmd {
                Some(msg) => {
                    let Ok(json) = serde_json::to_string(&msg) else { continue };
                    if ws.send(Message::Text(json.into())).await.is_err() {
                        return Outcome::Lost;
                    }
                }
                None => {
                    let _ = ws.close(None).await;
                    return Outcome::Stopped;
                }
            },
            _ = ping.tick() => {
                if ws.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return Outcome::Lost;
                }
            }
        }
    }
}

/// Opens the WebSocket and keeps it open, reconnecting with a short backoff.
/// Send messages through the returned sender; dropping it ends the link.
/// Must be called inside a tokio runtime.
pub fn spawn_ws(addr: ServerAddr, token: String, events: UnboundedSender<NetEvent>) -> UnboundedSender<ClientMsg> {
    let (cmd_tx, mut cmds) = unbounded_channel::<ClientMsg>();
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            match connect_once(&addr, &token).await {
                Ok(ws) => {
                    backoff = 1;
                    if events.send(NetEvent::Connected).is_err() {
                        return;
                    }
                    let outcome = pump(ws, &mut cmds, &events).await;
                    let _ = events.send(NetEvent::Disconnected);
                    if matches!(outcome, Outcome::Stopped) {
                        return;
                    }
                }
                Err(ConnectError::Unauthorized) => {
                    let _ = events.send(NetEvent::AuthExpired);
                    return;
                }
                Err(ConnectError::Other) => {}
            }

            // Wait before retrying, but notice if the caller hung up meanwhile.
            let sleep = tokio::time::sleep(Duration::from_secs(backoff));
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    cmd = cmds.recv() => if cmd.is_none() { return },
                }
            }
            backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
        }
    });
    cmd_tx
}
