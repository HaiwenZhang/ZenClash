use futures_util::StreamExt;
use reqwest::{Method, RequestBuilder, Response};
use serde::de::DeserializeOwned;

use super::{MihomoClient, MihomoError, MihomoResult};

const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

impl MihomoClient {
    pub(super) async fn get_json<T: DeserializeOwned>(&self, path: &str) -> MihomoResult<T> {
        let response = self.request(Method::GET, path)?.send().await?;
        Ok(ensure_success(response).await?.json().await?)
    }

    pub(super) async fn patch_json<T: serde::Serialize + Sync + ?Sized>(
        &self,
        path: &str,
        body: &T,
    ) -> MihomoResult<()> {
        let _mutation_guard = self.mutation_gate.lock().await;
        let response = self.request(Method::PATCH, path)?.json(body).send().await?;
        ensure_success(response).await?;
        Ok(())
    }

    pub(super) async fn put_json<T: serde::Serialize + Sync + ?Sized>(
        &self,
        path: &str,
        body: &T,
    ) -> MihomoResult<()> {
        let _mutation_guard = self.mutation_gate.lock().await;
        let response = self.request(Method::PUT, path)?.json(body).send().await?;
        ensure_success(response).await?;
        Ok(())
    }

    pub(super) async fn send_empty(&self, method: Method, path: &str) -> MihomoResult<()> {
        let _mutation_guard = self.mutation_gate.lock().await;
        let response = self.request(method, path)?.send().await?;
        ensure_success(response).await?;
        Ok(())
    }

    pub(super) fn request(&self, method: Method, path: &str) -> MihomoResult<RequestBuilder> {
        let mut request = self.http.request(method, self.endpoint.http_url(path)?);
        if !self.endpoint.secret.is_empty() {
            request = request.bearer_auth(&self.endpoint.secret);
        }
        Ok(request)
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

    if let Ok(body) = serde_json::from_slice::<ErrorBody>(payload) {
        if !body.message.trim().is_empty() {
            return body.message;
        }
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
