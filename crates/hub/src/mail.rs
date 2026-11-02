//! Sending one email over SMTP (RFC 5321): the hub's sign-in links (`web/email.rs`). Enough
//! of the protocol for that and no more: TLS from the first byte (`smtps://`, port 465) or
//! STARTTLS (`smtp://`, required, port 587), AUTH PLAIN, one recipient, a plain-text body.
//! Certificates are checked against the platform's roots. Plain SMTP without TLS goes only
//! to this machine's loopback (a test's server). Nothing here logs the password or the body.

use std::{sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

/// How the connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tls {
    /// TLS from the first byte (`smtps://`).
    Implicit,
    /// STARTTLS before anything else (`smtp://`).
    StartTls,
    /// None: loopback only, for tests.
    Plain,
}

/// A mail server from its URL; never prints a password (none is in the URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub tls: Tls,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
}

const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_LINE: usize = 4096;

impl Server {
    /// `smtps://user@host[:465]` or `smtp://user@host[:587]`; `smtp://…?tls=none` only to
    /// loopback. A password in the URL is refused: it comes from the secret store.
    pub fn parse(url: &str) -> Result<Self, String> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or("the SMTP URL is smtps://user@host:465 or smtp://user@host:587")?;
        let (authority, query) = rest.split_once('?').unwrap_or((rest, ""));
        let authority = authority.trim_end_matches('/');
        let (user, hostport) = match authority.rsplit_once('@') {
            Some((user, hostport)) => (Some(user), hostport),
            None => (None, authority),
        };
        if user.is_some_and(|user| user.contains(':')) {
            return Err(
                "the SMTP URL must not carry a password; set password_env from the secret store"
                    .into(),
            );
        }
        let user = user
            .map(|user| {
                percent_encoding::percent_decode_str(user)
                    .decode_utf8_lossy()
                    .into_owned()
            })
            .filter(|user| !user.is_empty());
        let (host, port) = match hostport.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') || hostport.starts_with('[') => (
                host.trim_start_matches('[').trim_end_matches(']'),
                Some(
                    port.parse::<u16>()
                        .map_err(|_| "the SMTP port is a number")?,
                ),
            ),
            _ => (hostport.trim_start_matches('[').trim_end_matches(']'), None),
        };
        if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c == '/') {
            return Err("the SMTP URL names no host".into());
        }
        let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
        let tls = match (scheme, query) {
            ("smtps", "") => Tls::Implicit,
            ("smtp", "") => Tls::StartTls,
            ("smtp", "tls=none") if loopback => Tls::Plain,
            ("smtp", "tls=none") => {
                return Err("SMTP without TLS goes only to this machine's loopback".into());
            }
            _ => return Err("the SMTP URL is smtps://… or smtp://…".into()),
        };
        Ok(Self {
            tls,
            host: host.to_ascii_lowercase(),
            port: port.unwrap_or(if tls == Tls::Implicit { 465 } else { 587 }),
            user,
        })
    }
}

/// The address in `Name <address>`, or the whole text when it is a bare address.
pub fn address_of(from: &str) -> Option<String> {
    let address = match (from.rfind('<'), from.rfind('>')) {
        (Some(open), Some(close)) if open < close => &from[open + 1..close],
        _ => from,
    }
    .trim();
    let ok = address.len() <= 254
        && address.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && (domain.contains('.') || domain == "localhost")
        })
        && !address
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>,;\"".contains(c));
    ok.then(|| address.to_string())
}

/// One message.
pub struct Message<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub subject: &'a str,
    pub body: &'a str,
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

struct Connection {
    stream: Option<Stream>,
    buffer: Vec<u8>,
}

impl Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        let done = match self.stream.as_mut().ok_or("closed")? {
            Stream::Plain(stream) => stream.write_all(bytes).await,
            Stream::Tls(stream) => stream.write_all(bytes).await,
        };
        done.map_err(|error| format!("SMTP write: {error}"))?;
        match self.stream.as_mut().ok_or("closed")? {
            Stream::Plain(stream) => stream.flush().await,
            Stream::Tls(stream) => stream.flush().await,
        }
        .map_err(|error| format!("SMTP write: {error}"))
    }

    async fn line(&mut self) -> Result<String, String> {
        loop {
            if let Some(end) = self.buffer.windows(2).position(|w| w == b"\r\n") {
                let line: Vec<u8> = self.buffer.drain(..end + 2).collect();
                return Ok(String::from_utf8_lossy(&line[..end]).into_owned());
            }
            if self.buffer.len() > MAX_LINE {
                return Err("the mail server sent a line too long".into());
            }
            let mut chunk = [0u8; 1024];
            let read = match self.stream.as_mut().ok_or("closed")? {
                Stream::Plain(stream) => stream.read(&mut chunk).await,
                Stream::Tls(stream) => stream.read(&mut chunk).await,
            }
            .map_err(|error| format!("SMTP read: {error}"))?;
            if read == 0 {
                return Err("the mail server closed the connection".into());
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    /// One reply (all its lines); refused unless its code is one of `want`.
    async fn reply(&mut self, want: &[u16]) -> Result<Vec<String>, String> {
        let mut lines = Vec::new();
        loop {
            let line = self.line().await?;
            let code: u16 = line.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| {
                "the mail server sent something that is not an SMTP reply".to_string()
            })?;
            let last = line.as_bytes().get(3) != Some(&b'-');
            lines.push(line.get(4..).unwrap_or("").to_string());
            if last {
                if want.contains(&code) {
                    return Ok(lines);
                }
                return Err(format!(
                    "the mail server answered {code}: {}",
                    lines.join(" ")
                ));
            }
        }
    }

    async fn command(&mut self, command: &str, want: &[u16]) -> Result<Vec<String>, String> {
        self.write(format!("{command}\r\n").as_bytes()).await?;
        self.reply(want).await
    }

    async fn start_tls(&mut self, host: &str) -> Result<(), String> {
        let Some(Stream::Plain(tcp)) = self.stream.take() else {
            return Err("TLS is already on".into());
        };
        self.buffer.clear();
        self.stream = Some(Stream::Tls(Box::new(tls(tcp, host).await?)));
        Ok(())
    }
}

async fn tls(
    tcp: TcpStream,
    host: &str,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_platform_verifier())
        .map_err(|error| format!("TLS setup: {error}"))?
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| format!("{host} is not a server name"))?;
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|error| format!("TLS with the mail server: {error}"))
}

/// The message as sent: headers, a blank line, the body with CRLF line ends and every
/// line that starts with a dot doubled (RFC 5321 §4.5.2).
fn data(message: &Message<'_>, id_host: &str) -> String {
    let clean = |text: &str| -> String { text.chars().filter(|c| !c.is_control()).collect() };
    let mut out = format!(
        "From: {}\r\nTo: <{}>\r\nSubject: {}\r\nMessage-ID: <{}@{}>\r\nMIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
        clean(message.from),
        clean(message.to),
        clean(message.subject),
        hex::encode(crate::link::random::<12>()),
        id_host
    );
    for line in message.body.replace("\r\n", "\n").split('\n') {
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str(".\r\n");
    out
}

/// Send `message` through `server`, signing in as its user with `password`.
pub async fn send(
    server: &Server,
    password: Option<&str>,
    message: &Message<'_>,
) -> Result<(), String> {
    tokio::time::timeout(TIMEOUT, deliver(server, password, message))
        .await
        .map_err(|_| "the mail server did not answer in time".to_string())?
}

async fn deliver(
    server: &Server,
    password: Option<&str>,
    message: &Message<'_>,
) -> Result<(), String> {
    let from = address_of(message.from).ok_or("the sender has no address")?;
    let to = address_of(message.to).ok_or("the recipient is not an address")?;
    let tcp = TcpStream::connect((server.host.as_str(), server.port))
        .await
        .map_err(|error| format!("{}:{}: {error}", server.host, server.port))?;
    let stream = match server.tls {
        Tls::Implicit => Stream::Tls(Box::new(tls(tcp, &server.host).await?)),
        Tls::StartTls | Tls::Plain => Stream::Plain(tcp),
    };
    let mut connection = Connection {
        stream: Some(stream),
        buffer: Vec::new(),
    };
    let me = from
        .rsplit_once('@')
        .map(|(_, d)| d.to_string())
        .unwrap_or_default();
    connection.reply(&[220]).await?;
    let mut offered = connection.command(&format!("EHLO {me}"), &[250]).await?;
    if server.tls == Tls::StartTls {
        if !offered
            .iter()
            .any(|line| line.eq_ignore_ascii_case("STARTTLS"))
        {
            return Err("the mail server does not offer STARTTLS; use smtps://".into());
        }
        connection.command("STARTTLS", &[220]).await?;
        connection.start_tls(&server.host).await?;
        offered = connection.command(&format!("EHLO {me}"), &[250]).await?;
    }
    if let Some(user) = &server.user {
        let password = password.ok_or("the mail server's user has no password set")?;
        if !offered.iter().any(|line| {
            line.to_ascii_uppercase().starts_with("AUTH")
                && line.to_ascii_uppercase().contains("PLAIN")
        }) {
            return Err("the mail server does not offer AUTH PLAIN".into());
        }
        let token = STANDARD.encode(format!("\0{user}\0{password}"));
        connection
            .command(&format!("AUTH PLAIN {token}"), &[235])
            .await
            .map_err(|_| "the mail server refused the user or password".to_string())?;
    }
    connection
        .command(&format!("MAIL FROM:<{from}>"), &[250])
        .await?;
    connection
        .command(&format!("RCPT TO:<{to}>"), &[250, 251])
        .await?;
    connection.command("DATA", &[354]).await?;
    connection.write(data(message, &me).as_bytes()).await?;
    connection.reply(&[250]).await?;
    let _ = connection.command("QUIT", &[221]).await;
    Ok(())
}

#[cfg(test)]
pub mod stub {
    //! A mail server for tests: plain SMTP on loopback, any AUTH accepted, every message kept.

    use std::sync::{Arc, Mutex};

    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };

    #[derive(Debug, Clone, Default)]
    pub struct Received {
        pub from: String,
        pub to: String,
        pub data: String,
        pub auth: Option<String>,
    }

    pub struct Stub {
        pub url: String,
        pub received: Arc<Mutex<Vec<Received>>>,
    }

    pub async fn start(user: &str) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "smtp://{user}@127.0.0.1:{}?tls=none",
            listener.local_addr().unwrap().port()
        );
        let received = Arc::new(Mutex::new(Vec::new()));
        let kept = received.clone();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let kept = kept.clone();
                tokio::spawn(async move {
                    let (read, mut write) = socket.into_split();
                    let mut lines = BufReader::new(read).lines();
                    let _ = write.write_all(b"220 stub ESMTP\r\n").await;
                    let mut message = Received::default();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let upper = line.to_ascii_uppercase();
                        let answer: &[u8] = if upper.starts_with("EHLO") {
                            b"250-stub\r\n250 AUTH PLAIN\r\n"
                        } else if upper.starts_with("AUTH PLAIN") {
                            message.auth = Some(line[11..].to_string());
                            b"235 ok\r\n"
                        } else if upper.starts_with("MAIL FROM:") {
                            message.from = line[10..].trim_matches(['<', '>']).to_string();
                            b"250 ok\r\n"
                        } else if upper.starts_with("RCPT TO:") {
                            message.to = line[8..].trim_matches(['<', '>']).to_string();
                            b"250 ok\r\n"
                        } else if upper == "DATA" {
                            let _ = write.write_all(b"354 go\r\n").await;
                            while let Ok(Some(line)) = lines.next_line().await {
                                if line == "." {
                                    break;
                                }
                                message.data.push_str(&line);
                                message.data.push('\n');
                            }
                            kept.lock().unwrap().push(std::mem::take(&mut message));
                            b"250 queued\r\n"
                        } else if upper == "QUIT" {
                            let _ = write.write_all(b"221 bye\r\n").await;
                            return;
                        } else {
                            b"502 no\r\n"
                        };
                        let _ = write.write_all(answer).await;
                    }
                });
            }
        });
        Stub { url, received }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_need_tls_except_to_loopback_and_never_carry_a_password() {
        assert_eq!(
            Server::parse("smtps://login%40example.com@smtp.example.com").unwrap(),
            Server {
                tls: Tls::Implicit,
                host: "smtp.example.com".into(),
                port: 465,
                user: Some("login@example.com".into()),
            }
        );
        let start = Server::parse("smtp://apikey@smtp.sendgrid.net:587").unwrap();
        assert_eq!((start.tls, start.port), (Tls::StartTls, 587));
        assert_eq!(
            Server::parse("smtp://127.0.0.1:2525?tls=none").unwrap().tls,
            Tls::Plain
        );
        for bad in [
            "smtps://user:hunter2@smtp.example.com",
            "smtp://smtp.example.com?tls=none",
            "http://smtp.example.com",
            "smtps://",
        ] {
            let refused = Server::parse(bad).unwrap_err();
            assert!(!refused.contains("hunter2"), "{refused}");
        }
    }

    #[test]
    fn senders_and_recipients_are_one_address() {
        assert_eq!(
            address_of("Devagotchi <sign-in@devagotchi.com>").as_deref(),
            Some("sign-in@devagotchi.com")
        );
        assert_eq!(address_of("a@b.co").as_deref(), Some("a@b.co"));
        for bad in ["nobody", "a@b.co, c@d.co", "x@y.z\r\nBcc: e@f.g", "<>"] {
            assert_eq!(address_of(bad), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn a_message_reaches_the_server_dot_stuffed_with_auth() {
        let stub = stub::start("hub").await;
        let server = Server::parse(&stub.url).unwrap();
        send(
            &server,
            Some("secret"),
            &Message {
                from: "Devagotchi <sign-in@devagotchi.com>",
                to: "jesse@example.com",
                subject: "Your sign-in link\r\nBcc: evil@example.com",
                body: "Open it:\n.hidden line\nhttps://hub.example/auth/email#tok",
            },
        )
        .await
        .unwrap();
        let received = stub.received.lock().unwrap().clone();
        assert_eq!(received.len(), 1);
        let got = &received[0];
        assert_eq!(
            (got.from.as_str(), got.to.as_str()),
            ("sign-in@devagotchi.com", "jesse@example.com")
        );
        assert_eq!(
            got.auth.as_deref(),
            Some(STANDARD.encode("\0hub\0secret").as_str())
        );
        assert!(got.data.contains("\n..hidden line\n"), "{}", got.data);
        assert!(!got.data.contains("\nBcc:"), "{}", got.data);
        // A user with no password is refused before anything is sent.
        let refused = send(
            &server,
            None,
            &Message {
                from: "a@b.co",
                to: "c@d.co",
                subject: "s",
                body: "b",
            },
        )
        .await
        .unwrap_err();
        assert!(refused.contains("no password"), "{refused}");
    }
}
