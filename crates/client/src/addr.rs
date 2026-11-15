/// Port assumed for plain `http` when the user types none. TLS uses the scheme default.
const DEFAULT_PLAIN_PORT: u16 = 8080;

/// Where the server lives and whether to talk to it over TLS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAddr {
    secure: bool,
    /// `host` or `host:port`, IPv6 in brackets.
    authority: String,
}

impl ServerAddr {
    /// Accepts `host`, `host:port`, or a pasted URL; a scheme in the input is ignored
    /// because `secure` decides it.
    pub fn parse(input: &str, secure: bool) -> Result<Self, String> {
        let mut s = input.trim();
        for prefix in ["https://", "http://", "wss://", "ws://"] {
            if matches!(s.get(..prefix.len()), Some(head) if head.eq_ignore_ascii_case(prefix)) {
                s = &s[prefix.len()..];
                break;
            }
        }
        let s = s.split(['/', '?', '#']).next().unwrap_or("");
        if s.is_empty() {
            return Err("enter the server address".into());
        }
        let (host, port) = split_host_port(s)?;
        let authority = match (port, secure) {
            (Some(p), _) => format!("{host}:{p}"),
            (None, false) => format!("{host}:{DEFAULT_PLAIN_PORT}"),
            (None, true) => host.to_owned(),
        };
        Ok(Self { secure, authority })
    }

    pub fn is_secure(&self) -> bool {
        self.secure
    }

    /// Shown to the user, e.g. `https://chat.example.com`.
    pub fn display(&self) -> String {
        format!("{}://{}", self.http_scheme(), self.authority)
    }

    pub fn http_url(&self, path: &str) -> String {
        format!("{}://{}{}", self.http_scheme(), self.authority, path)
    }

    pub fn ws_url(&self) -> String {
        let scheme = if self.secure { "wss" } else { "ws" };
        format!("{scheme}://{}/ws", self.authority)
    }

    fn http_scheme(&self) -> &'static str {
        if self.secure {
            "https"
        } else {
            "http"
        }
    }
}

fn split_host_port(s: &str) -> Result<(&str, Option<u16>), String> {
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let end = rest.find(']').ok_or("missing ] in IPv6 address")?;
        let host = &s[..end + 2];
        let after = &rest[end + 1..];
        if !rest[..end].chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') || end == 0 {
            return Err("invalid IPv6 address".into());
        }
        match after {
            "" => (host, None),
            _ => (host, Some(after.strip_prefix(':').ok_or("invalid server address")?)),
        }
    } else {
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (s, None),
        };
        if host.contains(':') {
            return Err("wrap IPv6 addresses in [brackets]".into());
        }
        if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
            return Err("invalid server address".into());
        }
        (host, port)
    };
    let port = match port {
        None => None,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => Some(n),
            _ => return Err("invalid port".into()),
        },
    };
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str, secure: bool) -> Result<ServerAddr, String> {
        ServerAddr::parse(s, secure)
    }

    #[test]
    fn plain_without_port_gets_the_default_port() {
        let a = parse("203.0.113.10", false).unwrap();
        assert_eq!(a.http_url("/healthz"), "http://203.0.113.10:8080/healthz");
        assert_eq!(a.ws_url(), "ws://203.0.113.10:8080/ws");
        assert!(!a.is_secure());
    }

    #[test]
    fn tls_without_port_uses_the_scheme_default() {
        let a = parse("chat.example.com", true).unwrap();
        assert_eq!(a.http_url("/api/login"), "https://chat.example.com/api/login");
        assert_eq!(a.ws_url(), "wss://chat.example.com/ws");
        assert_eq!(a.display(), "https://chat.example.com");
    }

    #[test]
    fn explicit_port_wins() {
        assert_eq!(parse("example.com:9000", true).unwrap().ws_url(), "wss://example.com:9000/ws");
        assert_eq!(parse("10.0.0.5:9000", false).unwrap().ws_url(), "ws://10.0.0.5:9000/ws");
    }

    #[test]
    fn pasted_urls_are_reduced_to_the_host() {
        for input in ["http://example.com/", "HTTPS://example.com/some/path?x=1", "wss://example.com/ws", "  example.com  "] {
            assert_eq!(parse(input, true).unwrap().display(), "https://example.com", "{input:?}");
        }
        // The checkbox decides the scheme, not the pasted text.
        assert!(!parse("https://example.com", false).unwrap().is_secure());
    }

    #[test]
    fn ipv6_needs_brackets() {
        assert_eq!(parse("[::1]:9000", false).unwrap().ws_url(), "ws://[::1]:9000/ws");
        assert_eq!(parse("[::1]", false).unwrap().ws_url(), "ws://[::1]:8080/ws");
        assert!(parse("::1", false).is_err());
    }

    #[test]
    fn bad_input_is_rejected() {
        for bad in ["", "   ", "http://", "a b.com", "exa$mple.com", "host:0", "host:99999", "host:abc", ":8080", "[]", "[::1"] {
            assert!(parse(bad, false).is_err(), "{bad:?}");
        }
    }
}
