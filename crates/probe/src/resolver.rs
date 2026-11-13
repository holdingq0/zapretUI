// A resolver that whole DNS messages are passed to: over DoH (HTTP/2, one connection kept open)
// and, where it answers there, over plain DNS on port 53.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use h2::client::SendRequest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

use crate::{dns, net, tls};

const CONNECT: Duration = Duration::from_secs(3);
const ANSWER: Duration = Duration::from_secs(4);
const PLAIN: Duration = Duration::from_secs(2);
const IMPATIENT: Duration = Duration::from_millis(1200);
const MAX_ANSWER: usize = 65_535;

pub struct Resolver {
    /// Host, port and path of the DoH endpoint.
    doh: Option<(String, u16, String)>,
    /// The resolver's addresses; with none, the DoH host is looked up.
    ips: Vec<IpAddr>,
    plain: bool,
    session: Mutex<Option<SendRequest<Bytes>>>,
    /// DoH failed while port 53 answered: port 53 goes first until `retry_doh`.
    doh_down: AtomicBool,
}

impl Resolver {
    pub fn new(doh: Option<(String, u16, String)>, ips: Vec<IpAddr>, plain: bool) -> Self {
        Self { doh, ips, plain, session: Mutex::new(None), doh_down: AtomicBool::new(false) }
    }

    /// DoH gets another chance to be the first choice.
    pub fn retry_doh(&self) {
        self.doh_down.store(false, Ordering::Relaxed);
    }

    /// Sends a DNS message and returns the answer to it.
    pub async fn exchange(&self, query: &[u8]) -> Option<Vec<u8>> {
        if query.len() < 12 {
            return None;
        }
        let doh_first = self.doh.is_some() && !self.doh_down.load(Ordering::Relaxed);
        if doh_first {
            // With port 53 to fall back on, a DoH that does not connect is not waited for: the
            // program that asked gives up in a few seconds.
            let answer = match self.plain {
                true => tokio::time::timeout(IMPATIENT, self.doh(query)).await.ok().flatten(),
                false => self.doh(query).await,
            };
            if answer.is_some() {
                return answer;
            }
        }
        if self.plain {
            if let Some(answer) = self.plain(query).await {
                if doh_first {
                    self.doh_down.store(true, Ordering::Relaxed);
                }
                return Some(answer);
            }
        }
        if self.doh.is_some() && !doh_first {
            let answer = self.doh(query).await?;
            self.retry_doh();
            return Some(answer);
        }
        None
    }

    /// The IPv4 addresses the resolver gives for a name; empty when it knows no such name.
    pub async fn resolve(&self, host: &str) -> Option<Vec<IpAddr>> {
        dns::parse_a(&self.exchange(&dns::query(host)?).await?)
    }

    async fn doh(&self, query: &[u8]) -> Option<Vec<u8>> {
        let (host, port, path) = self.doh.as_ref()?;
        let uri = if *port == 443 { format!("https://{host}{path}") } else { format!("https://{host}:{port}{path}") };
        // A connection kept from before may have been closed by the server: one more try on a new one.
        for _ in 0..2 {
            let sender = self.session().await?;
            if let Ok(Some(answer)) = tokio::time::timeout(ANSWER, post(sender, &uri, query)).await {
                return Some(answer);
            }
            *self.session.lock().await = None;
        }
        None
    }

    /// The HTTP/2 connection to the resolver, opened when there is none.
    async fn session(&self) -> Option<SendRequest<Bytes>> {
        let (host, port, _) = self.doh.as_ref()?;
        let mut slot = self.session.lock().await;
        if let Some(sender) = slot.as_ref() {
            return Some(sender.clone());
        }
        let mut addrs = self.ips.clone();
        if addrs.is_empty() {
            addrs = dns::system_global(host).await;
        }
        if addrs.is_empty() {
            addrs = dns::doh_global(host, &[]).await;
        }
        for ip in addrs.into_iter().take(4) {
            let Ok(tcp) = net::connect(ip, *port, None, CONNECT).await else { continue };
            let Ok(stream) = tls::handshake_h2(tcp, host, CONNECT).await else { continue };
            let Ok(Ok((sender, connection))) = tokio::time::timeout(CONNECT, h2::client::handshake(stream)).await else { continue };
            tokio::spawn(async move {
                let _ = connection.await;
            });
            *slot = Some(sender.clone());
            return Some(sender);
        }
        None
    }

    async fn plain(&self, query: &[u8]) -> Option<Vec<u8>> {
        for ip in &self.ips {
            if let Ok(Some(answer)) = tokio::time::timeout(PLAIN, udp(*ip, query)).await {
                return Some(answer);
            }
        }
        None
    }
}

async fn post(sender: SendRequest<Bytes>, uri: &str, query: &[u8]) -> Option<Vec<u8>> {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/dns-message")
        .header("accept", "application/dns-message")
        .header("content-length", query.len().to_string())
        .body(())
        .ok()?;
    let mut sender = sender.ready().await.ok()?;
    let (response, mut body) = sender.send_request(request, false).ok()?;
    body.send_data(Bytes::copy_from_slice(query), true).ok()?;
    let response = response.await.ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let mut stream = response.into_body();
    let mut answer = Vec::new();
    while let Some(chunk) = stream.data().await {
        let chunk = chunk.ok()?;
        let _ = stream.flow_control().release_capacity(chunk.len());
        answer.extend_from_slice(&chunk);
        if answer.len() > MAX_ANSWER {
            return None;
        }
    }
    (answer.len() >= 12).then_some(answer)
}

async fn udp(ip: IpAddr, query: &[u8]) -> Option<Vec<u8>> {
    let local: SocketAddr = if ip.is_ipv4() { (Ipv4Addr::UNSPECIFIED, 0).into() } else { (Ipv6Addr::UNSPECIFIED, 0).into() };
    let socket = UdpSocket::bind(local).await.ok()?;
    socket.connect(SocketAddr::new(ip, 53)).await.ok()?;
    socket.send(query).await.ok()?;
    let mut buf = vec![0u8; 4096];
    loop {
        let n = socket.recv(&mut buf).await.ok()?;
        if n < 12 || buf[..2] != query[..2] {
            continue;
        }
        // Cut to fit a datagram: the whole answer comes over TCP.
        if buf[2] & 0x02 != 0 {
            return tcp(ip, query).await;
        }
        buf.truncate(n);
        return Some(buf);
    }
}

async fn tcp(ip: IpAddr, query: &[u8]) -> Option<Vec<u8>> {
    let mut stream = net::connect(ip, 53, None, CONNECT).await.ok()?;
    let len = u16::try_from(query.len()).ok()?;
    stream.write_all(&len.to_be_bytes()).await.ok()?;
    stream.write_all(query).await.ok()?;
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await.ok()?;
    let mut answer = vec![0u8; usize::from(u16::from_be_bytes(head))];
    stream.read_exact(&mut answer).await.ok()?;
    (answer.len() >= 12).then_some(answer)
}

/// The same message through the well-known public resolvers, whichever answers first.
pub async fn public(query: &[u8]) -> Option<Vec<u8>> {
    dns::public(query, |answer| answer.len() >= 12).await
}

/// Where the only question of a query ends.
fn question_end(query: &[u8]) -> Option<usize> {
    if query.len() < 12 || query[4..6] != [0, 1] {
        return None;
    }
    let end = dns::skip_name(query, 12)? + 4;
    (end <= query.len()).then_some(end)
}

/// The record type a query asks for.
pub fn qtype(query: &[u8]) -> Option<u16> {
    let end = question_end(query)?;
    Some(u16::from_be_bytes([query[end - 4], query[end - 3]]))
}

/// An answer with no records: the question back with this response code.
pub fn reply(query: &[u8], rcode: u8) -> Option<Vec<u8>> {
    let end = question_end(query)?;
    let mut out = query[..end].to_vec();
    out[2] = 0x80 | (query[2] & 0x01);
    out[3] = 0x80 | (rcode & 0x0f);
    out[6..12].fill(0);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_answer_keeps_the_question() {
        let mut query = dns::query("chatgpt.com").unwrap();
        query[0..2].copy_from_slice(&[0xab, 0xcd]);
        let asked = query.len();
        // An EDNS record after the question, the way Windows sends it.
        query[11] = 1;
        query.extend_from_slice(&[0, 0, 41, 0x04, 0xd0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(qtype(&query), Some(1));
        let answer = reply(&query, 2).unwrap();
        assert_eq!(answer.len(), asked);
        assert_eq!(answer[..4], [0xab, 0xcd, 0x81, 0x82]);
        assert_eq!(answer[4..12], [0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(reply(&query[..14], 0), None);
        assert_eq!(qtype(&[0; 5]), None);
    }
}
