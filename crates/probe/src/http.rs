use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::Target;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Complete,
    Stall,
    Broken(io::ErrorKind),
}

#[derive(Debug, Clone)]
pub struct Fetch {
    pub status: Option<u16>,
    pub location: Option<String>,
    pub bytes: usize,
    pub end: End,
}

struct Head {
    status: u16,
    location: Option<String>,
    length: Option<usize>,
    chunked: bool,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_head(raw: &[u8]) -> Option<Head> {
    let text = String::from_utf8_lossy(raw);
    let mut lines = text.split("\r\n");
    let status = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let mut head = Head { status, location: None, length: None, chunked: false };
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "location" => head.location = Some(value.to_owned()),
            "content-length" => head.length = value.parse().ok(),
            "transfer-encoding" => head.chunked = value.to_ascii_lowercase().contains("chunked"),
            _ => {}
        }
    }
    Some(head)
}

pub async fn get<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, target: &Target, want: usize, idle: Duration) -> Fetch {
    let mut fetch = Fetch { status: None, location: None, bytes: 0, end: End::Complete };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {UA}\r\nAccept: text/html,application/xhtml+xml,*/*;q=0.8\r\nAccept-Language: ru,en;q=0.8\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
        target.path, target.host
    );
    match tokio::time::timeout(idle, stream.write_all(request.as_bytes())).await {
        Err(_) => {
            fetch.end = End::Stall;
            return fetch;
        }
        Ok(Err(e)) => {
            fetch.end = End::Broken(e.kind());
            return fetch;
        }
        Ok(Ok(())) => {}
    }

    let mut raw = Vec::new();
    let mut head: Option<Head> = None;
    let mut body = 0usize;
    let mut tail: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match tokio::time::timeout(idle, stream.read(&mut buf)).await {
            Err(_) => {
                fetch.end = End::Stall;
                break;
            }
            Ok(Err(e)) => {
                let finished = e.kind() == io::ErrorKind::UnexpectedEof
                    && head.as_ref().is_some_and(|h| h.length.is_none_or(|len| body >= len));
                fetch.end = if finished { End::Complete } else { End::Broken(e.kind()) };
                break;
            }
            Ok(Ok(0)) => {
                let short = head.as_ref().and_then(|h| h.length).is_some_and(|len| body < len);
                fetch.end = if short { End::Broken(io::ErrorKind::UnexpectedEof) } else { End::Complete };
                break;
            }
            Ok(Ok(n)) => n,
        };
        fetch.bytes += n;
        let chunk = &buf[..n];
        match &head {
            None => {
                raw.extend_from_slice(chunk);
                if let Some(end) = find(&raw, b"\r\n\r\n") {
                    let Some(parsed) = parse_head(&raw[..end]) else {
                        fetch.end = End::Broken(io::ErrorKind::InvalidData);
                        break;
                    };
                    fetch.status = Some(parsed.status);
                    fetch.location = parsed.location.clone();
                    body = raw.len() - end - 4;
                    tail = raw[end + 4..].to_vec();
                    head = Some(parsed);
                } else if raw.len() > 64 * 1024 {
                    fetch.end = End::Broken(io::ErrorKind::InvalidData);
                    break;
                }
            }
            Some(_) => {
                body += n;
                tail.extend_from_slice(chunk);
            }
        }
        if tail.len() > 16 {
            tail.drain(..tail.len() - 16);
        }
        if fetch.bytes >= want {
            break;
        }
        if let Some(h) = &head {
            if h.length.is_some_and(|len| body >= len) || (h.chunked && tail.ends_with(b"0\r\n\r\n")) {
                break;
            }
        }
    }
    fetch
}

pub fn body(raw: &[u8]) -> Option<(u16, Vec<u8>)> {
    let end = find(raw, b"\r\n\r\n")?;
    let head = parse_head(&raw[..end])?;
    let rest = &raw[end + 4..];
    if !head.chunked {
        let len = head.length.unwrap_or(rest.len()).min(rest.len());
        return Some((head.status, rest[..len].to_vec()));
    }
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let line_end = pos + find(&rest[pos..], b"\r\n")?;
        let size_text = std::str::from_utf8(&rest[pos..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        pos = line_end + 2;
        if size == 0 {
            return Some((head.status, out));
        }
        out.extend_from_slice(rest.get(pos..pos + size)?);
        pos += size + 2;
    }
}
