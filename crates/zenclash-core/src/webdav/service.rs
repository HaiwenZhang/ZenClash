use std::time::Duration;

use reqwest::{header::CONTENT_TYPE, Method, RequestBuilder, StatusCode, Url};

use self::{
    protocol::{
        append_segments, parse_backup_listing, push_url_segment, read_bytes_limited,
        read_utf8_limited, require_status, webdav_method,
    },
    transfer::{backup_filename, backup_prefix, TransferFile},
};

use super::{
    model::{validate_filename, ValidatedWebDavSettings},
    WebDavBackup, WebDavError, WebDavResult, WebDavSettings, WebDavUploadSummary,
};
use crate::{backup::MAX_ARCHIVE_BYTES, BackupManager, PreparedBackupRestore};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
static WEBDAV_MUTATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

mod protocol;
mod transfer;

/// Authenticated `WebDAV` client integrated with `ZenClash` backup archives.
#[derive(Clone)]
pub struct WebDavService {
    settings: WebDavSettings,
    client: reqwest::Client,
    base_url: Url,
    directory: Vec<String>,
}

impl std::fmt::Debug for WebDavService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebDavService")
            .field("settings", &self.settings)
            .field("base_url", &self.base_url)
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}

impl WebDavService {
    /// Creates a redirect-resistant `WebDAV` client from validated settings.
    ///
    /// Redirects are disabled so HTTP Basic credentials cannot be forwarded to
    /// an unexpected host. Invalid TLS certificates are accepted only when the
    /// persisted setting explicitly opts in.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid URLs/directories or TLS client construction.
    pub fn new(settings: WebDavSettings) -> WebDavResult<Self> {
        let ValidatedWebDavSettings {
            base_url,
            directory,
        } = settings.validate()?;
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(settings.accept_invalid_certificates)
            .user_agent(concat!("ZenClash/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            settings,
            client,
            base_url,
            directory,
        })
    }

    /// Ensures the configured remote directory exists and reads its backup list.
    ///
    /// # Errors
    ///
    /// Returns transport, authentication, status, XML, or response-limit errors.
    pub async fn test_connection(&self) -> WebDavResult<Vec<WebDavBackup>> {
        self.list_backups().await
    }

    /// Lists safe ZIP backups in the configured remote directory.
    ///
    /// # Errors
    ///
    /// Returns an error when directory creation, `PROPFIND`, XML parsing, or
    /// bounded response reading fails.
    pub async fn list_backups(&self) -> WebDavResult<Vec<WebDavBackup>> {
        self.ensure_remote_directory().await?;
        let url = self.directory_url()?;
        let method = webdav_method(b"PROPFIND")?;
        let response = self
            .request(method.clone(), url)
            .header("depth", "1")
            .header(CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(
                r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#,
            )
            .send()
            .await?;
        let response = require_status("PROPFIND", response, &[StatusCode::MULTI_STATUS]).await?;
        let xml = read_utf8_limited(response, protocol::MAX_PROPFIND_BYTES, "PROPFIND XML").await?;
        parse_backup_listing(&xml)
    }

    /// Exports the current local snapshot and uploads it with same-device retention.
    ///
    /// # Errors
    ///
    /// Returns an error for local backup, task, upload, listing, or deletion failures.
    pub async fn upload_snapshot(
        &self,
        manager: &BackupManager,
    ) -> WebDavResult<WebDavUploadSummary> {
        let _mutation = WEBDAV_MUTATION_LOCK.lock().await;
        self.ensure_remote_directory().await?;
        let filename = backup_filename();
        let transfer = TransferFile::new(manager.data_root(), &filename)?;
        let export_manager = manager.clone();
        let export_path = transfer.path.clone();
        tokio::task::spawn_blocking(move || export_manager.export_to(export_path))
            .await
            .map_err(|error| WebDavError::Task(error.to_string()))??;
        let bytes = tokio::fs::read(&transfer.path).await?;
        if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
            return Err(WebDavError::ResponseTooLarge(
                "本地备份 ZIP 超过 128 MiB".into(),
            ));
        }
        let response = self
            .request(Method::PUT, self.file_url(&filename)?)
            .header(CONTENT_TYPE, "application/zip")
            .body(bytes)
            .send()
            .await?;
        require_status(
            "PUT",
            response,
            &[StatusCode::OK, StatusCode::CREATED, StatusCode::NO_CONTENT],
        )
        .await?;
        let removed_backups = self.enforce_retention(&filename).await?;
        let backup = self
            .list_backups()
            .await?
            .into_iter()
            .find(|backup| backup.filename == filename)
            .unwrap_or(WebDavBackup {
                filename,
                size_bytes: None,
                modified: None,
            });
        Ok(WebDavUploadSummary {
            backup,
            removed_backups,
        })
    }

    /// Downloads and validates a remote archive into a reversible local restore.
    ///
    /// No live data changes until the caller activates the returned value.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe filenames, HTTP failures, response limits,
    /// local staging failures, or invalid backup contents.
    pub async fn prepare_restore(
        &self,
        manager: &BackupManager,
        filename: &str,
    ) -> WebDavResult<PreparedBackupRestore> {
        validate_filename(filename)?;
        let response = self
            .request(Method::GET, self.file_url(filename)?)
            .send()
            .await?;
        let response = require_status("GET", response, &[StatusCode::OK]).await?;
        let archive_limit = usize::try_from(MAX_ARCHIVE_BYTES).map_err(|error| {
            WebDavError::ResponseTooLarge(format!("平台无法表示备份大小限制：{error}"))
        })?;
        let bytes = read_bytes_limited(response, archive_limit, "备份 ZIP").await?;
        let transfer = TransferFile::new(manager.data_root(), filename)?;
        tokio::fs::write(&transfer.path, bytes).await?;
        let restore_manager = manager.clone();
        let path = transfer.path.clone();
        tokio::task::spawn_blocking(move || restore_manager.prepare_restore(path))
            .await
            .map_err(|error| WebDavError::Task(error.to_string()))?
            .map_err(WebDavError::from)
    }

    /// Deletes one safe remote ZIP basename.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe names, transport failures, or rejected deletes.
    pub async fn delete_backup(&self, filename: &str) -> WebDavResult<()> {
        let _mutation = WEBDAV_MUTATION_LOCK.lock().await;
        self.delete_backup_unlocked(filename).await
    }

    async fn delete_backup_unlocked(&self, filename: &str) -> WebDavResult<()> {
        validate_filename(filename)?;
        let response = self
            .request(Method::DELETE, self.file_url(filename)?)
            .send()
            .await?;
        require_status(
            "DELETE",
            response,
            &[StatusCode::OK, StatusCode::NO_CONTENT],
        )
        .await?;
        Ok(())
    }

    async fn ensure_remote_directory(&self) -> WebDavResult<()> {
        let mut url = self.base_url.clone();
        for segment in &self.directory {
            push_url_segment(&mut url, segment)?;
            let response = self
                .request(webdav_method(b"MKCOL")?, url.clone())
                .send()
                .await?;
            require_status(
                "MKCOL",
                response,
                &[StatusCode::CREATED, StatusCode::METHOD_NOT_ALLOWED],
            )
            .await?;
        }
        Ok(())
    }

    async fn enforce_retention(&self, newest: &str) -> WebDavResult<usize> {
        if self.settings.max_backups == 0 {
            return Ok(0);
        }
        let prefix = backup_prefix();
        let mut backups = self
            .list_backups()
            .await?
            .into_iter()
            .filter(|backup| backup.filename.starts_with(&prefix))
            .collect::<Vec<_>>();
        backups.sort_unstable_by(|left, right| right.filename.cmp(&left.filename));
        if !backups.iter().any(|backup| backup.filename == newest) {
            return Err(WebDavError::Xml("上传成功后目录列表未返回新备份".into()));
        }
        let obsolete = backups
            .into_iter()
            .skip(self.settings.max_backups)
            .collect::<Vec<_>>();
        for backup in &obsolete {
            self.delete_backup_unlocked(&backup.filename).await?;
        }
        Ok(obsolete.len())
    }

    fn request(&self, method: Method, url: Url) -> RequestBuilder {
        let request = self.client.request(method, url);
        if self.settings.username.is_empty() {
            request
        } else {
            request.basic_auth(&self.settings.username, Some(&self.settings.password))
        }
    }

    fn directory_url(&self) -> WebDavResult<Url> {
        append_segments(self.base_url.clone(), &self.directory)
    }

    fn file_url(&self, filename: &str) -> WebDavResult<Url> {
        validate_filename(filename)?;
        let mut url = self.directory_url()?;
        push_url_segment(&mut url, filename)?;
        Ok(url)
    }
}
