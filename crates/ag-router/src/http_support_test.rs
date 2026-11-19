//! Shared transport test fixtures.

use serde::de::DeserializeOwned;

use super::{HttpTransport, ProviderRequest, TransportError, decode_success_body};

/// Sends a request and decodes its successful JSON body, as the backends
/// read a response that does not stream.
pub(super) trait PostJson {
    /// Sends `request` and decodes its successful body.
    async fn post_json<Response: DeserializeOwned>(
        &self,
        request: &ProviderRequest,
    ) -> Result<Response, TransportError>;
}

impl PostJson for HttpTransport {
    async fn post_json<Response: DeserializeOwned>(
        &self,
        request: &ProviderRequest,
    ) -> Result<Response, TransportError> {
        let mut response = self.send(request).await?;

        decode_success_body(&mut response).await
    }
}
