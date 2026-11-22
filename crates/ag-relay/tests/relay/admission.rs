//! Admission policies an embedding binary supplies.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use ag_relay::{
    AdmissionError, AdmissionPolicy, CONNECT_PATH_PREFIX, REGISTER_PATH, RelayConfig, RelayHandle,
    RelayLimits,
};
use async_trait::async_trait;
use tokio::sync::Notify;

use crate::support::{
    TestResult, connect_device, keypair, next_close, next_stream_id, open, prove_key,
    refused_registration, refused_status, register,
};

/// Policy an embedder might write, such as one backed by its own store.
enum StorePolicy {
    /// Admits only this instance identifier.
    Only(String),
    /// The store cannot be reached.
    Unreachable,
    /// Reports each question on `asked`, then never answers.
    Stalled { asked: Arc<Notify> },
}

#[async_trait]
impl AdmissionPolicy for StorePolicy {
    async fn admit(&self, instance_id: &str) -> Result<bool, AdmissionError> {
        match self {
            Self::Only(admitted) => Ok(instance_id == admitted),
            Self::Unreachable => Err(AdmissionError("store is unreachable".to_string())),
            Self::Stalled { asked } => {
                asked.notify_one();

                std::future::pending().await
            }
        }
    }
}

/// Starts a relay with `policy` on a free loopback port, as an embedding
/// binary would.
async fn start_with_policy(policy: StorePolicy, limits: RelayLimits) -> TestResult<RelayHandle> {
    let config = RelayConfig {
        bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        client_address_header: None,
        limits,
    };
    let relay = ag_relay::start_with_admission(config, Arc::new(policy)).await?;

    Ok(relay)
}

#[tokio::test]
async fn custom_policy_registers_admitted_instances_and_refuses_others() -> TestResult {
    // Arrange
    let admitted = keypair()?;
    let refused = keypair()?;
    let policy = StorePolicy::Only(admitted.public().fingerprint());
    let relay = start_with_policy(policy, RelayLimits::default()).await?;

    // Act
    let mut control = register(&relay, &admitted).await?;
    let _device = connect_device(&relay, &admitted).await?;
    let stream_id = next_stream_id(&mut control).await?;
    let refusal = refused_registration(relay.local_addr(), &refused).await?;
    let refused_path = format!("{CONNECT_PATH_PREFIX}{}", refused.public().fingerprint());
    let refused_device = refused_status(&relay, &refused_path).await?;

    // Assert
    assert_ne!(stream_id, "");
    assert_eq!(refusal, (1008, "Instance is not admitted".to_string()));
    assert_eq!(refused_device, 404);

    Ok(())
}

#[tokio::test]
async fn policy_errors_and_timeouts_refuse_the_instance() -> TestResult {
    // Arrange
    let limits = RelayLimits {
        admission_timeout: Duration::from_millis(50),
        ..RelayLimits::default()
    };
    let policies = [
        StorePolicy::Unreachable,
        StorePolicy::Stalled {
            asked: Arc::new(Notify::new()),
        },
    ];

    for policy in policies {
        let relay = start_with_policy(policy, limits).await?;

        // Act
        let refusal = refused_registration(relay.local_addr(), &keypair()?).await?;

        // Assert
        assert_eq!(refusal, (1001, "Relay is unavailable".to_string()));
    }

    Ok(())
}

#[tokio::test]
async fn pending_decisions_count_as_handshakes_and_end_with_the_relay() -> TestResult {
    // Arrange
    let limits = RelayLimits {
        admission_timeout: Duration::from_mins(1),
        max_handshakes: 1,
        ..RelayLimits::default()
    };
    let asked = Arc::new(Notify::new());
    let policy = StorePolicy::Stalled {
        asked: Arc::clone(&asked),
    };
    let relay = start_with_policy(policy, limits).await?;
    let mut waiting = open(&relay, REGISTER_PATH).await?;
    prove_key(&mut waiting, &keypair()?).await?;
    asked.notified().await;

    // Act
    let refused = refused_status(&relay, REGISTER_PATH).await?;
    relay.shutdown().await;
    let close = next_close(&mut waiting).await?;

    // Assert
    assert_eq!(refused, 503);
    assert_eq!(close, (1001, "Relay stopped".to_string()));

    Ok(())
}
