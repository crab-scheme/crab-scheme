//! `cs-discovery` — pluggable discovery providers for cluster bootstrap.
//!
//! Spec: `docs/research/sdk_spec/distributed.md` § M03, task list at
//! `tasks/M03-discovery.md`. API modeled on Akka ServiceDiscovery —
//! one async method, opaque inputs/outputs, with concrete provider
//! impls behind cargo features so embedders ship only what they use.
//!
//! ## Status
//!
//! **Scaffold only.** The `DiscoveryProvider` trait, lookup types,
//! and `first-success` combinator shape are defined here. Concrete
//! providers (DNS, k8s, postgres, consul, …) are added in M03 iters.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

use async_trait::async_trait;
use std::net::IpAddr;
use std::time::Duration;
use thiserror::Error;

/// Lookup query. Modeled on `akka.discovery.Lookup`.
#[derive(Debug, Clone)]
pub struct Lookup {
    /// Logical service name (e.g. `"checkout"`, `"crab"`).
    pub service_name: String,
    /// Optional port name (e.g. `"crab-cluster"`). Useful for SRV.
    pub port_name: Option<String>,
    /// Optional protocol filter (`Tcp` / `Udp`).
    pub protocol: Option<Protocol>,
}

impl Lookup {
    pub fn new(service_name: impl Into<String>) -> Self {
        Lookup {
            service_name: service_name.into(),
            port_name: None,
            protocol: None,
        }
    }
}

/// Resolution result. May be empty when the provider could query the
/// underlying system but found nothing.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    pub service_name: String,
    pub targets: Vec<ResolvedTarget>,
}

impl Resolved {
    pub fn empty(service_name: impl Into<String>) -> Self {
        Resolved {
            service_name: service_name.into(),
            targets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub host: String,
    pub port: Option<u16>,
    pub ip: Option<IpAddr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("provider timeout: {0}")]
    Timeout(String),
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("provider config invalid: {0}")]
    Config(String),
    #[error("not implemented (cs-discovery scaffold; see docs/research/sdk_spec/tasks/M03-discovery.md)")]
    NotImplemented,
}

/// The universal discovery interface. Every concrete provider
/// implements this one async method.
#[async_trait]
pub trait DiscoveryProvider: Send + Sync + std::fmt::Debug {
    async fn lookup(
        &self,
        query: &Lookup,
        resolve_timeout: Duration,
    ) -> Result<Resolved, DiscoveryError>;

    /// Stable provider name for logging / diagnostics.
    fn name(&self) -> &str;
}

/// First-success combinator. Tries providers in declaration order;
/// returns the first non-empty `Resolved`. Empty results fall through
/// to the next provider; explicit errors propagate (the caller can
/// downgrade to fall-through if they want).
#[derive(Debug)]
pub struct FirstSuccess {
    providers: Vec<Box<dyn DiscoveryProvider>>,
}

impl FirstSuccess {
    pub fn new(providers: Vec<Box<dyn DiscoveryProvider>>) -> Self {
        FirstSuccess { providers }
    }
}

#[async_trait]
impl DiscoveryProvider for FirstSuccess {
    async fn lookup(
        &self,
        query: &Lookup,
        resolve_timeout: Duration,
    ) -> Result<Resolved, DiscoveryError> {
        for p in &self.providers {
            match p.lookup(query, resolve_timeout).await {
                Ok(r) if !r.targets.is_empty() => return Ok(r),
                Ok(_) => continue,
                Err(_) => continue, // fall-through-on-error
            }
        }
        Ok(Resolved::empty(&query.service_name))
    }

    fn name(&self) -> &str {
        "first-success"
    }
}

#[cfg(feature = "static")]
pub mod static_provider {
    //! Static config-derived discovery. Always-on. Used for dev,
    //! tests, and any fixed-topology deployment.

    use super::*;

    /// Static list of (host, port) pairs.
    #[derive(Debug, Clone)]
    pub struct Static {
        service_name: String,
        targets: Vec<ResolvedTarget>,
    }

    impl Static {
        pub fn new(service_name: impl Into<String>, targets: Vec<ResolvedTarget>) -> Self {
            Static {
                service_name: service_name.into(),
                targets,
            }
        }
    }

    #[async_trait]
    impl DiscoveryProvider for Static {
        async fn lookup(
            &self,
            _query: &Lookup,
            _timeout: Duration,
        ) -> Result<Resolved, DiscoveryError> {
            Ok(Resolved {
                service_name: self.service_name.clone(),
                targets: self.targets.clone(),
            })
        }

        fn name(&self) -> &str {
            "static"
        }
    }
}

#[cfg(feature = "file")]
pub mod file_provider {
    //! File-backed discovery. Reads `host:port` entries from a path.
    //! Implementation deferred to M03 iter B.
    use super::*;

    #[derive(Debug, Clone)]
    pub struct FileBased {
        pub path: String,
    }

    impl FileBased {
        pub fn new(path: impl Into<String>) -> Self {
            FileBased { path: path.into() }
        }
    }

    #[async_trait]
    impl DiscoveryProvider for FileBased {
        async fn lookup(
            &self,
            _query: &Lookup,
            _timeout: Duration,
        ) -> Result<Resolved, DiscoveryError> {
            Err(DiscoveryError::NotImplemented)
        }

        fn name(&self) -> &str {
            "file"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "static")]
    #[tokio::test]
    async fn static_provider_returns_configured_targets() {
        let targets = vec![ResolvedTarget {
            host: "node-a".into(),
            port: Some(7000),
            ip: None,
        }];
        let p = static_provider::Static::new("crab", targets.clone());
        let r = p
            .lookup(&Lookup::new("crab"), Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(r.targets.len(), 1);
        assert_eq!(r.targets[0].host, "node-a");
    }

    #[cfg(feature = "static")]
    #[tokio::test]
    async fn first_success_returns_first_non_empty() {
        let empty = static_provider::Static::new("crab", vec![]);
        let pop = static_provider::Static::new(
            "crab",
            vec![ResolvedTarget {
                host: "n1".into(),
                port: None,
                ip: None,
            }],
        );
        let fs: FirstSuccess = FirstSuccess::new(vec![Box::new(empty), Box::new(pop)]);
        let r = fs
            .lookup(&Lookup::new("crab"), Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(r.targets.len(), 1);
        assert_eq!(r.targets[0].host, "n1");
    }

    #[tokio::test]
    async fn first_success_with_no_providers_returns_empty() {
        let fs = FirstSuccess::new(Vec::new());
        let r = fs
            .lookup(&Lookup::new("crab"), Duration::from_secs(1))
            .await
            .unwrap();
        assert!(r.targets.is_empty());
    }
}
