use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const METRICS_PATH: &str = "/v1/metrics";
const PROTOBUF_CONTENT_TYPE: &str = "application/x-protobuf";
const TRACES_PATH: &str = "/v1/traces";

pub(crate) struct OtlpCollector {
    server: MockServer,
}

impl OtlpCollector {
    pub(crate) async fn start() -> Self {
        let server = MockServer::start().await;
        for signal_path in [METRICS_PATH, TRACES_PATH] {
            Mock::given(method("POST"))
                .and(path(signal_path))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
        }

        Self { server }
    }

    pub(crate) fn metrics_endpoint(&self) -> String {
        format!("{}{METRICS_PATH}", self.server.uri())
    }

    pub(crate) fn traces_endpoint(&self) -> String {
        format!("{}{TRACES_PATH}", self.server.uri())
    }

    pub(crate) async fn requests(&self) -> Result<Vec<OtlpRequest>, String> {
        self.server
            .received_requests()
            .await
            .ok_or_else(|| "the loopback collector did not record requests".to_string())?
            .into_iter()
            .map(OtlpRequest::decode)
            .collect()
    }
}

pub(crate) struct OtlpRequest {
    pub(crate) body: Vec<u8>,
    pub(crate) content_type: Option<String>,
    pub(crate) payload: OtlpPayload,
    pub(crate) resource_count: usize,
}

impl OtlpRequest {
    fn decode(request: wiremock::Request) -> Result<Self, String> {
        let content_type = request
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let payload = match request.url.path() {
            METRICS_PATH => OtlpPayload::Metrics(
                ExportMetricsServiceRequest::decode(request.body.as_slice())
                    .map_err(|error| format!("invalid OTLP metric payload: {error}"))?,
            ),
            TRACES_PATH => OtlpPayload::Traces(
                ExportTraceServiceRequest::decode(request.body.as_slice())
                    .map_err(|error| format!("invalid OTLP trace payload: {error}"))?,
            ),
            path => return Err(format!("unsupported OTLP signal path: {path}")),
        };
        let resource_count = payload.resource_count();

        Ok(Self {
            body: request.body,
            content_type,
            payload,
            resource_count,
        })
    }

    pub(crate) fn assert_protobuf(&self) {
        assert_eq!(self.content_type.as_deref(), Some(PROTOBUF_CONTENT_TYPE));
    }
}

pub(crate) enum OtlpPayload {
    Metrics(ExportMetricsServiceRequest),
    Traces(ExportTraceServiceRequest),
}

impl OtlpPayload {
    fn resource_count(&self) -> usize {
        match self {
            Self::Metrics(payload) => payload.resource_metrics.len(),
            Self::Traces(payload) => payload.resource_spans.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exposes_signal_endpoints() {
        // Arrange
        let collector = OtlpCollector::start().await;

        // Act
        let metrics_endpoint = collector.metrics_endpoint();
        let traces_endpoint = collector.traces_endpoint();

        // Assert
        assert!(metrics_endpoint.ends_with(METRICS_PATH));
        assert!(traces_endpoint.ends_with(TRACES_PATH));
    }
}
