#[cfg(feature = "std")]
use crate::error::Error;
#[cfg(any(feature = "std", feature = "hashbrown"))]
use crate::hash_map::HashMap;
#[cfg(feature = "std")]
use crate::limited_cache;
#[cfg(any(feature = "std", feature = "hashbrown"))]
use crate::lock::Mutex;
use crate::msgs::handshake::CertificateChain;
use crate::server;
use crate::server::ClientHello;
use crate::sign;
#[cfg(feature = "std")]
use crate::webpki::{verify_server_name, ParsedCertificate};

#[cfg(feature = "std")]
use pki_types::{DnsName, ServerName};

#[cfg(feature = "std")]
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Debug;
#[cfg(feature = "std")]
use core::fmt::Formatter;

/// Something which never stores sessions.
#[derive(Debug)]
pub struct NoServerSessionStorage {}

impl server::StoresServerSessions for NoServerSessionStorage {
    fn put(&self, _id: Vec<u8>, _sec: Vec<u8>) -> bool {
        false
    }
    fn get(&self, _id: &[u8]) -> Option<Vec<u8>> {
        None
    }
    fn take(&self, _id: &[u8]) -> Option<Vec<u8>> {
        None
    }
    fn can_cache(&self) -> bool {
        false
    }
}

/// An implementer of `StoresServerSessions` that stores everything
/// in memory.  If enforces a limit on the number of stored sessions
/// to bound memory usage.
#[cfg(feature = "std")]
pub struct ServerSessionMemoryCache {
    cache: Mutex<limited_cache::LimitedCache<Vec<u8>, Vec<u8>>>,
}

#[cfg(feature = "std")]
impl ServerSessionMemoryCache {
    /// Make a new ServerSessionMemoryCache.  `size` is the maximum
    /// number of stored sessions, and may be rounded-up for
    /// efficiency.
    pub fn new(size: usize) -> Arc<Self> {
        Arc::new(Self {
            cache: Mutex::new(limited_cache::LimitedCache::new(size)),
        })
    }
}

#[cfg(feature = "std")]
impl server::StoresServerSessions for ServerSessionMemoryCache {
    fn put(&self, key: Vec<u8>, value: Vec<u8>) -> bool {
        self.cache
            .lock()
            .unwrap()
            .insert(key, value);
        true
    }

    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.cache
            .lock()
            .unwrap()
            .get(key)
            .cloned()
    }

    fn take(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.cache.lock().unwrap().remove(key)
    }

    fn can_cache(&self) -> bool {
        true
    }
}

#[cfg(feature = "std")]
impl Debug for ServerSessionMemoryCache {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServerSessionMemoryCache")
            .finish()
    }
}

/// Something which never produces tickets.
#[derive(Debug)]
pub(super) struct NeverProducesTickets {}

impl server::ProducesTickets for NeverProducesTickets {
    fn enabled(&self) -> bool {
        false
    }
    fn lifetime(&self) -> u32 {
        0
    }
    fn encrypt(&self, _bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }
    fn decrypt(&self, _bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

/// Something which always resolves to the same cert chain.
#[derive(Debug)]
pub(super) struct AlwaysResolvesChain(Arc<sign::CertifiedKey>);

impl AlwaysResolvesChain {
    /// Creates an `AlwaysResolvesChain`, using the supplied key and certificate chain.
    pub(super) fn new(private_key: Arc<dyn sign::SigningKey>, chain: CertificateChain) -> Self {
        Self(Arc::new(sign::CertifiedKey::new(chain.0, private_key)))
    }

    /// Creates an `AlwaysResolvesChain`, using the supplied key, certificate chain and OCSP response.
    ///
    /// If non-empty, the given OCSP response is attached.
    pub(super) fn new_with_extras(
        private_key: Arc<dyn sign::SigningKey>,
        chain: CertificateChain,
        ocsp: Vec<u8>,
    ) -> Self {
        let mut r = Self::new(private_key, chain);

        {
            let cert = Arc::make_mut(&mut r.0);
            if !ocsp.is_empty() {
                cert.ocsp = Some(ocsp);
            }
        }

        r
    }
}

impl server::ResolvesServerCert for AlwaysResolvesChain {
    fn resolve(&self, _client_hello: ClientHello) -> Option<Arc<sign::CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }
}

/// Something that resolves do different cert chains/keys based
/// on client-supplied server name (via SNI).
#[cfg(feature = "std")]
#[derive(Debug)]
pub struct ResolvesServerCertUsingSni {
    by_name: HashMap<String, Arc<sign::CertifiedKey>>,
}

#[cfg(feature = "std")]
impl ResolvesServerCertUsingSni {
    /// Create a new and empty (i.e., knows no certificates) resolver.
    pub fn new() -> Self {
        Self {
            by_name: HashMap::new(),
        }
    }

    /// Add a new `sign::CertifiedKey` to be used for the given SNI `name`.
    ///
    /// This function fails if `name` is not a valid DNS name, or if
    /// it's not valid for the supplied certificate, or if the certificate
    /// chain is syntactically faulty.
    pub fn add(&mut self, name: &str, ck: sign::CertifiedKey) -> Result<(), Error> {
        let server_name = {
            let checked_name = DnsName::try_from(name)
                .map_err(|_| Error::General("Bad DNS name".into()))
                .map(|name| name.to_lowercase_owned())?;
            ServerName::DnsName(checked_name)
        };

        // Check the certificate chain for validity:
        // - it should be non-empty list
        // - the first certificate should be parsable as a x509v3,
        // - the first certificate should quote the given server name
        //   (if provided)
        //
        // These checks are not security-sensitive.  They are the
        // *server* attempting to detect accidental misconfiguration.

        ck.end_entity_cert()
            .and_then(ParsedCertificate::try_from)
            .and_then(|cert| verify_server_name(&cert, &server_name))?;

        if let ServerName::DnsName(name) = server_name {
            self.by_name
                .insert(name.as_ref().to_string(), Arc::new(ck));
        }
        Ok(())
    }
}

#[cfg(feature = "std")]
impl server::ResolvesServerCert for ResolvesServerCertUsingSni {
    fn resolve(&self, client_hello: ClientHello) -> Option<Arc<sign::CertifiedKey>> {
        if let Some(name) = client_hello.server_name() {
            self.by_name.get(name).map(Arc::clone)
        } else {
            // This kind of resolver requires SNI
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::ProducesTickets;
    #[cfg(feature = "std")]
    use crate::server::ResolvesServerCert;
    use crate::server::StoresServerSessions;

    #[test]
    fn test_noserversessionstorage_drops_put() {
        let c = NoServerSessionStorage {};
        assert!(!c.put(vec![0x01], vec![0x02]));
    }

    #[test]
    fn test_noserversessionstorage_denies_gets() {
        let c = NoServerSessionStorage {};
        c.put(vec![0x01], vec![0x02]);
        assert_eq!(c.get(&[]), None);
        assert_eq!(c.get(&[0x01]), None);
        assert_eq!(c.get(&[0x02]), None);
    }

    #[test]
    fn test_noserversessionstorage_denies_takes() {
        let c = NoServerSessionStorage {};
        assert_eq!(c.take(&[]), None);
        assert_eq!(c.take(&[0x01]), None);
        assert_eq!(c.take(&[0x02]), None);
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_serversessionmemorycache_accepts_put() {
        let c = ServerSessionMemoryCache::new(4);
        assert!(c.put(vec![0x01], vec![0x02]));
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_serversessionmemorycache_persists_put() {
        let c = ServerSessionMemoryCache::new(4);
        assert!(c.put(vec![0x01], vec![0x02]));
        assert_eq!(c.get(&[0x01]), Some(vec![0x02]));
        assert_eq!(c.get(&[0x01]), Some(vec![0x02]));
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_serversessionmemorycache_overwrites_put() {
        let c = ServerSessionMemoryCache::new(4);
        assert!(c.put(vec![0x01], vec![0x02]));
        assert!(c.put(vec![0x01], vec![0x04]));
        assert_eq!(c.get(&[0x01]), Some(vec![0x04]));
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_serversessionmemorycache_drops_to_maintain_size_invariant() {
        let c = ServerSessionMemoryCache::new(2);
        assert!(c.put(vec![0x01], vec![0x02]));
        assert!(c.put(vec![0x03], vec![0x04]));
        assert!(c.put(vec![0x05], vec![0x06]));
        assert!(c.put(vec![0x07], vec![0x08]));
        assert!(c.put(vec![0x09], vec![0x0a]));

        let count = c.get(&[0x01]).iter().count()
            + c.get(&[0x03]).iter().count()
            + c.get(&[0x05]).iter().count()
            + c.get(&[0x07]).iter().count()
            + c.get(&[0x09]).iter().count();

        assert!(count < 5);
    }

    #[test]
    fn test_neverproducestickets_does_nothing() {
        let npt = NeverProducesTickets {};
        assert!(!npt.enabled());
        assert_eq!(0, npt.lifetime());
        assert_eq!(None, npt.encrypt(&[]));
        assert_eq!(None, npt.decrypt(&[]));
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_resolvesservercertusingsni_requires_sni() {
        let rscsni = ResolvesServerCertUsingSni::new();
        assert!(rscsni
            .resolve(ClientHello::new(&None, &[], None, &[]))
            .is_none());
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_resolvesservercertusingsni_handles_unknown_name() {
        let rscsni = ResolvesServerCertUsingSni::new();
        let name = DnsName::try_from("hello.com")
            .unwrap()
            .to_owned();
        assert!(rscsni
            .resolve(ClientHello::new(&Some(name), &[], None, &[]))
            .is_none());
    }
}
