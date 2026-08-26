use std::{
    fs::OpenOptions,
    io::{Cursor, Read, Write},
    path::Path,
    time::Duration,
};

use flate2::read::GzDecoder;

use super::{
    service::verify_sha256,
    transaction::{sibling_path, PreparedCoreUpdate},
    CoreUpdateError, CoreUpdateResult, MihomoRelease,
};
use crate::platform_command;

const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;
const CANDIDATE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn prepare_downloaded(
    release: &MihomoRelease,
    target: &Path,
    archive: &[u8],
) -> CoreUpdateResult<PreparedCoreUpdate> {
    verify_sha256(archive, &release.asset.sha256)?;
    let staging = sibling_path(target, "staging")?;
    let result = write_candidate(&release.asset.name, archive, &staging)
        .and_then(|()| validate_candidate(&staging, &release.tag));
    if let Err(error) = result {
        let _ = std::fs::remove_file(&staging);
        return Err(error);
    }
    Ok(PreparedCoreUpdate {
        staging: Some(staging),
        target: target.to_path_buf(),
        tag: release.tag.clone(),
    })
}

fn write_candidate(asset_name: &str, archive: &[u8], staging: &Path) -> CoreUpdateResult<()> {
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(staging)
        .map_err(|error| {
            CoreUpdateError::Io(format!("无法创建 staging {}：{error}", staging.display()))
        })?;
    let extension = Path::new(asset_name)
        .extension()
        .and_then(|value| value.to_str());
    if extension.is_some_and(|value| value.eq_ignore_ascii_case("gz")) {
        let mut decoder = GzDecoder::new(Cursor::new(archive));
        copy_limited(&mut decoder, &mut output)?;
    } else if extension.is_some_and(|value| value.eq_ignore_ascii_case("zip")) {
        let stem = Path::new(asset_name)
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| CoreUpdateError::Archive(format!("资产名称无效：{asset_name}")))?;
        let expected = format!("{stem}.exe");
        let mut zip = zip::ZipArchive::new(Cursor::new(archive))
            .map_err(|error| CoreUpdateError::Archive(error.to_string()))?;
        let index = (0..zip.len())
            .find(|index| {
                zip.by_index(*index)
                    .is_ok_and(|entry| entry.name() == expected)
            })
            .ok_or_else(|| CoreUpdateError::Archive(format!("ZIP 中找不到 {expected}")))?;
        let mut entry = zip
            .by_index(index)
            .map_err(|error| CoreUpdateError::Archive(error.to_string()))?;
        copy_limited(&mut entry, &mut output)?;
    } else {
        return Err(CoreUpdateError::Archive(format!(
            "不支持的资产格式：{asset_name}"
        )));
    }
    output
        .flush()
        .map_err(|error| CoreUpdateError::Io(format!("刷新 staging 失败：{error}")))?;
    set_executable(staging)?;
    output
        .sync_all()
        .map_err(|error| CoreUpdateError::Io(format!("同步 staging 失败：{error}")))
}

fn copy_limited(reader: &mut impl Read, writer: &mut impl Write) -> CoreUpdateResult<()> {
    let mut limited = reader.take(MAX_BINARY_BYTES + 1);
    let copied = std::io::copy(&mut limited, writer)
        .map_err(|error| CoreUpdateError::Archive(error.to_string()))?;
    if copied > MAX_BINARY_BYTES {
        return Err(CoreUpdateError::TooLarge(format!(
            "解压后超过 {} MiB",
            MAX_BINARY_BYTES / 1024 / 1024
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> CoreUpdateResult<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|error| {
        CoreUpdateError::Io(format!("无法设置 {} 执行权限：{error}", path.display()))
    })
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> CoreUpdateResult<()> {
    Ok(())
}

fn validate_candidate(path: &Path, tag: &str) -> CoreUpdateResult<()> {
    let executable = path
        .to_str()
        .ok_or_else(|| CoreUpdateError::Candidate("候选内核路径不是有效 UTF-8".into()))?;
    let output = platform_command::output_with_timeout(executable, &["-v"], CANDIDATE_TIMEOUT)
        .map_err(CoreUpdateError::Candidate)?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let version = tag.trim_start_matches('v');
    if !output.status.success()
        || !combined.to_ascii_lowercase().contains("mihomo meta")
        || !combined.contains(version)
    {
        return Err(CoreUpdateError::Candidate(format!(
            "{} -v 未返回 Mihomo Meta {version}：{}",
            path.display(),
            combined.trim()
        )));
    }
    Ok(())
}
