use std::{fmt, sync::Arc};

use async_trait::async_trait;
use futures_util::TryStreamExt;
use rustls::{
    ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use secrecy::ExposeSecret;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};

use crate::{
    config::ImapConfig,
    error::Result,
    mail_source::{MailSource, MailSourceCapabilities, MailSourceKind, MessageRef, RawEmail},
};

type ImapStream = Compat<TlsStream<TcpStream>>;
type ImapSession = async_imap::Session<ImapStream>;

#[derive(Debug, Clone)]
pub struct ImapSource {
    pub config: ImapConfig,
}

impl ImapSource {
    #[must_use]
    pub const fn new(config: ImapConfig) -> Self {
        Self { config }
    }

    async fn connect_session(&self) -> std::result::Result<ImapSession, ImapSourceError> {
        let tcp_stream = TcpStream::connect((self.config.host.as_str(), self.config.port))
            .await
            .map_err(ImapSourceError::backend)?;
        let server_name = ServerName::try_from(self.config.host.clone()).map_err(|_| {
            ImapSourceError::InvalidServerName {
                host: self.config.host.clone(),
            }
        })?;
        let tls_config = tls_config(self.config.accept_invalid_certs);
        let tls_stream = TlsConnector::from(Arc::new(tls_config))
            .connect(server_name, tcp_stream)
            .await
            .map_err(ImapSourceError::backend)?;
        let mut client = async_imap::Client::new(tls_stream.compat());
        let greeting = client
            .read_response()
            .await
            .map_err(ImapSourceError::backend)?;

        if greeting.is_none() {
            return Err(ImapSourceError::MissingGreeting);
        }

        client
            .login(
                self.config.username.as_str(),
                self.config.password.expose_secret(),
            )
            .await
            .map_err(|(error, _client)| ImapSourceError::Backend(error.to_string()))
    }

    fn source_id(&self) -> String {
        format!("{}:{}", self.config.host, self.config.port)
    }

    fn message_ref(&self, uid: u32) -> MessageRef {
        MessageRef::new(
            MailSourceKind::Imap,
            self.source_id(),
            Some(self.config.mailbox.clone()),
            uid.to_string(),
        )
    }

    fn parse_uid(&self, message: &MessageRef) -> std::result::Result<u32, ImapSourceError> {
        if message.source != MailSourceKind::Imap {
            return Err(ImapSourceError::InvalidMessageRef {
                reason: "message ref source is not IMAP",
            });
        }

        message
            .stable_id
            .parse::<u32>()
            .map_err(|_| ImapSourceError::InvalidUid {
                stable_id: message.stable_id.clone(),
            })
    }
}

#[async_trait]
impl MailSource for ImapSource {
    async fn list_unread(&self) -> Result<Vec<MessageRef>> {
        let mut session = self.connect_session().await?;
        session
            .select(&self.config.mailbox)
            .await
            .map_err(ImapSourceError::backend)?;
        let mut uids = session
            .uid_search("UNSEEN")
            .await
            .map_err(ImapSourceError::backend)?
            .into_iter()
            .collect::<Vec<_>>();
        uids.sort_unstable();
        session.logout().await.map_err(ImapSourceError::backend)?;

        Ok(uids.into_iter().map(|uid| self.message_ref(uid)).collect())
    }

    async fn fetch(&self, message: &MessageRef) -> Result<RawEmail> {
        let uid = self.parse_uid(message)?;
        let mut session = self.connect_session().await?;
        session
            .select(&self.config.mailbox)
            .await
            .map_err(ImapSourceError::backend)?;

        let body = {
            let mut fetches = session
                .uid_fetch(uid.to_string(), "RFC822")
                .await
                .map_err(ImapSourceError::backend)?;
            let mut body = None;

            while let Some(fetch) = fetches.try_next().await.map_err(ImapSourceError::backend)? {
                if let Some(bytes) = fetch.body() {
                    body = Some(bytes.to_vec());
                    break;
                }
            }

            body
        };

        session.logout().await.map_err(ImapSourceError::backend)?;

        Ok(RawEmail::new(
            body.ok_or(ImapSourceError::MissingBody { uid })?,
            message.clone(),
        ))
    }

    async fn mark_read(&self, message: &MessageRef) -> Result<()> {
        let uid = self.parse_uid(message)?;
        let mut session = self.connect_session().await?;
        session
            .select(&self.config.mailbox)
            .await
            .map_err(ImapSourceError::backend)?;

        {
            let mut updates = session
                .uid_store(uid.to_string(), "+FLAGS (\\Seen)")
                .await
                .map_err(ImapSourceError::backend)?;

            while updates
                .try_next()
                .await
                .map_err(ImapSourceError::backend)?
                .is_some()
            {}
        }

        session.logout().await.map_err(ImapSourceError::backend)?;

        Ok(())
    }

    fn capabilities(&self) -> MailSourceCapabilities {
        MailSourceCapabilities::imap()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImapSourceError {
    #[error("invalid IMAP TLS server name for host {host:?}")]
    InvalidServerName { host: String },

    #[error("IMAP server closed connection before greeting")]
    MissingGreeting,

    #[error("invalid IMAP message ref: {reason}")]
    InvalidMessageRef { reason: &'static str },

    #[error("invalid IMAP UID in message ref: {stable_id:?}")]
    InvalidUid { stable_id: String },

    #[error("IMAP fetch for UID {uid} did not include RFC822 body")]
    MissingBody { uid: u32 },

    #[error("IMAP backend error: {0}")]
    Backend(String),
}

impl ImapSourceError {
    fn backend(error: impl fmt::Display) -> Self {
        Self::Backend(error.to_string())
    }
}

fn tls_config(accept_invalid_certs: bool) -> ClientConfig {
    if accept_invalid_certs {
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptInvalidCertVerifier))
            .with_no_client_auth()
    } else {
        let mut root_store = RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    }
}

#[derive(Debug)]
struct AcceptInvalidCertVerifier;

impl ServerCertVerifier for AcceptInvalidCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::*;

    fn source() -> ImapSource {
        ImapSource::new(ImapConfig {
            host: "imap.example.com".to_owned(),
            port: 993,
            username: "user@example.com".to_owned(),
            password: SecretString::from("secret".to_owned()),
            use_tls: true,
            accept_invalid_certs: false,
            mailbox: "INBOX".to_owned(),
            poll_interval: std::time::Duration::from_secs(60),
            force_reconnect: std::time::Duration::from_secs(60),
            mark_seen: false,
        })
    }

    #[test]
    fn builds_provider_neutral_refs_from_uid() {
        let source = source();

        let message_ref = source.message_ref(42);

        assert_eq!(message_ref.source, MailSourceKind::Imap);
        assert_eq!(message_ref.source_id, "imap.example.com:993");
        assert_eq!(message_ref.mailbox.as_deref(), Some("INBOX"));
        assert_eq!(message_ref.stable_id, "42");
    }

    #[test]
    fn rejects_non_imap_refs() {
        let source = source();
        let message_ref = MessageRef::new(MailSourceKind::ProtonCustom, "proton", None, "42");

        assert_eq!(
            source.parse_uid(&message_ref),
            Err(ImapSourceError::InvalidMessageRef {
                reason: "message ref source is not IMAP"
            })
        );
    }

    #[test]
    fn rejects_invalid_uid() {
        let source = source();
        let message_ref = MessageRef::new(MailSourceKind::Imap, "imap.example.com:993", None, "x");

        assert_eq!(
            source.parse_uid(&message_ref),
            Err(ImapSourceError::InvalidUid {
                stable_id: "x".to_owned()
            })
        );
    }
}
