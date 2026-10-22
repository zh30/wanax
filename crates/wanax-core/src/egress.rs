//! Domain-allowlist egress proxy — the last isolation gap.
//!
//! Filesystem sandboxes (Seatbelt/bwrap) confine reads and writes, but a
//! spawned worker could still `curl` anything readable to any host. Mode
//! `allowlist`: children get HTTP(S)_PROXY → this localhost CONNECT/HTTP
//! proxy, which resolves DNS itself and tunnels only whitelisted domains;
//! Seatbelt then restricts outbound to `localhost:proxy` — even a direct
//! non-proxied connection can't leave. Mode `isolated`: no egress at all.

use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressMode {
    Off,
    Allowlist,
    Isolated,
}

impl EgressMode {
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "allowlist" => Self::Allowlist,
            "isolated" => Self::Isolated,
            _ => Self::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Allowlist => "allowlist",
            Self::Isolated => "isolated",
        }
    }
}

/// Domain allowlist: `"api.anthropic.com"` matches exactly; `"*.github.com"`
/// matches the domain and any subdomain.
#[derive(Debug, Clone, Default)]
pub struct EgressAllowlist {
    hosts: BTreeSet<String>,
}

impl EgressAllowlist {
    pub fn new<I, S>(hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let hosts = hosts
            .into_iter()
            .map(|h| h.into().trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        Self { hosts }
    }

    pub fn len(&self) -> usize {
        self.hosts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    pub fn allows(&self, host: &str) -> bool {
        let h = host.trim().to_ascii_lowercase();
        self.hosts.iter().any(|p| match p.strip_prefix("*.") {
            Some(suf) => h == suf || h.ends_with(&format!(".{suf}")),
            None => h == *p,
        })
    }
}

/// Extract the host from a config `base_url` (`https://api.x.com/v1` →
/// `api.x.com`). Deliberately string-level: a bad URL yields no entry.
pub fn host_of_base_url(base_url: &str) -> Option<String> {
    let no_scheme = base_url
        .trim()
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| base_url.trim());
    let authority = no_scheme.split('/').next()?.trim();
    if authority.is_empty() {
        return None;
    }
    // strip :port and any userinfo
    let host = authority
        .rsplit('@')
        .next()?
        .split(':')
        .next()?
        .trim()
        .to_ascii_lowercase();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// Running proxy; aborts on drop. `port` is where children must point their
/// HTTP(S)_PROXY env.
pub struct EgressProxy {
    pub port: u16,
    task: JoinHandle<()>,
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl EgressProxy {
    pub async fn start(
        allow: EgressAllowlist,
        on_deny: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let task = tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let allow = allow.clone();
                let on_deny = on_deny.clone();
                tokio::spawn(async move {
                    let _ = handle_conn(sock, &allow, &on_deny).await;
                });
            }
        });
        Ok(Self { port, task })
    }
}

/// Proxy env vars from a resolved URL.
pub fn proxy_env_from_url(url: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for k in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        out.push((k.to_string(), url.to_string()));
    }
    out.push(("NO_PROXY".into(), "localhost,127.0.0.1,::1".into()));
    out.push(("no_proxy".into(), "localhost,127.0.0.1,::1".into()));
    out
}

/// Proxy env vars a confined child needs. UPPER+lower so Node/curl/go alike
/// honor it; NO_PROXY keeps loopback direct.
pub fn proxy_env(port: u16) -> Vec<(String, String)> {
    proxy_env_from_url(&format!("http://127.0.0.1:{port}"))
}

const MAX_HEAD: usize = 64 * 1024;

async fn read_head(sock: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "eof",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(buf);
        }
        if buf.len() > MAX_HEAD {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "head too large",
            ));
        }
    }
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn handle_conn(
    mut sock: TcpStream,
    allow: &EgressAllowlist,
    on_deny: &Arc<dyn Fn(&str) + Send + Sync>,
) -> std::io::Result<()> {
    let head = read_head(&mut sock).await?;
    let text = String::from_utf8_lossy(&head);
    let Some(first) = text.lines().next() else {
        return Ok(());
    };
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method.eq_ignore_ascii_case("CONNECT") {
        // CONNECT api.x.com:443 — DNS resolves proxy-side; a child with
        // dead DNS can't leak hostnames anywhere else.
        let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(target);
        if !allow.allows(host) {
            on_deny(&format!("egress denied CONNECT {target}"));
            sock.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .await?;
            return Ok(());
        }
        let mut upstream = match TcpStream::connect(target).await {
            Ok(u) => u,
            Err(_) => {
                sock.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                    .await?;
                return Ok(());
            }
        };
        sock.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        tokio::io::copy_bidirectional(&mut sock, &mut upstream).await?;
        return Ok(());
    }

    // Plain HTTP: absolute-form URI or Host header → host[:port].
    let host = if let Some(rest) = target.strip_prefix("http://") {
        rest.split('/').next().unwrap_or("")
    } else {
        header(&text, "Host").unwrap_or("")
    };
    let (host_only, port) = match host.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') => (h, p.parse::<u16>().unwrap_or(80)),
        _ => (host, 80),
    };
    if host_only.is_empty() {
        sock.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }
    if !allow.allows(host_only) {
        on_deny(&format!("egress denied {method} {host}"));
        sock.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }
    let mut upstream = TcpStream::connect(format!("{host_only}:{port}")).await?;
    upstream.write_all(&head).await?;
    tokio::io::copy_bidirectional(&mut sock, &mut upstream).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn allowlist_exact_and_wildcard() {
        let a = EgressAllowlist::new(["api.anthropic.com", "*.github.com"]);
        assert!(a.allows("api.anthropic.com"));
        assert!(a.allows("API.Anthropic.com")); // case-insensitive
        assert!(a.allows("github.com"));
        assert!(a.allows("api.github.com"));
        assert!(!a.allows("evilgithub.com"));
        assert!(!a.allows("example.com"));
    }

    #[test]
    fn host_of_base_url_strips_scheme_and_port() {
        assert_eq!(
            host_of_base_url("https://api.openai.com/v1"),
            Some("api.openai.com".into())
        );
        assert_eq!(
            host_of_base_url("http://localhost:11434/api"),
            Some("localhost".into())
        );
        assert_eq!(host_of_base_url(""), None);
    }

    #[tokio::test]
    async fn connect_allowlisted_tunnels() {
        // upstream echo listener
        let up = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut b = [0u8; 16];
            let n = s.read(&mut b).await.unwrap();
            s.write_all(&b[..n]).await.unwrap();
        });
        let allow = EgressAllowlist::new(["127.0.0.1"]);
        let proxy = EgressProxy::start(allow, Arc::new(|_| {})).await.unwrap();
        let mut c = TcpStream::connect(format!("127.0.0.1:{}", proxy.port))
            .await
            .unwrap();
        c.write_all(format!("CONNECT 127.0.0.1:{up_port} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut resp = [0u8; 256];
        let n = c.read(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp[..n]).contains("200"));
        c.write_all(b"ping").await.unwrap();
        let n = c.read(&mut resp).await.unwrap();
        assert_eq!(&resp[..n], b"ping");
    }

    #[tokio::test]
    async fn plain_http_forwards_to_allowlisted_host() {
        // Upstream HTTP server that replies 200 to any request.
        let up = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut b = [0u8; 1024];
            let _ = s.read(&mut b).await.unwrap();
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi")
                .await
                .unwrap();
        });
        // 127.0.0.1 is allowlisted → GET must reach the upstream and get 200.
        let allow = EgressAllowlist::new(["127.0.0.1"]);
        let proxy = EgressProxy::start(allow, Arc::new(|_| {})).await.unwrap();
        let mut c = TcpStream::connect(format!("127.0.0.1:{}", proxy.port))
            .await
            .unwrap();
        c.write_all(
            format!("GET http://127.0.0.1:{up_port}/x HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        let mut resp = [0u8; 512];
        let n = c.read(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp[..n]);
        assert!(text.contains("200") && text.contains("hi"), "{text}");
    }

    #[tokio::test]
    async fn deny_callback_fires_and_returns_403() {
        let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hits2 = hits.clone();
        let allow = EgressAllowlist::new(["api.anthropic.com"]);
        let proxy = EgressProxy::start(
            allow,
            Arc::new(move |m: &str| hits2.lock().unwrap().push(m.to_string())),
        )
        .await
        .unwrap();
        let mut c = TcpStream::connect(format!("127.0.0.1:{}", proxy.port))
            .await
            .unwrap();
        c.write_all(b"GET http://evil.com/ HTTP/1.1\r\nHost: evil.com\r\n\r\n")
            .await
            .unwrap();
        let mut resp = [0u8; 256];
        let n = c.read(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp[..n]).contains("403"));
        let got = hits.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("evil.com"));
        // and the matched secret — the denial log — never carries a body.
        assert!(!got[0].contains("hunter"));
    }

    #[tokio::test]
    async fn connect_denied_returns_403() {
        let allow = EgressAllowlist::new(["api.anthropic.com"]);
        let proxy = EgressProxy::start(allow, Arc::new(|_| {})).await.unwrap();
        let mut c = TcpStream::connect(format!("127.0.0.1:{}", proxy.port))
            .await
            .unwrap();
        c.write_all(b"CONNECT evil.com:443 HTTP/1.1\r\nHost: evil.com\r\n\r\n")
            .await
            .unwrap();
        let mut resp = [0u8; 256];
        let n = c.read(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp[..n]).contains("403"));
    }
}
