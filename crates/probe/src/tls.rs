use std::io;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, NamedGroup, RootCertStore};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::net::timed_out;

/// The size of the ClientHello, which DPI is sensitive to. Browsers send a post-quantum key share
/// that pushes the hello over one packet; most other programs still send a small one. A DPI that
/// does not put the two packets together lets the big hello through and blocks the small one, and
/// a bypass strategy that splits one of them may break the other. A probe that tries only one
/// size sees half of the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hello {
    /// Classic key shares only: a few hundred bytes, one packet.
    #[default]
    Small,
    /// With X25519MLKEM768, like Chrome and Firefox: two packets.
    Big,
}

static CONFIGS: OnceLock<[Option<Arc<ClientConfig>>; 2]> = OnceLock::new();

fn build(hello: Hello) -> Option<Arc<ClientConfig>> {
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    if hello == Hello::Small {
        provider.kx_groups.retain(|g| matches!(g.name(), NamedGroup::X25519 | NamedGroup::secp256r1 | NamedGroup::secp384r1));
    }
    let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let mut config = ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .ok()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Some(Arc::new(config))
}

fn config(hello: Hello) -> io::Result<Arc<ClientConfig>> {
    let configs = CONFIGS.get_or_init(|| [build(Hello::Small), build(Hello::Big)]);
    configs[hello as usize].clone().ok_or_else(|| io::Error::other("TLS не инициализирован"))
}

pub fn bad_certificate(e: &io::Error) -> bool {
    e.get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .is_some_and(|e| matches!(e, rustls::Error::InvalidCertificate(_)))
}

pub async fn handshake(tcp: TcpStream, host: &str, limit: Duration, hello: Hello) -> io::Result<TlsStream<TcpStream>> {
    let name = ServerName::try_from(host.to_owned()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let connector = TlsConnector::from(config(hello)?);
    tokio::time::timeout(limit, connector.connect(name, tcp))
        .await
        .map_err(|_| timed_out("нет ответа на ClientHello"))?
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    use super::*;

    /// The bytes of the ClientHello as a server gets them.
    async fn hello_len(hello: Hello) -> usize {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let _ = handshake(tcp, "www.example.com", Duration::from_millis(700), hello).await;
        });
        let (mut server, _) = listener.accept().await.unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(300), server.read(&mut buf)).await {
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        let _ = client.await;
        got.len()
    }

    #[tokio::test]
    async fn the_two_hellos_differ_the_way_browsers_and_other_programs_do() {
        let (small, big) = (hello_len(Hello::Small).await, hello_len(Hello::Big).await);
        assert!((100..900).contains(&small), "small hello: {small} bytes");
        // More than fits in one packet of 1460 bytes.
        assert!(big > 1460, "big hello: {big} bytes");
    }
}
