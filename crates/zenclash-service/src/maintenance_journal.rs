//! Fixed-path durable installation recovery; callers own native authorization.
use crate::InstalledMetadata;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAX_ARTIFACT: u64 = 256 * 1024 * 1024;
const MAX_RECORD: u64 = 128 * 1024;
const RECORD: &str = "maintenance.json";
const DIAGNOSTICS: [&str; 2] = ["helper.diagnostic", "core.diagnostic"];
const COMPONENTS: [(&str, &str, &str); 2] = [
    ("helper.new", "helper.previous", "helper.restore"),
    ("core.new", "core.previous", "core.restore"),
];

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    Committed,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RollbackKind {
    ApprovedOrEmpty,
    Diagnostic,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Journal {
    schema_version: u32,
    phase: Phase,
    rollback_kind: RollbackKind,
    old_hashes: [Option<String>; 2],
    new_hashes: [String; 2],
    allowed_hashes: [Vec<String>; 2],
    old_metadata: Option<InstalledMetadata>,
    new_metadata: InstalledMetadata,
    allowed_metadata: Vec<InstalledMetadata>,
    was_running: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyJournal {
    schema_version: u32,
    phase: Phase,
    old_hashes: [Option<String>; 2],
    new_hashes: [String; 2],
    allowed_hashes: [Vec<String>; 2],
    old_metadata: Option<InstalledMetadata>,
    new_metadata: InstalledMetadata,
    allowed_metadata: Vec<InstalledMetadata>,
    was_running: bool,
}

impl Journal {
    pub(crate) fn prepare_repair(
        root: &Path,
        metadata: &InstalledMetadata,
        helper_hash: &str,
        was_running: bool,
    ) -> io::Result<Self> {
        Self::prepare_inner(root, metadata, helper_hash, was_running, true)
    }

    pub(crate) fn prepare(
        root: &Path,
        metadata: &InstalledMetadata,
        helper_hash: &str,
        was_running: bool,
    ) -> io::Result<Self> {
        Self::prepare_inner(root, metadata, helper_hash, was_running, false)
    }

    fn prepare_inner(
        root: &Path,
        metadata: &InstalledMetadata,
        helper_hash: &str,
        was_running: bool,
        allow_diagnostic: bool,
    ) -> io::Result<Self> {
        if Self::load(root)?.is_some() {
            return Err(conflict("unfinished maintenance requires recovery"));
        }
        metadata.validate().map_err(io::Error::other)?;
        let names = [helper_name(), core_name()];
        for name in DIAGNOSTICS {
            if regular(&root.join(name))?.is_some() {
                return Err(conflict("unrecorded maintenance diagnostic exists"));
            }
        }
        let mut old_hashes = [None, None];
        let new_hashes = [helper_hash.to_owned(), metadata.core_sha256().to_owned()];
        let mut allowed_hashes: [Vec<String>; 2] = [vec![], vec![]];
        for index in 0..2 {
            old_hashes[index] = hash_optional(&root.join(names[index]))?;
            for name in [
                names[index],
                COMPONENTS[index].0,
                COMPONENTS[index].1,
                COMPONENTS[index].2,
            ] {
                if let Some(hash) = hash_optional(&root.join(name))?
                    && !allowed_hashes[index].contains(&hash)
                {
                    allowed_hashes[index].push(hash);
                }
            }
            let staged = root.join(COMPONENTS[index].0);
            if regular(&staged)?.is_none_or(|file| file.len() == 0)
                || hash_optional(&staged)?.as_ref() != Some(&new_hashes[index])
            {
                return Err(conflict("approved staging digest changed"));
            }
        }
        let mut old_metadata = metadata_optional(&root.join("install.json"))?;
        if allow_diagnostic && old_metadata.is_none() {
            return Err(conflict(
                "repair requires existing installation ownership metadata",
            ));
        }
        let archived = metadata_optional(&root.join("metadata.previous"))?;
        let mut allowed_metadata = vec![metadata.clone()];
        for record in [old_metadata.as_ref(), archived.as_ref()]
            .into_iter()
            .flatten()
        {
            if !allowed_metadata.contains(record) {
                allowed_metadata.push(record.clone());
            }
        }
        // Only a complete pair authenticated by its manifest may become the
        // recovery archive. A damaged component must not erase the last good
        // pair, even when the other current component is unchanged.
        let mut rollback_kind = RollbackKind::ApprovedOrEmpty;
        if let Some(record) = &old_metadata {
            if !approved_pair(root, names, record)? {
                match archived {
                    Some(record)
                        if approved_pair(root, ["helper.previous", "core.previous"], &record)? =>
                    {
                        old_hashes = [
                            Some(record.helper_sha256().to_owned()),
                            Some(record.core_sha256().to_owned()),
                        ];
                        old_metadata = Some(record);
                    }
                    _ if allow_diagnostic => rollback_kind = RollbackKind::Diagnostic,
                    _ => {
                        return Err(conflict(
                            "damaged installation has no complete approved archive; administrator recovery is required",
                        ));
                    }
                }
            }
        } else if old_hashes.iter().any(Option::is_some)
            || archived.is_some()
            || hash_optional(&root.join("helper.previous"))?.is_some()
            || hash_optional(&root.join("core.previous"))?.is_some()
        {
            return Err(conflict(
                "installation ownership metadata is missing; administrator recovery is required",
            ));
        }
        let journal = Self {
            schema_version: 2,
            phase: Phase::Prepared,
            rollback_kind,
            old_hashes,
            new_hashes,
            allowed_hashes,
            old_metadata,
            new_metadata: metadata.clone(),
            allowed_metadata,
            was_running,
        };
        journal.validate()?;
        journal.persist(root)?;
        Ok(journal)
    }

    pub(crate) fn load(root: &Path) -> io::Result<Option<Self>> {
        let path = root.join(RECORD);
        if regular(&path)?.is_none() {
            return Ok(None);
        }
        let mut bytes = vec![];
        File::open(path)?
            .take(MAX_RECORD + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(conflict("maintenance journal exceeds budget"));
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let journal = match value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(1) => {
                // Legacy records have only the approved-pair invariant. Never
                // infer diagnostic authority from mismatched old bytes.
                let legacy: LegacyJournal =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if legacy.schema_version != 1 {
                    return Err(conflict("unsupported maintenance journal schema"));
                }
                Self {
                    schema_version: 2,
                    phase: legacy.phase,
                    rollback_kind: RollbackKind::ApprovedOrEmpty,
                    old_hashes: legacy.old_hashes,
                    new_hashes: legacy.new_hashes,
                    allowed_hashes: legacy.allowed_hashes,
                    old_metadata: legacy.old_metadata,
                    new_metadata: legacy.new_metadata,
                    allowed_metadata: legacy.allowed_metadata,
                    was_running: legacy.was_running,
                }
            }
            Some(2) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            _ => return Err(conflict("unsupported maintenance journal schema")),
        };
        journal.validate()?;
        Ok(Some(journal))
    }

    fn validate(&self) -> io::Result<()> {
        if self.schema_version != 2 {
            return Err(conflict("unsupported maintenance journal schema"));
        }
        for hash in self
            .old_hashes
            .iter()
            .flatten()
            .chain(self.new_hashes.iter())
            .chain(self.allowed_hashes.iter().flatten())
        {
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(conflict("invalid maintenance digest"));
            }
        }
        if self.allowed_hashes.iter().any(|hashes| hashes.len() > 4)
            || self.allowed_metadata.len() > 3
        {
            return Err(conflict("maintenance snapshot exceeds budget"));
        }
        for record in self
            .old_metadata
            .iter()
            .chain(std::iter::once(&self.new_metadata))
            .chain(self.allowed_metadata.iter())
        {
            record.validate().map_err(io::Error::other)?;
        }
        if self.new_hashes[0] != self.new_metadata.helper_sha256()
            || self.new_hashes[1] != self.new_metadata.core_sha256()
        {
            return Err(conflict("maintenance metadata digest mismatch"));
        }
        let approved_old = self
            .old_metadata
            .as_ref()
            .map(|record| {
                [
                    Some(record.helper_sha256().to_owned()),
                    Some(record.core_sha256().to_owned()),
                ]
            })
            .unwrap_or([None, None]);
        match self.rollback_kind {
            RollbackKind::ApprovedOrEmpty if self.old_hashes != approved_old => {
                return Err(conflict(
                    "previous maintenance pair is not administrator approved",
                ));
            }
            RollbackKind::Diagnostic
                if self.old_metadata.is_none() || self.old_hashes == approved_old =>
            {
                return Err(conflict(
                    "diagnostic rollback requires a damaged owned installation",
                ));
            }
            _ => {}
        }
        for index in 0..2 {
            if !self.allowed_hashes[index].contains(&self.new_hashes[index])
                || self.old_hashes[index]
                    .as_ref()
                    .is_some_and(|hash| !self.allowed_hashes[index].contains(hash))
            {
                return Err(conflict("unrecorded maintenance digest"));
            }
        }
        if !self.allowed_metadata.contains(&self.new_metadata)
            || self
                .old_metadata
                .as_ref()
                .is_some_and(|metadata| !self.allowed_metadata.contains(metadata))
        {
            return Err(conflict("unrecorded maintenance metadata"));
        }
        Ok(())
    }

    fn persist(&self, root: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(conflict("maintenance journal exceeds budget"));
        }
        atomic_bytes(root, RECORD, &bytes)
    }

    fn preflight(&self, root: &Path) -> io::Result<()> {
        for (index, active) in [helper_name(), core_name()].iter().enumerate() {
            if let Some(hash) = hash_optional(&root.join(DIAGNOSTICS[index]))?
                && (self.rollback_kind != RollbackKind::Diagnostic
                    || self.old_hashes[index].as_ref() != Some(&hash))
            {
                return Err(conflict("maintenance diagnostic has unrecorded digest"));
            }
            regular(&root.join(COMPONENTS[index].2))?;
            for name in [*active, COMPONENTS[index].0, COMPONENTS[index].1] {
                if hash_optional(&root.join(name))?
                    .as_ref()
                    .is_some_and(|hash| !self.allowed_hashes[index].contains(hash))
                {
                    return Err(conflict("maintenance artifact has unrecorded digest"));
                }
            }
        }
        for name in ["install.json", "metadata.previous"] {
            if metadata_optional(&root.join(name))?
                .as_ref()
                .is_some_and(|record| !self.allowed_metadata.contains(record))
            {
                return Err(conflict("maintenance metadata changed"));
            }
        }
        Ok(())
    }

    pub(crate) fn swap(&self, root: &Path) -> io::Result<()> {
        self.preflight(root)?;
        let sources = self.old_sources(root)?;
        // Copy before replacing any active file. Every old source remains
        // available through a crash at either component's atomic replacement.
        for (index, source) in sources.iter().enumerate() {
            let archive = if self.rollback_kind == RollbackKind::Diagnostic {
                DIAGNOSTICS[index]
            } else {
                COMPONENTS[index].1
            };
            if let Some(source) = source
                && source != &root.join(archive)
            {
                copy_recorded(
                    source,
                    &root.join(archive),
                    &root.join(COMPONENTS[index].2),
                    self.old_hashes[index].as_deref().unwrap(),
                    self.rollback_kind == RollbackKind::Diagnostic,
                )?;
            }
        }
        if self.rollback_kind == RollbackKind::ApprovedOrEmpty
            && let Some(metadata) = &self.old_metadata
        {
            crate::write_metadata_atomic(&root.join("metadata.previous"), metadata)
                .map_err(io::Error::other)?;
        }
        for (index, active) in [helper_name(), core_name()].iter().enumerate() {
            replace(&root.join(COMPONENTS[index].0), &root.join(active))?;
        }
        Ok(())
    }

    fn old_sources(&self, root: &Path) -> io::Result<[Option<PathBuf>; 2]> {
        let mut sources = [None, None];
        for (index, active) in [helper_name(), core_name()].iter().enumerate() {
            if let Some(expected) = &self.old_hashes[index] {
                let archive = if self.rollback_kind == RollbackKind::Diagnostic {
                    DIAGNOSTICS[index]
                } else {
                    COMPONENTS[index].1
                };
                for name in [*active, archive] {
                    let path = root.join(name);
                    if hash_optional(&path)?.as_ref() == Some(expected) {
                        sources[index] = Some(path);
                        break;
                    }
                }
                if sources[index].is_none() {
                    return Err(conflict("previous maintenance artifact is missing"));
                }
            }
        }
        Ok(sources)
    }

    pub(crate) fn recover_storage(&self, root: &Path) -> io::Result<()> {
        self.preflight(root)?;
        if self.phase == Phase::Committed {
            return self.verify_active(root, &self.new_hashes, Some(&self.new_metadata));
        }
        let sources = self.old_sources(root)?;
        for (index, active) in [helper_name(), core_name()].iter().enumerate() {
            if let Some(source) = &sources[index] {
                let target = root.join(active);
                if source != &target {
                    copy_recorded(
                        source,
                        &target,
                        &root.join(COMPONENTS[index].2),
                        self.old_hashes[index].as_deref().unwrap(),
                        self.rollback_kind == RollbackKind::Diagnostic,
                    )?;
                }
            } else {
                remove(&root.join(active))?;
            }
        }
        if let Some(metadata) = &self.old_metadata {
            crate::write_metadata_atomic(&root.join("install.json"), metadata)
                .map_err(io::Error::other)?;
        } else {
            remove(&root.join("install.json"))?;
        }
        Ok(())
    }

    pub(crate) fn validate_recovery(&self, root: &Path) -> io::Result<()> {
        self.preflight(root)?;
        if self.phase == Phase::Committed {
            self.verify_active(root, &self.new_hashes, Some(&self.new_metadata))
        } else {
            self.old_sources(root).map(|_| ())
        }
    }

    fn verify_active(
        &self,
        root: &Path,
        hashes: &[String; 2],
        metadata: Option<&InstalledMetadata>,
    ) -> io::Result<()> {
        for (index, name) in [helper_name(), core_name()].iter().enumerate() {
            if hash_optional(&root.join(name))?.as_ref() != Some(&hashes[index]) {
                return Err(conflict("committed artifact missing or changed"));
            }
        }
        if metadata_optional(&root.join("install.json"))?.as_ref() != metadata {
            return Err(conflict("committed metadata missing or changed"));
        }
        Ok(())
    }

    pub(crate) fn commit(&mut self, root: &Path) -> io::Result<()> {
        self.preflight(root)?;
        self.verify_active(root, &self.new_hashes, Some(&self.new_metadata))?;
        self.phase = Phase::Committed;
        self.persist(root)
    }
    pub(crate) fn new_metadata(&self) -> &InstalledMetadata {
        &self.new_metadata
    }
    pub(crate) fn recovery_metadata(&self) -> Option<&InstalledMetadata> {
        if self.phase == Phase::Committed {
            Some(&self.new_metadata)
        } else if self.rollback_kind == RollbackKind::Diagnostic {
            None
        } else {
            self.old_metadata.as_ref()
        }
    }
    pub(crate) fn restore_running(&self) -> bool {
        self.phase == Phase::Committed
            || (self.rollback_kind == RollbackKind::ApprovedOrEmpty && self.was_running)
    }
    pub(crate) fn finish(&self, root: &Path) -> io::Result<()> {
        self.preflight(root)?;
        if self.phase == Phase::Committed {
            self.verify_active(root, &self.new_hashes, Some(&self.new_metadata))?;
        } else {
            for (index, name) in [helper_name(), core_name()].iter().enumerate() {
                if hash_optional(&root.join(name))? != self.old_hashes[index] {
                    return Err(conflict("previous artifact not restored"));
                }
            }
            if metadata_optional(&root.join("install.json"))?.as_ref() != self.old_metadata.as_ref()
            {
                return Err(conflict("previous metadata not restored"));
            }
        }
        for (stage, _, restore) in COMPONENTS {
            remove(&root.join(stage))?;
            remove(&root.join(restore))?;
        }
        if self.rollback_kind == RollbackKind::Diagnostic {
            for name in DIAGNOSTICS {
                remove(&root.join(name))?;
            }
        }
        remove(&root.join(RECORD))
    }
}

fn approved_pair(root: &Path, names: [&str; 2], metadata: &InstalledMetadata) -> io::Result<bool> {
    for (name, expected) in names
        .into_iter()
        .zip([metadata.helper_sha256(), metadata.core_sha256()])
    {
        let path = root.join(name);
        if regular(&path)?.is_none_or(|file| file.len() == 0)
            || hash_optional(&path)?.as_deref() != Some(expected)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn helper_name() -> &'static str {
    if cfg!(windows) {
        "zenclash-service.exe"
    } else {
        "zenclash-service"
    }
}
fn core_name() -> &'static str {
    if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    }
}
fn conflict(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn regular(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(conflict("maintenance reparse point rejected"));
                }
            }
            Ok(Some(metadata))
        }
        Ok(_) => Err(conflict("maintenance artifact must be a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
fn hash_optional(path: &Path) -> io::Result<Option<String>> {
    let Some(metadata) = regular(path)? else {
        return Ok(None);
    };
    if metadata.len() > MAX_ARTIFACT {
        return Err(conflict("maintenance artifact exceeds budget"));
    }
    let mut reader = File::open(path)?.take(MAX_ARTIFACT + 1);
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut count = 0;
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        hash.update(&buffer[..n]);
    }
    if count > MAX_ARTIFACT {
        return Err(conflict("maintenance artifact exceeds budget"));
    }
    Ok(Some(format!("{:x}", hash.finalize())))
}
fn metadata_optional(path: &Path) -> io::Result<Option<InstalledMetadata>> {
    if regular(path)?.is_none() {
        Ok(None)
    } else {
        crate::read_metadata(path)
            .map(Some)
            .map_err(io::Error::other)
    }
}
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o700);
    }
    options
}
fn copy_recorded(
    source: &Path,
    target: &Path,
    temporary: &Path,
    expected: &str,
    allow_empty: bool,
) -> io::Result<()> {
    // The exclusive temporary is never authoritative. A crash can leave a
    // prefix here; callers have already verified both complete old sources.
    remove(temporary)?;
    let mut input = File::open(source)?.take(MAX_ARTIFACT + 1);
    let mut output = private_options().open(temporary)?;
    let count = io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    drop(output);
    if (!allow_empty && count == 0)
        || count > MAX_ARTIFACT
        || hash_optional(temporary)?.as_deref() != Some(expected)
    {
        return Err(conflict("recovery copy digest changed"));
    }
    replace(temporary, target)
}
fn atomic_bytes(root: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let temporary = root.join("maintenance.new");
    // An interrupted pre-rename journal write contains no artifact mutation.
    // A complete existing record remains authoritative until atomic replace.
    if regular(&temporary)?.is_some() {
        remove(&temporary)?;
    }
    let mut output = private_options().open(&temporary)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    drop(output);
    replace(&temporary, &root.join(name))
}
fn remove(path: &Path) -> io::Result<()> {
    if regular(path)?.is_some() {
        fs::remove_file(path)?;
        sync_parent(path)?;
    }
    Ok(())
}
#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(
        path.parent()
            .ok_or_else(|| conflict("maintenance path has no parent"))?,
    )?
    .sync_all()
}
#[cfg(windows)]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}
#[cfg(unix)]
fn replace(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)?;
    sync_parent(target)
}
#[cfg(windows)]
fn replace(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both owned UTF-16 paths are NUL-terminated for this call.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Self {
            let mut nonce = [0; 8];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir()
                .join(format!("zenclash-journal-{:x}", u64::from_le_bytes(nonce)));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            for file in std::fs::read_dir(&self.0).unwrap() {
                std::fs::remove_file(file.unwrap().path()).unwrap();
            }
            std::fs::remove_dir(&self.0).unwrap();
        }
    }

    fn fixture(root: &std::path::Path) -> Journal {
        fs::write(root.join(helper_name()), b"old helper").unwrap();
        fs::write(root.join(core_name()), b"old core").unwrap();
        let old = crate::InstalledMetadata::new(
            digest(b"old core"),
            digest(b"old helper"),
            "1000".into(),
        )
        .unwrap();
        crate::write_metadata_atomic(&root.join("install.json"), &old).unwrap();
        fs::write(root.join("helper.new"), b"new helper").unwrap();
        fs::write(root.join("core.new"), b"new core").unwrap();
        let new = crate::InstalledMetadata::new(
            digest(b"new core"),
            digest(b"new helper"),
            "1000".into(),
        )
        .unwrap();
        Journal::prepare(root, &new, &digest(b"new helper"), true).unwrap()
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", sha2::Sha256::digest(bytes))
    }

    fn damaged_fixture(
        root: &Path,
        helper: Option<&[u8]>,
        core: Option<&[u8]>,
    ) -> InstalledMetadata {
        for (name, bytes) in [(helper_name(), helper), (core_name(), core)] {
            if let Some(bytes) = bytes {
                fs::write(root.join(name), bytes).unwrap();
            }
        }
        let old = InstalledMetadata::new(digest(b"old core"), digest(b"old helper"), "1000".into())
            .unwrap();
        crate::write_metadata_atomic(&root.join("install.json"), &old).unwrap();
        fs::write(root.join("helper.new"), b"new helper").unwrap();
        fs::write(root.join("core.new"), b"new core").unwrap();
        old
    }

    fn repaired_metadata() -> InstalledMetadata {
        InstalledMetadata::new(digest(b"new core"), digest(b"new helper"), "1000".into()).unwrap()
    }

    #[test]
    fn repair_without_an_approved_archive_restores_diagnostics_without_restart_authority() {
        let cases = [
            (Some(b"bad helper".as_slice()), Some(b"old core".as_slice())),
            (None, Some(b"old core")),
            (Some(b""), Some(b"old core")),
            (Some(b"old helper"), None),
            (Some(b"old helper"), Some(b"")),
            (None, None),
        ];
        for (helper, core) in cases {
            for running in [false, true] {
                let root = Directory::new();
                let old = damaged_fixture(&root.0, helper, core);
                let journal = Journal::prepare_repair(
                    &root.0,
                    &repaired_metadata(),
                    &digest(b"new helper"),
                    running,
                )
                .unwrap();
                journal.swap(&root.0).unwrap();
                crate::write_metadata_atomic(&root.0.join("install.json"), journal.new_metadata())
                    .unwrap();
                let journal = Journal::load(&root.0).unwrap().unwrap();
                assert!(
                    journal.recovery_metadata().is_none(),
                    "damaged files must never authorize native registration"
                );
                assert!(
                    !journal.restore_running(),
                    "observed Running cannot authorize damaged bytes"
                );
                journal.recover_storage(&root.0).unwrap();
                journal.recover_storage(&root.0).unwrap();
                for (name, bytes) in [(helper_name(), helper), (core_name(), core)] {
                    assert_eq!(fs::read(root.0.join(name)).ok().as_deref(), bytes);
                }
                assert_eq!(
                    crate::read_metadata(&root.0.join("install.json")).unwrap(),
                    old
                );
                journal.finish(&root.0).unwrap();
                assert!(!root.0.join("helper.previous").exists());
                assert!(!root.0.join("core.previous").exists());
            }
        }
    }

    #[test]
    fn committed_repair_without_an_archive_retains_only_the_approved_new_pair() {
        let root = Directory::new();
        damaged_fixture(&root.0, Some(b""), None);
        let new = repaired_metadata();
        let mut journal =
            Journal::prepare_repair(&root.0, &new, &digest(b"new helper"), true).unwrap();
        journal.swap(&root.0).unwrap();
        crate::write_metadata_atomic(&root.0.join("install.json"), &new).unwrap();
        journal.commit(&root.0).unwrap();
        let journal = Journal::load(&root.0).unwrap().unwrap();
        journal.recover_storage(&root.0).unwrap();
        assert_eq!(journal.recovery_metadata(), Some(&new));
        assert!(journal.restore_running());
        journal.finish(&root.0).unwrap();
        assert_eq!(fs::read(root.0.join(helper_name())).unwrap(), b"new helper");
        assert_eq!(fs::read(root.0.join(core_name())).unwrap(), b"new core");
    }

    #[test]
    fn unknown_diagnostic_replacement_preserves_all_recovery_evidence() {
        let root = Directory::new();
        damaged_fixture(&root.0, Some(b"bad helper"), None);
        let journal =
            Journal::prepare_repair(&root.0, &repaired_metadata(), &digest(b"new helper"), false)
                .unwrap();
        journal.swap(&root.0).unwrap();
        fs::write(root.0.join("helper.diagnostic"), b"unrecorded diagnostic").unwrap();
        assert!(journal.recover_storage(&root.0).is_err());
        assert!(journal.finish(&root.0).is_err());
        assert_eq!(
            fs::read(root.0.join("helper.diagnostic")).unwrap(),
            b"unrecorded diagnostic"
        );
        assert_eq!(fs::read(root.0.join(helper_name())).unwrap(), b"new helper");
        assert!(root.0.join(RECORD).exists());
    }

    #[test]
    fn each_interrupted_diagnostic_swap_restores_original_bytes_and_missing_files() {
        for interruption in 0..=5 {
            let root = Directory::new();
            let old = damaged_fixture(&root.0, Some(b""), Some(b"bad core"));
            let journal = Journal::prepare_repair(
                &root.0,
                &repaired_metadata(),
                &digest(b"new helper"),
                true,
            )
            .unwrap();
            if interruption >= 1 {
                fs::copy(root.0.join(helper_name()), root.0.join("helper.diagnostic")).unwrap();
            }
            if interruption >= 2 {
                fs::copy(root.0.join(core_name()), root.0.join("core.diagnostic")).unwrap();
            }
            if interruption >= 3 {
                replace(&root.0.join("helper.new"), &root.0.join(helper_name())).unwrap();
            }
            if interruption >= 4 {
                replace(&root.0.join("core.new"), &root.0.join(core_name())).unwrap();
            }
            if interruption >= 5 {
                crate::write_metadata_atomic(&root.0.join("install.json"), journal.new_metadata())
                    .unwrap();
            }
            let journal = Journal::load(&root.0).unwrap().unwrap();
            journal.validate_recovery(&root.0).unwrap();
            journal.recover_storage(&root.0).unwrap();
            journal.recover_storage(&root.0).unwrap();
            assert_eq!(fs::read(root.0.join(helper_name())).unwrap(), b"");
            assert_eq!(fs::read(root.0.join(core_name())).unwrap(), b"bad core");
            assert_eq!(
                crate::read_metadata(&root.0.join("install.json")).unwrap(),
                old
            );
            assert!(journal.recovery_metadata().is_none());
            assert!(!journal.restore_running());
            journal.finish(&root.0).unwrap();
            assert!(!root.0.join(RECORD).exists());
        }
    }

    #[test]
    fn schema_two_requires_a_rollback_kind_and_legacy_one_recovers_strictly() {
        let root = Directory::new();
        let journal = fixture(&root.0);
        journal.swap(&root.0).unwrap();
        let path = root.0.join(RECORD);
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(record["schema_version"], 2);
        record.as_object_mut().unwrap().remove("rollback_kind");
        let incomplete = serde_json::to_vec(&record).unwrap();
        fs::write(&path, &incomplete).unwrap();
        assert!(Journal::load(&root.0).is_err());
        assert_eq!(fs::read(&path).unwrap(), incomplete);
        record["schema_version"] = serde_json::json!(1);
        fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        let legacy = Journal::load(&root.0).unwrap().unwrap();
        legacy.recover_storage(&root.0).unwrap();
        assert_eq!(fs::read(root.0.join(helper_name())).unwrap(), b"old helper");
        assert_eq!(fs::read(root.0.join(core_name())).unwrap(), b"old core");
        legacy.finish(&root.0).unwrap();
    }

    #[test]
    fn duplicate_fields_are_rejected_in_both_legacy_and_current_journals() {
        for schema in [1, 2] {
            let root = Directory::new();
            fixture(&root.0);
            let path = root.0.join(RECORD);
            let mut record: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            record["schema_version"] = serde_json::json!(schema);
            if schema == 1 {
                record.as_object_mut().unwrap().remove("rollback_kind");
            }
            let bytes = serde_json::to_string(&record).unwrap().replace(
                "\"phase\":\"prepared\"",
                "\"phase\":\"prepared\",\"phase\":\"prepared\"",
            );
            fs::write(&path, bytes.as_bytes()).unwrap();
            assert!(Journal::load(&root.0).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
            assert_eq!(fs::read(root.0.join(helper_name())).unwrap(), b"old helper");
        }
    }

    #[test]
    fn oversized_old_diagnostics_are_rejected_before_any_artifact_changes() {
        let root = Directory::new();
        damaged_fixture(&root.0, None, Some(b"old core"));
        File::create(root.0.join(helper_name()))
            .unwrap()
            .set_len(MAX_ARTIFACT + 1)
            .unwrap();
        assert!(
            Journal::prepare_repair(&root.0, &repaired_metadata(), &digest(b"new helper"), false)
                .is_err()
        );
        assert!(!root.0.join(RECORD).exists());
        assert_eq!(
            fs::metadata(root.0.join(helper_name())).unwrap().len(),
            MAX_ARTIFACT + 1
        );
        assert_eq!(fs::read(root.0.join("helper.new")).unwrap(), b"new helper");
    }

    #[test]
    fn repair_requires_known_metadata_and_never_adopts_an_unowned_installation() {
        for metadata in [None, Some(b"{\"schema_version\":999}".as_slice())] {
            let root = Directory::new();
            fs::write(root.0.join("helper.new"), b"new helper").unwrap();
            fs::write(root.0.join("core.new"), b"new core").unwrap();
            if let Some(bytes) = metadata {
                fs::write(root.0.join("install.json"), bytes).unwrap();
            }
            assert!(
                Journal::prepare_repair(
                    &root.0,
                    &repaired_metadata(),
                    &digest(b"new helper"),
                    false
                )
                .is_err()
            );
            assert_eq!(
                fs::read(root.0.join("install.json")).ok().as_deref(),
                metadata
            );
            assert!(!root.0.join(RECORD).exists());
            assert!(!root.0.join(helper_name()).exists());
        }
    }

    #[test]
    fn a_legacy_record_cannot_authorize_diagnostic_rollback() {
        let root = Directory::new();
        damaged_fixture(&root.0, Some(b"bad helper"), None);
        Journal::prepare_repair(&root.0, &repaired_metadata(), &digest(b"new helper"), true)
            .unwrap();
        let path = root.0.join(RECORD);
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["schema_version"] = serde_json::json!(1);
        record.as_object_mut().unwrap().remove("rollback_kind");
        let bytes = serde_json::to_vec(&record).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(Journal::load(&root.0).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read(root.0.join(helper_name())).unwrap(), b"bad helper");
        assert!(!root.0.join(core_name()).exists());
    }

    #[test]
    fn diagnostic_cleanup_failure_preserves_the_journal_and_original_bytes() {
        let root = Directory::new();
        damaged_fixture(&root.0, Some(b"bad helper"), None);
        let journal =
            Journal::prepare_repair(&root.0, &repaired_metadata(), &digest(b"new helper"), false)
                .unwrap();
        journal.swap(&root.0).unwrap();
        journal.recover_storage(&root.0).unwrap();
        fs::create_dir(root.0.join("core.restore")).unwrap();
        assert!(journal.finish(&root.0).is_err());
        assert!(root.0.join(RECORD).exists());
        assert_eq!(
            fs::read(root.0.join("helper.diagnostic")).unwrap(),
            b"bad helper"
        );
        assert_eq!(fs::read(root.0.join(helper_name())).unwrap(), b"bad helper");
        assert!(!root.0.join(core_name()).exists());
        fs::remove_dir(root.0.join("core.restore")).unwrap();
        journal.recover_storage(&root.0).unwrap();
        journal.finish(&root.0).unwrap();
    }

    #[test]
    fn diagnostic_repair_never_overwrites_an_incomplete_previous_archive() {
        let root = Directory::new();
        damaged_fixture(&root.0, Some(b""), None);
        fs::write(root.0.join("helper.previous"), b"damaged archive helper").unwrap();
        fs::write(root.0.join("core.previous"), b"old core").unwrap();
        let old = crate::read_metadata(&root.0.join("install.json")).unwrap();
        crate::write_metadata_atomic(&root.0.join("metadata.previous"), &old).unwrap();
        let journal =
            Journal::prepare_repair(&root.0, &repaired_metadata(), &digest(b"new helper"), true)
                .unwrap();
        journal.swap(&root.0).unwrap();
        journal.recover_storage(&root.0).unwrap();
        journal.finish(&root.0).unwrap();
        assert_eq!(
            fs::read(root.0.join("helper.previous")).unwrap(),
            b"damaged archive helper"
        );
        assert_eq!(fs::read(root.0.join("core.previous")).unwrap(), b"old core");
        assert_eq!(
            crate::read_metadata(&root.0.join("metadata.previous")).unwrap(),
            old
        );
    }

    #[test]
    fn every_interrupted_swap_restores_one_complete_previous_installation() {
        for interruption in 0..=5 {
            let root = Directory::new();
            let journal = fixture(&root.0);
            if interruption >= 1 {
                fs::rename(root.0.join(helper_name()), root.0.join("helper.previous")).unwrap();
            }
            if interruption >= 2 {
                fs::rename(root.0.join(core_name()), root.0.join("core.previous")).unwrap();
            }
            if interruption >= 3 {
                fs::rename(root.0.join("helper.new"), root.0.join(helper_name())).unwrap();
            }
            if interruption >= 4 {
                fs::rename(root.0.join("core.new"), root.0.join(core_name())).unwrap();
            }
            if interruption >= 5 {
                crate::write_metadata_atomic(&root.0.join("install.json"), journal.new_metadata())
                    .unwrap();
            }
            let recovered = Journal::load(&root.0).unwrap().unwrap();
            recovered.recover_storage(&root.0).unwrap();
            recovered.recover_storage(&root.0).unwrap();
            assert_eq!(fs::read(root.0.join(helper_name())).unwrap(), b"old helper");
            assert_eq!(fs::read(root.0.join(core_name())).unwrap(), b"old core");
            assert_eq!(
                crate::read_metadata(&root.0.join("install.json"))
                    .unwrap()
                    .core_sha256(),
                digest(b"old core")
            );
            assert!(recovered.restore_running());
            recovered.finish(&root.0).unwrap();
            assert!(Journal::load(&root.0).unwrap().is_none());
        }
    }

    #[test]
    fn committed_recovery_preserves_the_new_approved_pair() {
        let root = Directory::new();
        let mut journal = fixture(&root.0);
        journal.swap(&root.0).unwrap();
        crate::write_metadata_atomic(&root.0.join("install.json"), journal.new_metadata()).unwrap();
        journal.commit(&root.0).unwrap();
        let recovered = Journal::load(&root.0).unwrap().unwrap();
        recovered.recover_storage(&root.0).unwrap();
        recovered.finish(&root.0).unwrap();
        assert_eq!(fs::read(root.0.join(core_name())).unwrap(), b"new core");
        assert_eq!(fs::read(root.0.join("core.previous")).unwrap(), b"old core");
    }

    #[test]
    fn damaged_or_missing_components_never_replace_the_complete_approved_archive() {
        let cases: [(Option<&[u8]>, &[u8]); 4] = [
            (Some(b"damaged helper"), b"old core"),
            (None, b"old core"),
            (Some(b""), b"old core"),
            (Some(b"old helper"), b""),
        ];
        for (helper, core) in cases {
            let root = Directory::new();
            if let Some(helper) = helper {
                fs::write(root.0.join(helper_name()), helper).unwrap();
            }
            fs::write(root.0.join(core_name()), core).unwrap();
            fs::write(root.0.join("helper.previous"), b"old helper").unwrap();
            fs::write(root.0.join("core.previous"), b"old core").unwrap();
            let old =
                InstalledMetadata::new(digest(b"old core"), digest(b"old helper"), "1000".into())
                    .unwrap();
            crate::write_metadata_atomic(&root.0.join("install.json"), &old).unwrap();
            crate::write_metadata_atomic(&root.0.join("metadata.previous"), &old).unwrap();
            fs::write(root.0.join("helper.new"), b"new helper").unwrap();
            fs::write(root.0.join("core.new"), b"new core").unwrap();
            let new =
                InstalledMetadata::new(digest(b"new core"), digest(b"new helper"), "1000".into())
                    .unwrap();
            let journal = Journal::prepare(&root.0, &new, &digest(b"new helper"), false).unwrap();
            journal.swap(&root.0).unwrap();
            assert_eq!(
                fs::read(root.0.join("helper.previous")).unwrap(),
                b"old helper"
            );
            assert_eq!(fs::read(root.0.join("core.previous")).unwrap(), b"old core");
            let recovered = Journal::load(&root.0).unwrap().unwrap();
            recovered.recover_storage(&root.0).unwrap();
            recovered.recover_storage(&root.0).unwrap();
            recovered.finish(&root.0).unwrap();
            assert_eq!(fs::read(root.0.join(helper_name())).unwrap(), b"old helper");
            assert_eq!(fs::read(root.0.join(core_name())).unwrap(), b"old core");
            assert_eq!(
                crate::read_metadata(&root.0.join("install.json")).unwrap(),
                old
            );
        }
    }

    #[test]
    fn damaged_archive_helper_blocks_repair_without_changing_diagnostic_bytes() {
        let root = Directory::new();
        fs::write(root.0.join(helper_name()), b"").unwrap();
        fs::write(root.0.join(core_name()), b"old core").unwrap();
        fs::write(root.0.join("helper.previous"), b"damaged archive").unwrap();
        fs::write(root.0.join("core.previous"), b"old core").unwrap();
        let old = InstalledMetadata::new(digest(b"old core"), digest(b"old helper"), "1000".into())
            .unwrap();
        crate::write_metadata_atomic(&root.0.join("install.json"), &old).unwrap();
        crate::write_metadata_atomic(&root.0.join("metadata.previous"), &old).unwrap();
        fs::write(root.0.join("helper.new"), b"new helper").unwrap();
        fs::write(root.0.join("core.new"), b"new core").unwrap();
        let new = InstalledMetadata::new(digest(b"new core"), digest(b"new helper"), "1000".into())
            .unwrap();
        let error = Journal::prepare(&root.0, &new, &digest(b"new helper"), false)
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("administrator recovery is required")
        );
        assert_eq!(fs::read(root.0.join(helper_name())).unwrap(), b"");
        assert_eq!(
            fs::read(root.0.join("helper.previous")).unwrap(),
            b"damaged archive"
        );
        assert!(!root.0.join(RECORD).exists());
    }

    #[test]
    fn empty_new_artifacts_are_never_approved_by_the_journal() {
        let root = Directory::new();
        fs::write(root.0.join("helper.new"), b"new helper").unwrap();
        fs::write(root.0.join("core.new"), b"").unwrap();
        let new =
            InstalledMetadata::new(digest(b""), digest(b"new helper"), "1000".into()).unwrap();
        assert!(Journal::prepare(&root.0, &new, &digest(b"new helper"), false).is_err());
        assert!(!root.0.join(RECORD).exists());
        assert!(!root.0.join(helper_name()).exists());
    }

    #[test]
    fn unknown_journal_schema_is_preserved_without_touching_artifacts() {
        let root = Directory::new();
        fixture(&root.0);
        let path = root.0.join("maintenance.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["schema_version"] = serde_json::json!(3);
        let unknown = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &unknown).unwrap();
        assert!(Journal::load(&root.0).is_err());
        assert_eq!(fs::read(path).unwrap(), unknown);
        assert_eq!(fs::read(root.0.join(core_name())).unwrap(), b"old core");
    }

    #[test]
    fn unrecorded_artifact_replacement_blocks_recovery_without_deleting_diagnostics() {
        let root = Directory::new();
        let journal = fixture(&root.0);
        journal.swap(&root.0).unwrap();
        fs::write(root.0.join("core.previous"), b"unapproved bytes").unwrap();
        assert!(journal.recover_storage(&root.0).is_err());
        assert_eq!(fs::read(root.0.join(core_name())).unwrap(), b"new core");
        assert_eq!(
            fs::read(root.0.join("core.previous")).unwrap(),
            b"unapproved bytes"
        );
        assert!(root.0.join("maintenance.json").exists());
    }

    #[test]
    fn interrupted_first_install_removes_only_its_recorded_new_artifacts() {
        let root = Directory::new();
        fs::write(root.0.join("helper.new"), b"new helper").unwrap();
        fs::write(root.0.join("core.new"), b"new core").unwrap();
        fs::write(root.0.join("unrelated"), b"keep").unwrap();
        let new = crate::InstalledMetadata::new(
            digest(b"new core"),
            digest(b"new helper"),
            "1000".into(),
        )
        .unwrap();
        let journal = Journal::prepare(&root.0, &new, &digest(b"new helper"), false).unwrap();
        journal.swap(&root.0).unwrap();
        journal.recover_storage(&root.0).unwrap();
        journal.finish(&root.0).unwrap();
        assert!(!root.0.join(helper_name()).exists());
        assert!(!root.0.join(core_name()).exists());
        assert_eq!(fs::read(root.0.join("unrelated")).unwrap(), b"keep");
        assert!(!journal.restore_running());
    }

    #[test]
    fn interrupted_recovery_copy_is_discarded_only_when_complete_old_sources_exist() {
        let root = Directory::new();
        let journal = fixture(&root.0);
        journal.swap(&root.0).unwrap();
        fs::write(root.0.join("helper.restore"), b"partial").unwrap();
        journal.recover_storage(&root.0).unwrap();
        fs::write(root.0.join("core.restore"), b"partial").unwrap();
        journal.recover_storage(&root.0).unwrap();
        journal.finish(&root.0).unwrap();
        assert_eq!(fs::read(root.0.join(core_name())).unwrap(), b"old core");
        assert!(!root.0.join("core.restore").exists());
    }

    #[test]
    fn stopped_installation_stays_stopped_after_recovery() {
        let root = Directory::new();
        let mut journal = fixture(&root.0);
        journal.was_running = false;
        journal.persist(&root.0).unwrap();
        journal.swap(&root.0).unwrap();
        let recovered = Journal::load(&root.0).unwrap().unwrap();
        assert!(!recovered.restore_running());
        assert!(recovered.recovery_metadata().is_some());
        recovered.validate_recovery(&root.0).unwrap();
        recovered.recover_storage(&root.0).unwrap();
        recovered.finish(&root.0).unwrap();
    }

    #[test]
    fn repairing_corrupt_current_keeps_the_complete_approved_archive() {
        let root = Directory::new();
        fs::write(root.0.join(helper_name()), b"bad helper").unwrap();
        fs::write(root.0.join(core_name()), b"bad core").unwrap();
        fs::write(root.0.join("helper.previous"), b"old helper").unwrap();
        fs::write(root.0.join("core.previous"), b"old core").unwrap();
        let old = crate::InstalledMetadata::new(
            digest(b"old core"),
            digest(b"old helper"),
            "1000".into(),
        )
        .unwrap();
        crate::write_metadata_atomic(&root.0.join("install.json"), &old).unwrap();
        crate::write_metadata_atomic(&root.0.join("metadata.previous"), &old).unwrap();
        fs::write(root.0.join("helper.new"), b"new helper").unwrap();
        fs::write(root.0.join("core.new"), b"new core").unwrap();
        let new = crate::InstalledMetadata::new(
            digest(b"new core"),
            digest(b"new helper"),
            "1000".into(),
        )
        .unwrap();
        let journal = Journal::prepare(&root.0, &new, &digest(b"new helper"), false).unwrap();
        journal.swap(&root.0).unwrap();
        journal.recover_storage(&root.0).unwrap();
        journal.finish(&root.0).unwrap();
        assert_eq!(fs::read(root.0.join(core_name())).unwrap(), b"old core");
        assert_eq!(
            crate::read_metadata(&root.0.join("install.json")).unwrap(),
            old
        );
        assert_eq!(fs::read(root.0.join("core.previous")).unwrap(), b"old core");
    }
}
