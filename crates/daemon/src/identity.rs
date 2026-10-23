use anyhow::{Context, Result};
use rcgen::{CertificateParams, KeyPair};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: String,
}

pub fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn load_or_create(dir: &Path) -> Result<Identity> {
    let cert_path = dir.join("cert.der");
    let key_path = dir.join("key.der");
    if cert_path.exists() && key_path.exists() {
        let cert_der = fs::read(&cert_path)?;
        let key_der = fs::read(&key_path)?;
        return Ok(Identity {
            fingerprint: fingerprint(&cert_der),
            cert_der,
            key_der,
        });
    }
    let key = KeyPair::generate()?;
    let params = CertificateParams::new(vec!["multicontrol".into()])?;
    let cert = params.self_signed(&key)?;
    let cert_der = cert.der().to_vec();
    let key_der = key.serialize_der();
    fs::write(&cert_path, &cert_der)?;
    fs::write(&key_path, &key_der)?;
    Ok(Identity {
        fingerprint: fingerprint(&cert_der),
        cert_der,
        key_der,
    })
}

pub fn fingerprint(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

pub struct Gate {
    pub pins: Arc<RwLock<std::collections::HashSet<String>>>,
    pub require_pin: bool,
    pub seen: Arc<Mutex<Option<String>>>,
    provider: Arc<CryptoProvider>,
}

impl Gate {
    pub fn new(pins: Arc<RwLock<std::collections::HashSet<String>>>, require_pin: bool) -> Arc<Self> {
        Arc::new(Self {
            pins,
            require_pin,
            seen: Arc::new(Mutex::new(None)),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        })
    }

    fn accept(&self, der: &[u8]) -> Result<(), Error> {
        let fp = fingerprint(der);
        let allowed = !self.require_pin || self.pins.read().expect("pins").contains(&fp);
        if allowed {
            *self.seen.lock().expect("seen") = Some(fp);
            Ok(())
        } else {
            Err(Error::General("peer is not paired".into()))
        }
    }
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gate")
    }
}

impl ServerCertVerifier for Gate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.accept(end_entity.as_ref())?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ClientCertVerifier for Gate {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        self.accept(end_entity.as_ref())?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }
}

pub fn server_config(identity: &Identity, gate: Arc<Gate>) -> Result<ServerConfig> {
    let cert = CertificateDer::from(identity.cert_der.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der.clone()));
    ServerConfig::builder()
        .with_client_cert_verifier(gate)
        .with_single_cert(vec![cert], key)
        .context("server config")
}

pub fn client_config(identity: &Identity, gate: Arc<Gate>) -> Result<ClientConfig> {
    let cert = CertificateDer::from(identity.cert_der.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der.clone()));
    ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(gate)
        .with_client_auth_cert(vec![cert], key)
        .context("client config")
}
