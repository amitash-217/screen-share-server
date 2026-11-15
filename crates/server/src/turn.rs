use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use hmac::{Hmac, Mac};
use protocol::{IceServer, UserId};
use sha1::Sha1;

const CREDENTIAL_LIFETIME_SECS: u64 = 24 * 60 * 60;
const TURN_PORT: u16 = 3478;

/// Settings for the coturn server started by `deploy/docker-compose.yml`.
pub struct TurnConfig {
    /// Same value as coturn's `--static-auth-secret`.
    pub secret: String,
    /// Public IP or domain of the VPS.
    pub host: String,
}

impl TurnConfig {
    /// Time-limited credentials in coturn's `use-auth-secret` format:
    /// username `<expiry>:<user>`, password base64(HMAC-SHA1(secret, username)).
    pub fn ice_servers(&self, user: UserId, now_unix: u64) -> Vec<IceServer> {
        let username = format!("{}:{}", now_unix + CREDENTIAL_LIFETIME_SECS, user);
        let mut mac = Hmac::<Sha1>::new_from_slice(self.secret.as_bytes()).expect("hmac takes any key length");
        mac.update(username.as_bytes());
        let credential = STANDARD.encode(mac.finalize().into_bytes());
        let host = &self.host;
        vec![IceServer {
            urls: vec![
                format!("stun:{host}:{TURN_PORT}"),
                format!("turn:{host}:{TURN_PORT}?transport=udp"),
                format!("turn:{host}:{TURN_PORT}?transport=tcp"),
            ],
            username: Some(username),
            credential: Some(credential),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_matches_openssl_reference() {
        // Expected value from: printf '1700086400:7' | openssl dgst -sha1 -hmac secret -binary | base64
        let turn = TurnConfig { secret: "secret".into(), host: "turn.example.com".into() };
        let ice = turn.ice_servers(7, 1_700_000_000);
        assert_eq!(ice[0].username.as_deref(), Some("1700086400:7"));
        assert_eq!(ice[0].credential.as_deref(), Some("i2ccdNMa2+bRRxPRRdcOb/9UxoE="));
        assert_eq!(ice[0].urls[0], "stun:turn.example.com:3478");
    }
}
