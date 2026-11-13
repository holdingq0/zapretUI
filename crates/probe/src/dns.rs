use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinSet;

use crate::{http, net, tls};

const DOH: [(&str, [u8; 4]); 4] = [
    ("cloudflare-dns.com", [1, 1, 1, 1]),
    ("dns.google", [8, 8, 8, 8]),
    ("dns.quad9.net", [9, 9, 9, 9]),
    ("common.dot.dns.yandex.net", [77, 88, 8, 8]),
];

pub fn is_global(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
                || a >= 240)
        }
        IpAddr::V6(v6) => !(v6.is_unspecified() || v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00),
    }
}

pub async fn system_global(host: &str) -> Vec<IpAddr> {
    system(host).await.into_iter().filter(is_global).collect()
}

pub async fn doh_global(host: &str, system: &[IpAddr]) -> Vec<IpAddr> {
    doh(host)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|ip| is_global(ip) && !system.contains(ip))
        .collect()
}

async fn system(host: &str) -> Vec<IpAddr> {
    let lookup = tokio::net::lookup_host((host, 443));
    match tokio::time::timeout(Duration::from_secs(4), lookup).await {
        Ok(Ok(addrs)) => {
            let mut ips: Vec<IpAddr> = Vec::new();
            for addr in addrs {
                if addr.is_ipv4() && !ips.contains(&addr.ip()) {
                    ips.push(addr.ip());
                }
            }
            ips
        }
        _ => Vec::new(),
    }
}

async fn doh(host: &str) -> Option<Vec<IpAddr>> {
    let answer = public(&query(host)?, |answer| parse_a(answer).is_some()).await?;
    parse_a(&answer)
}

/// Asks the public resolvers all at once; the first answer that `accept` takes wins.
pub(crate) async fn public(body: &[u8], accept: fn(&[u8]) -> bool) -> Option<Vec<u8>> {
    let mut set = JoinSet::new();
    for (name, ip) in DOH {
        let body = body.to_vec();
        set.spawn(async move { doh_one(name, Ipv4Addr::from(ip), &body).await });
    }
    while let Some(result) = set.join_next().await {
        if let Ok(Some(answer)) = result {
            if accept(&answer) {
                set.abort_all();
                return Some(answer);
            }
        }
    }
    None
}

async fn doh_one(name: &str, server: Ipv4Addr, body: &[u8]) -> Option<Vec<u8>> {
    let exchange = async {
        let tcp = net::connect(IpAddr::V4(server), 443, None, Duration::from_secs(3)).await.ok()?;
        let mut stream = tls::handshake(tcp, name, Duration::from_secs(3), tls::Hello::Small).await.ok()?;
        let head = format!(
            "POST /dns-query HTTP/1.1\r\nHost: {name}\r\nContent-Type: application/dns-message\r\nAccept: application/dns-message\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.ok()?;
        stream.write_all(body).await.ok()?;
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw).await;
        let (status, answer) = http::body(&raw)?;
        (status == 200).then_some(answer)
    };
    tokio::time::timeout(Duration::from_secs(5), exchange).await.ok().flatten()
}

pub(crate) fn query(host: &str) -> Option<Vec<u8>> {
    let mut q = vec![0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]);
    Some(q)
}

pub(crate) fn skip_name(buf: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *buf.get(pos)? as usize;
        if len == 0 {
            return Some(pos + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Some(pos + 2);
        }
        pos += 1 + len;
    }
}

fn u16_at(buf: &[u8], pos: usize) -> Option<usize> {
    Some(u16::from_be_bytes([*buf.get(pos)?, *buf.get(pos + 1)?]) as usize)
}

pub(crate) fn parse_a(buf: &[u8]) -> Option<Vec<IpAddr>> {
    let rcode = buf.get(3)? & 0x0f;
    if rcode == 3 {
        return Some(Vec::new());
    }
    if rcode != 0 {
        return None;
    }
    let questions = u16_at(buf, 4)?;
    let answers = u16_at(buf, 6)?;
    let mut pos = 12;
    for _ in 0..questions {
        pos = skip_name(buf, pos)? + 4;
    }
    let mut ips = Vec::new();
    for _ in 0..answers {
        pos = skip_name(buf, pos)?;
        let kind = u16_at(buf, pos)?;
        let len = u16_at(buf, pos + 8)?;
        let data = buf.get(pos + 10..pos + 10 + len)?;
        if kind == 1 && len == 4 {
            ips.push(IpAddr::V4(Ipv4Addr::new(data[0], data[1], data[2], data[3])));
        }
        pos += 10 + len;
    }
    Some(ips)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_an_a_query() {
        let parts: [&[u8]; 3] = [&[0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0], b"\x02ya\x02ru", &[0, 0, 1, 0, 1]];
        let expected = parts.concat();
        assert_eq!(query("ya.ru"), Some(expected.clone()));
        assert_eq!(query("ya.ru."), Some(expected));
        assert_eq!(query("a..b"), None);
    }

    fn answer(rcode: u8) -> Vec<u8> {
        let mut buf = vec![0, 0, 0x81, 0x80 | rcode, 0, 1, 0, 2, 0, 0, 0, 0];
        buf.extend_from_slice(b"\x02ya\x02ru\x00\x00\x01\x00\x01");
        // A CNAME first, the way big services answer, then the address.
        buf.extend_from_slice(&[0xc0, 0x0c, 0, 5, 0, 1, 0, 0, 0, 1, 0, 2, 0xc0, 0x0c]);
        buf.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 1, 2, 3, 4]);
        buf
    }

    #[test]
    fn reads_addresses_past_a_cname() {
        assert_eq!(parse_a(&answer(0)), Some(vec![IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))]));
    }

    #[test]
    fn tells_no_such_name_from_a_failed_server() {
        assert_eq!(parse_a(&answer(3)), Some(Vec::new()));
        assert_eq!(parse_a(&answer(2)), None);
        let whole = answer(0);
        assert_eq!(parse_a(&whole[..whole.len() - 2]), None, "cut short");
    }

    #[test]
    fn private_and_reserved_addresses_are_not_global() {
        let global = |s: &str| is_global(&s.parse().unwrap());
        assert!(global("8.8.8.8"));
        assert!(global("2606:4700::1111"));
        for local in ["192.168.1.1", "10.0.0.1", "100.64.0.1", "127.0.0.1", "169.254.1.1", "0.0.0.0", "240.0.0.1", "::1", "fd00::1"] {
            assert!(!global(local), "{local}");
        }
    }
}
