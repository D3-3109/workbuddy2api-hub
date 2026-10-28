use crate::{
    request::{PoolKey, Prepared},
    Error, ErrorKind, Result,
};
use bytes::{Buf, BytesMut};
use openssl::ssl::SslConnector;
use std::{
    collections::HashMap,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
    time::timeout,
};
use tokio_openssl::SslStream;

pub(crate) const MAX_HEADERS: usize = 64 * 1024;
const MAX_HEADER_COUNT: usize = 128;
const READ_SIZE: usize = 16 * 1024;
// Below the common 60-75 s server/LB keep-alive timeouts, so a pooled
// connection is rarely closed by the peer just as it is reused.
const IDLE_TTL: Duration = Duration::from_secs(55);
// Concurrent requests on one credential (shared affinity key, parallel agent
// calls) would otherwise reconnect and redo the TLS handshake every time.
const PER_KEY_CAP: usize = 16;
const GLOBAL_IDLE_CAP: usize = 256;

enum Stream {
    Plain(TcpStream),
    Tls(SslStream<TcpStream>),
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

pub(crate) struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub version: u8,
}

pub(crate) struct Connection {
    stream: Stream,
    pub buffer: BytesMut,
}

impl Connection {
    pub async fn connect(prepared: &Prepared, tls: &SslConnector, wait: Duration) -> Result<Self> {
        let (host, port) = prepared
            .proxy
            .as_ref()
            .map(|p| (p.host.as_str(), p.port))
            .unwrap_or((&prepared.key.host, prepared.key.port));
        let socket = TcpStream::connect((host, port))
            .await
            .map_err(|_| Error::new(ErrorKind::Connect, "TCP connect"))?;
        socket
            .set_nodelay(true)
            .map_err(|_| Error::new(ErrorKind::Connect, "TCP setup"))?;
        let mut connection = Self {
            stream: Stream::Plain(socket),
            buffer: BytesMut::new(),
        };
        if prepared.key.scheme == "https" {
            if let Some(proxy) = &prepared.proxy {
                let mut connect = format!(
                    "CONNECT {} HTTP/1.1\r\nHost: {}\r\n",
                    prepared.authority, prepared.authority
                );
                if let Some(auth) = &proxy.authorization {
                    connect.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
                }
                connect.push_str("\r\n");
                connection
                    .write_request(connect.as_bytes(), None, wait)
                    .await?;
                let head = connection.read_head(wait).await?;
                if !(200..300).contains(&head.status) || !connection.buffer.is_empty() {
                    return Err(Error::new(ErrorKind::Proxy, "proxy CONNECT"));
                }
            }
            let Stream::Plain(socket) = connection.stream else {
                unreachable!()
            };
            let ssl = tls
                .configure()
                .and_then(|cfg| cfg.into_ssl(&prepared.key.host))
                .map_err(|_| Error::new(ErrorKind::Tls, "TLS setup"))?;
            let mut stream =
                SslStream::new(ssl, socket).map_err(|_| Error::new(ErrorKind::Tls, "TLS setup"))?;
            Pin::new(&mut stream)
                .connect()
                .await
                .map_err(|_| Error::new(ErrorKind::Tls, "TLS handshake"))?;
            connection.stream = Stream::Tls(stream);
        }
        Ok(connection)
    }

    pub async fn write_request(
        &mut self,
        head: &[u8],
        body: Option<&[u8]>,
        wait: Duration,
    ) -> Result<()> {
        for bytes in [head, body.unwrap_or_default()] {
            let mut remaining = bytes;
            while !remaining.is_empty() {
                let written = timeout(wait, self.stream.write(remaining))
                    .await
                    .map_err(|_| Error::new(ErrorKind::Timeout, "request write"))?
                    .map_err(|_| Error::new(ErrorKind::Io, "request write"))?;
                if written == 0 {
                    return Err(Error::new(ErrorKind::Io, "request write"));
                }
                remaining = &remaining[written..];
            }
        }
        timeout(wait, self.stream.flush())
            .await
            .map_err(|_| Error::new(ErrorKind::Timeout, "request flush"))?
            .map_err(|_| Error::new(ErrorKind::Io, "request flush"))
    }

    pub async fn read_more(&mut self, wait: Duration, phase: &'static str) -> Result<usize> {
        self.buffer.reserve(READ_SIZE);
        let read = timeout(wait, self.stream.read_buf(&mut self.buffer))
            .await
            .map_err(|_| Error::new(ErrorKind::Timeout, phase))?
            .map_err(|e| {
                Error::new(
                    if e.kind() == io::ErrorKind::UnexpectedEof {
                        ErrorKind::Truncated
                    } else {
                        ErrorKind::Io
                    },
                    phase,
                )
            })?;
        Ok(read)
    }

    pub async fn read_head(&mut self, wait: Duration) -> Result<Head> {
        timeout(wait, self.read_head_inner(wait))
            .await
            .map_err(|_| Error::new(ErrorKind::Timeout, "response headers"))?
    }

    async fn read_head_inner(&mut self, wait: Duration) -> Result<Head> {
        let mut interim_count = 0;
        loop {
            if let Some(end) = self
                .buffer
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4)
            {
                if end > MAX_HEADERS {
                    return Err(Error::new(ErrorKind::Protocol, "response header limit"));
                }
                let mut headers = [httparse::EMPTY_HEADER; MAX_HEADER_COUNT];
                let mut parsed = httparse::Response::new(&mut headers);
                let result = parsed
                    .parse(&self.buffer[..end])
                    .map_err(|_| Error::new(ErrorKind::Protocol, "response headers"))?;
                if result != httparse::Status::Complete(end) {
                    return Err(Error::new(ErrorKind::Protocol, "response headers"));
                }
                let status = parsed
                    .code
                    .ok_or_else(|| Error::new(ErrorKind::Protocol, "response status"))?;
                let version = parsed
                    .version
                    .ok_or_else(|| Error::new(ErrorKind::Protocol, "response version"))?;
                if !(100..600).contains(&status) {
                    return Err(Error::new(ErrorKind::Protocol, "response status"));
                }
                let head = Head {
                    status,
                    version,
                    headers: parsed
                        .headers
                        .iter()
                        .map(|h| {
                            (
                                h.name.to_owned(),
                                h.value.iter().map(|b| char::from(*b)).collect(),
                            )
                        })
                        .collect(),
                };
                self.buffer.advance(end);
                if (100..200).contains(&status) && status != 101 {
                    interim_count += 1;
                    if interim_count > 8 {
                        return Err(Error::new(ErrorKind::Protocol, "interim response limit"));
                    }
                    continue;
                }
                return Ok(head);
            }
            if self.buffer.len() >= MAX_HEADERS {
                return Err(Error::new(ErrorKind::Protocol, "response header limit"));
            }
            if self.read_more(wait, "response headers").await? == 0 {
                return Err(Error::new(ErrorKind::Truncated, "response headers"));
            }
        }
    }

    fn idle_alive(&self) -> bool {
        if !self.buffer.is_empty() {
            return false;
        }
        let socket = match &self.stream {
            Stream::Plain(s) => s,
            Stream::Tls(s) => {
                if s.ssl().pending() > 0 {
                    return false;
                }
                s.get_ref()
            }
        };
        // Any data, EOF or error makes an idle connection unusable. Reading one
        // raw TLS byte is safe here because that connection is then discarded.
        matches!(socket.try_read(&mut [0u8; 1]), Err(e) if e.kind() == io::ErrorKind::WouldBlock)
    }
}

struct Idle {
    connection: Connection,
    since: Instant,
}

#[derive(Default)]
pub(crate) struct Pool {
    idle: HashMap<PoolKey, Vec<Idle>>,
}

impl Pool {
    fn sweep(&mut self) {
        self.idle.retain(|_, list| {
            list.retain(|idle| idle.since.elapsed() < IDLE_TTL);
            !list.is_empty()
        });
    }
    pub fn take(&mut self, key: &PoolKey) -> Option<Connection> {
        self.sweep();
        let mut found = None;
        if let Some(list) = self.idle.get_mut(key) {
            while let Some(idle) = list.pop() {
                if idle.connection.idle_alive() {
                    found = Some(idle.connection);
                    break;
                }
            }
            if list.is_empty() {
                self.idle.remove(key);
            }
        }
        found
    }
    pub fn put(&mut self, key: PoolKey, connection: Connection) {
        self.sweep();
        if !connection.idle_alive() {
            return;
        }
        let list = self.idle.entry(key).or_default();
        if list.len() == PER_KEY_CAP {
            list.remove(0);
        }
        list.push(Idle {
            connection,
            since: Instant::now(),
        });
        while self.idle.values().map(Vec::len).sum::<usize>() > GLOBAL_IDLE_CAP {
            let oldest = self
                .idle
                .iter()
                .filter_map(|(key, list)| list.first().map(|idle| (key, idle.since)))
                .min_by_key(|(_, since)| *since)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest {
                let list = self.idle.get_mut(&key).expect("existing idle key");
                list.remove(0);
                if list.is_empty() {
                    self.idle.remove(&key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_pool_is_bounded_and_expires() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut pool = Pool::default();
        let key = PoolKey {
            scheme: "http".into(),
            host: "127.0.0.1".into(),
            port: addr.port(),
            proxy: [0; 32],
            identity: [0; 32],
        };
        let mut peers = Vec::new();
        for _ in 0..PER_KEY_CAP + 1 {
            let socket = TcpStream::connect(addr).await.unwrap();
            peers.push(listener.accept().await.unwrap().0);
            pool.put(
                key.clone(),
                Connection {
                    stream: Stream::Plain(socket),
                    buffer: BytesMut::new(),
                },
            );
        }
        assert_eq!(pool.idle.get(&key).unwrap().len(), PER_KEY_CAP);
        for idle in pool.idle.get_mut(&key).unwrap() {
            idle.since = Instant::now() - IDLE_TTL - Duration::from_secs(1);
        }
        assert!(pool.take(&key).is_none());
        assert!(pool.idle.is_empty());
        for index in 0..260u16 {
            let socket = TcpStream::connect(addr).await.unwrap();
            peers.push(listener.accept().await.unwrap().0);
            let mut distinct = key.clone();
            distinct.port = index;
            pool.put(
                distinct,
                Connection {
                    stream: Stream::Plain(socket),
                    buffer: BytesMut::new(),
                },
            );
        }
        assert_eq!(pool.idle.values().map(Vec::len).sum::<usize>(), 256);
    }
}
