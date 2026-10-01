use futures_util::StreamExt;
use reqwest::{Method, Response};
use serde::de::DeserializeOwned;

use super::transport::ControllerBackend;
use super::{MihomoClient, MihomoError, MihomoResult};

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

impl MihomoClient {
    pub(super) async fn get_json<T: DeserializeOwned>(&self, path: &str) -> MihomoResult<T> {
        self.send_api(Method::GET, path, &[], None, None)
            .await?
            .json()
            .await
    }

    pub(super) async fn patch_json<T: serde::Serialize + Sync + ?Sized>(
        &self,
        path: &str,
        body: &T,
    ) -> MihomoResult<()> {
        let client = self.pin_binding()?;
        let _mutation_guard = client.mutation_gate.lock().await;
        client
            .send_api(
                Method::PATCH,
                path,
                &[],
                Some(serde_json::to_value(body)?),
                None,
            )
            .await?;
        Ok(())
    }

    pub(super) async fn put_json<T: serde::Serialize + Sync + ?Sized>(
        &self,
        path: &str,
        body: &T,
    ) -> MihomoResult<()> {
        let client = self.pin_binding()?;
        let _mutation_guard = client.mutation_gate.lock().await;
        client
            .send_api(
                Method::PUT,
                path,
                &[],
                Some(serde_json::to_value(body)?),
                None,
            )
            .await?;
        Ok(())
    }

    pub(super) async fn send_empty(&self, method: Method, path: &str) -> MihomoResult<()> {
        let client = self.pin_binding()?;
        let _mutation_guard = client.mutation_gate.lock().await;
        client.send_api(method, path, &[], None, None).await?;
        Ok(())
    }

    pub(super) async fn send_api(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<serde_json::Value>,
        timeout: Option<std::time::Duration>,
    ) -> MihomoResult<ApiReply> {
        let binding = self.operation_binding()?;
        let response = match &binding.backend {
            ControllerBackend::Direct(_) | ControllerBackend::Local(_) => {
                let endpoint = binding.endpoint().ok_or(MihomoError::StaleBinding)?;
                let mut request = self
                    .http
                    .request(method, endpoint.http_url(path)?)
                    .query(query);
                if !endpoint.secret.is_empty() {
                    request = request.bearer_auth(&endpoint.secret);
                }
                if let Some(body) = body {
                    request = request.json(&body);
                }
                if let Some(timeout) = timeout {
                    request = request.timeout(timeout);
                }
                if !self.binding.is_current(binding.generation) {
                    return Err(MihomoError::StaleBinding);
                }
                ReplyBody::Direct(ensure_success(request.send().await?).await?)
            }
            ControllerBackend::Service { runtime } => {
                let mut url = reqwest::Url::parse(&format!("http://localhost{path}"))
                    .map_err(|error| MihomoError::InvalidInput(error.to_string()))?;
                if !query.is_empty() {
                    url.query_pairs_mut().extend_pairs(query.iter().copied());
                }
                let path = format!(
                    "{}{}",
                    url.path(),
                    url.query()
                        .map_or_else(String::new, |query| format!("?{query}"))
                );
                if !self.binding.is_current(binding.generation) {
                    return Err(MihomoError::StaleBinding);
                }
                let response = runtime.client.api(method.as_str(), &path, body).await?;
                if !(200..300).contains(&response.status) {
                    let payload = serde_json::to_vec(&response.body)?;
                    let mut message =
                        error_message(&payload[..payload.len().min(MAX_ERROR_BODY_BYTES)]);
                    if payload.len() > MAX_ERROR_BODY_BYTES {
                        message.push('…');
                    }
                    return Err(MihomoError::Api {
                        status: response.status,
                        message,
                    });
                }
                ReplyBody::Service(response.body)
            }
        };
        if !self.binding.is_current(binding.generation) {
            return Err(MihomoError::StaleTransport);
        }
        Ok(ApiReply {
            response,
            binding: self.binding.clone(),
            generation: binding.generation,
        })
    }
}

enum ReplyBody {
    Direct(Response),
    Service(serde_json::Value),
}

pub(super) struct ApiReply {
    response: ReplyBody,
    binding: std::sync::Arc<super::ControllerBinding>,
    generation: u64,
}

impl ApiReply {
    pub(super) async fn json<T: DeserializeOwned>(self) -> MihomoResult<T> {
        let value = match self.response {
            ReplyBody::Direct(response) => response.json().await?,
            ReplyBody::Service(value) => serde_json::from_value(value)?,
        };
        if !self.binding.is_current(self.generation) {
            return Err(MihomoError::StaleTransport);
        }
        Ok(value)
    }
}

pub(super) async fn ensure_success(response: Response) -> MihomoResult<Response> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status().as_u16();
    let mut stream = response.bytes_stream();
    let mut payload = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(MihomoError::Http)?;
        let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(payload.len());
        if chunk.len() > remaining {
            payload.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        payload.extend_from_slice(&chunk);
    }
    let mut message = error_message(&payload);
    if truncated {
        message.push('…');
    }
    Err(MihomoError::Api { status, message })
}

fn error_message(payload: &[u8]) -> String {
    #[derive(serde::Deserialize)]
    struct ErrorBody {
        #[serde(default)]
        message: String,
    }

    if let Ok(body) = serde_json::from_slice::<ErrorBody>(payload)
        && !body.message.trim().is_empty()
    {
        return body.message;
    }
    let body = String::from_utf8_lossy(payload).trim().to_owned();
    if body.is_empty() {
        "Mihomo 未返回错误详情".into()
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::error_message;

    #[test]
    fn error_message_extracts_mihomo_json_message() {
        assert_eq!(
            error_message(br#"{"message":"configuration rejected"}"#),
            "configuration rejected"
        );
    }

    #[test]
    fn error_message_preserves_plain_text_response() {
        assert_eq!(error_message(b"bad gateway"), "bad gateway");
    }
}
