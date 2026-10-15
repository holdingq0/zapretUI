use std::io;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::net::timed_out;

static CONFIG: OnceLock<Option<Arc<ClientConfig>>> = OnceLock::new();

fn config() -> io::Result<Arc<ClientConfig>> {
    CONFIG
        .get_or_init(|| {
            let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            let mut config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .ok()?
                .with_root_certificates(roots)
                .with_no_client_auth();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            Some(Arc::new(config))
        })
        .clone()
        .ok_or_else(|| io::Error::other("TLS не инициализирован"))
}

pub async fn handshake(tcp: TcpStream, host: &str, limit: Duration) -> io::Result<TlsStream<TcpStream>> {
    let name = ServerName::try_from(host.to_owned()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let connector = TlsConnector::from(config()?);
    tokio::time::timeout(limit, connector.connect(name, tcp))
        .await
        .map_err(|_| timed_out("нет ответа на ClientHello"))?
}
