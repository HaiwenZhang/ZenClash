use futures_util::StreamExt;
use percent_encoding::percent_decode_str;
use reqwest::{Method, Response, StatusCode, Url};

use super::super::{model::validate_filename, WebDavBackup, WebDavError, WebDavResult};

const MAX_ERROR_BYTES: usize = 64 * 1024;
pub(super) const MAX_PROPFIND_BYTES: usize = 4 * 1024 * 1024;

pub(super) async fn require_status(
    method: &str,
    response: Response,
    accepted: &[StatusCode],
) -> WebDavResult<Response> {
    if accepted.contains(&response.status()) {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let message = read_utf8_limited(response, MAX_ERROR_BYTES, "错误响应")
        .await
        .unwrap_or_else(|error| error.to_string());
    Err(WebDavError::Status {
        method: method.into(),
        status,
        message,
    })
}

pub(super) async fn read_utf8_limited(
    response: Response,
    limit: usize,
    label: &str,
) -> WebDavResult<String> {
    let bytes = read_bytes_limited(response, limit, label).await?;
    String::from_utf8(bytes)
        .map_err(|error| WebDavError::Xml(format!("{label} 不是 UTF-8：{error}")))
}

pub(super) async fn read_bytes_limited(
    response: Response,
    limit: usize,
    label: &str,
) -> WebDavResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(WebDavError::ResponseTooLarge(format!(
            "{label} 的 Content-Length 超过 {limit} 字节"
        )));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(WebDavError::ResponseTooLarge(format!(
                "{label} 超过 {limit} 字节"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) fn parse_backup_listing(xml: &str) -> WebDavResult<Vec<WebDavBackup>> {
    let document =
        roxmltree::Document::parse(xml).map_err(|error| WebDavError::Xml(error.to_string()))?;
    let mut backups = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "response")
        .filter_map(parse_backup_response)
        .collect::<Vec<_>>();
    backups.sort_unstable_by(|left, right| right.filename.cmp(&left.filename));
    Ok(backups)
}

fn parse_backup_response(response: roxmltree::Node<'_, '_>) -> Option<WebDavBackup> {
    let is_collection = response
        .descendants()
        .any(|node| node.is_element() && node.tag_name().name() == "collection");
    if is_collection {
        return None;
    }
    let href = descendant_text(response, "href")?;
    let encoded_filename = href.trim_end_matches('/').rsplit('/').next()?;
    let filename = percent_decode_str(encoded_filename)
        .decode_utf8()
        .ok()?
        .into_owned();
    if validate_filename(&filename).is_err() {
        return None;
    }
    let size_bytes =
        descendant_text(response, "getcontentlength").and_then(|size| size.parse().ok());
    let modified = descendant_text(response, "getlastmodified").map(str::to_owned);
    Some(WebDavBackup {
        filename,
        size_bytes,
        modified,
    })
}

fn descendant_text<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.descendants()
        .find(|child| child.is_element() && child.tag_name().name() == name)
        .and_then(|child| child.text())
}

pub(super) fn append_segments(mut url: Url, segments: &[String]) -> WebDavResult<Url> {
    for segment in segments {
        push_url_segment(&mut url, segment)?;
    }
    Ok(url)
}

pub(super) fn push_url_segment(url: &mut Url, segment: &str) -> WebDavResult<()> {
    url.path_segments_mut()
        .map_err(|()| WebDavError::InvalidSettings("URL 不能追加路径".into()))?
        .push(segment);
    Ok(())
}

pub(super) fn webdav_method(method: &[u8]) -> WebDavResult<Method> {
    Method::from_bytes(method)
        .map_err(|error| WebDavError::InvalidSettings(format!("WebDAV 方法无效：{error}")))
}
