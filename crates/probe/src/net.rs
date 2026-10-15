use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::net::{TcpSocket, TcpStream};

#[derive(Debug)]
pub struct PortBlock {
    lo: u16,
    hi: u16,
    next: AtomicU32,
}

impl PortBlock {
    pub fn new(lo: u16, hi: u16) -> Self {
        Self { lo, hi: hi.max(lo), next: AtomicU32::new(0) }
    }

    pub fn lo(&self) -> u16 {
        self.lo
    }

    pub fn hi(&self) -> u16 {
        self.hi
    }

    fn take(&self) -> u16 {
        let span = u32::from(self.hi - self.lo) + 1;
        self.lo + (self.next.fetch_add(1, Ordering::Relaxed) % span) as u16
    }
}

pub fn timed_out(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, what.to_owned())
}

pub async fn connect(ip: IpAddr, port: u16, ports: Option<&PortBlock>, limit: Duration) -> io::Result<TcpStream> {
    let attempt = async {
        let tries = if ports.is_some() { 16 } else { 1 };
        let mut last = None;
        for _ in 0..tries {
            let socket = if ip.is_ipv4() { TcpSocket::new_v4()? } else { TcpSocket::new_v6()? };
            if let Some(block) = ports {
                let any: IpAddr = if ip.is_ipv4() { Ipv4Addr::UNSPECIFIED.into() } else { Ipv6Addr::UNSPECIFIED.into() };
                if let Err(e) = socket.bind(SocketAddr::new(any, block.take())) {
                    last = Some(e);
                    continue;
                }
            }
            return socket.connect(SocketAddr::new(ip, port)).await;
        }
        Err(last.unwrap_or_else(|| io::Error::other("нет свободного локального порта")))
    };
    let stream = tokio::time::timeout(limit, attempt).await.map_err(|_| timed_out("нет ответа"))??;
    stream.set_nodelay(true)?;
    Ok(stream)
}

pub fn describe(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::TimedOut => "нет ответа".into(),
        io::ErrorKind::ConnectionReset => "соединение сброшено".into(),
        io::ErrorKind::ConnectionRefused => "соединение отклонено".into(),
        io::ErrorKind::ConnectionAborted => "соединение прервано".into(),
        io::ErrorKind::UnexpectedEof => "соединение закрыто".into(),
        io::ErrorKind::InvalidData => format!("ошибка TLS: {e}"),
        _ => e.to_string(),
    }
}
