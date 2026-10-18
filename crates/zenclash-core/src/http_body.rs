//! Bounded response-body reads shared by downloads and metadata requests.

#[derive(Debug)]
pub(crate) enum ReadBodyError {
    Http(reqwest::Error),
    TooLarge,
}

pub(crate) async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ReadBodyError> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(ReadBodyError::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(ReadBodyError::Http)? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(ReadBodyError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::{io, time::Duration};

    use futures_util::{StreamExt as _, stream};

    use super::*;

    #[tokio::test]
    async fn a_stream_over_budget_is_rejected_without_waiting_for_its_end() {
        let chunks = stream::iter([Ok::<_, io::Error>(vec![0; 5])]).chain(stream::pending());
        let response =
            reqwest::Response::from(http::Response::new(reqwest::Body::wrap_stream(chunks)));
        let result = tokio::time::timeout(Duration::from_secs(1), read_limited(response, 4))
            .await
            .expect("body reader waited for EOF after exceeding its budget");
        assert!(matches!(result, Err(ReadBodyError::TooLarge)));
    }

    #[tokio::test]
    async fn empty_exactly_sized_and_oversized_bodies_follow_the_same_limit() {
        for size in [0, 4, 5] {
            let response = reqwest::Response::from(http::Response::new(vec![0; size]));
            let result = read_limited(response, 4).await;
            if size <= 4 {
                assert_eq!(result.unwrap().len(), size);
            } else {
                assert!(matches!(result, Err(ReadBodyError::TooLarge)));
            }
        }
    }

    #[tokio::test]
    async fn transport_errors_are_preserved() {
        let chunks = stream::iter([Err::<Vec<u8>, _>(io::Error::other("lost response"))]);
        let response =
            reqwest::Response::from(http::Response::new(reqwest::Body::wrap_stream(chunks)));
        assert!(matches!(
            read_limited(response, 4).await,
            Err(ReadBodyError::Http(_))
        ));
    }
}
