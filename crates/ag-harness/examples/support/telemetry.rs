use std::env;
use std::error::Error;
use std::future::Future;
use std::io::{self, Write};

use opentelemetry::global;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;

pub(crate) type DynError = Box<dyn Error + Send + Sync>;

/// Runs an example with optional OTLP metrics configured from the environment.
pub(crate) async fn run_with_metrics(
    service_name: &'static str,
    operation: impl Future<Output = Result<(), DynError>>,
) -> Result<(), DynError> {
    let meter_provider = init_metrics(service_name, metrics_endpoint_is_configured())?;
    let operation_result = operation.await;
    let shutdown_result = shutdown_metrics(meter_provider).await;

    finish(operation_result, shutdown_result)
}

fn init_metrics(
    service_name: &'static str,
    endpoint_is_configured: bool,
) -> Result<Option<SdkMeterProvider>, DynError> {
    if !endpoint_is_configured {
        return Ok(None);
    }

    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .build()?;
    let provider = SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(metrics_resource(
            service_name,
            env::var_os("OTEL_SERVICE_NAME").is_some(),
        ))
        .build();
    global::set_meter_provider(provider.clone());

    Ok(Some(provider))
}

fn metrics_endpoint_is_configured() -> bool {
    env::var_os("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT").is_some()
        || env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some()
}

fn metrics_resource(service_name: &'static str, service_name_is_configured: bool) -> Resource {
    let resource = Resource::builder();
    if service_name_is_configured {
        return resource.build();
    }

    resource.with_service_name(service_name).build()
}

async fn shutdown_metrics(meter_provider: Option<SdkMeterProvider>) -> Result<(), DynError> {
    let Some(meter_provider) = meter_provider else {
        return Ok(());
    };

    tokio::task::spawn_blocking(move || meter_provider.shutdown()).await??;

    Ok(())
}

fn finish(
    operation_result: Result<(), DynError>,
    shutdown_result: Result<(), DynError>,
) -> Result<(), DynError> {
    match (operation_result, shutdown_result) {
        (Err(error), Err(shutdown_error)) => {
            drop(writeln!(
                io::stderr().lock(),
                "telemetry shutdown also failed: {shutdown_error}"
            ));

            Err(error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), shutdown_result) => shutdown_result,
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::Key;

    use super::*;

    fn test_error(message: &'static str) -> DynError {
        io::Error::other(message).into()
    }

    #[test]
    fn metrics_are_disabled_without_an_endpoint() {
        // Arrange & Act
        let meter_provider =
            init_metrics("test-service", false).expect("disabled metrics setup should succeed");

        // Assert
        assert!(meter_provider.is_none());
    }

    #[test]
    fn supplied_service_name_is_used_only_without_environment_configuration() {
        // Arrange
        let service_name_key = Key::from_static_str("service.name");

        // Act
        let supplied_resource = metrics_resource("test-service", false);
        let configured_resource = metrics_resource("test-service", true);

        // Assert
        assert_eq!(
            supplied_resource
                .get(&service_name_key)
                .expect("supplied service name should be present")
                .as_str(),
            "test-service"
        );
        assert_ne!(
            configured_resource
                .get(&service_name_key)
                .expect("SDK service name should be present")
                .as_str(),
            "test-service"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_is_disabled_without_a_provider() {
        // Arrange & Act
        let result = shutdown_metrics(None).await;

        // Assert
        result.expect("disabled metrics shutdown should succeed");
    }

    #[test]
    fn finish_preserves_operation_error_priority() {
        // Arrange
        let operation_error = test_error("model request failed");
        let shutdown_error = test_error("metrics flush failed");

        // Act
        let combined_result = finish(Err(operation_error), Err(shutdown_error));
        let operation_only_result = finish(Err(test_error("operation only")), Ok(()));
        let shutdown_only_result = finish(Ok(()), Err(test_error("shutdown only")));
        let success_result = finish(Ok(()), Ok(()));

        // Assert
        assert_eq!(
            combined_result
                .expect_err("both failures should fail")
                .to_string(),
            "model request failed"
        );
        assert_eq!(
            operation_only_result
                .expect_err("the operation failure should be returned")
                .to_string(),
            "operation only"
        );
        assert_eq!(
            shutdown_only_result
                .expect_err("the shutdown failure should be returned")
                .to_string(),
            "shutdown only"
        );
        success_result.expect("successful operation and shutdown should succeed");
    }
}
