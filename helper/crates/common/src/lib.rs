#![cfg_attr(test, allow(clippy::unwrap_used))]
pub mod config;
pub mod documents;
pub use config::{Settings, SharedSettings, normalize_speech_endpoint, persist_azure_settings};
pub use documents::{
    build_document, export_filename, extract_description_links, normalize_description_links,
    publication_date, render_document, write_exports,
};
use serde_json::Value;
use std::{fmt, path::Path};
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MODEL: &str = "MAI-Transcribe-2";
pub const API_VERSION: &str = "2025-10-15";
#[derive(Debug, Clone)]
pub struct Error {
    pub status: u16,
    pub message: String,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(409, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, message)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::internal("The local file operation could not be completed.")
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::bad_request("The saved JSON document could not be read.")
    }
}
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
pub fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&local_io::read(path)?)?)
}
/// Bounded, descriptor-safe reads for comparing derived files. Missing is
/// distinct from an unsafe, changed or unreadable file.
pub fn read_bytes_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match local_io::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
pub fn read_json_optional(path: &Path) -> Result<Option<Value>> {
    match local_io::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
pub fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}
/// Create a private, short-lived input that will be consumed immediately and
/// discarded after interruption. This deliberately omits durable syncs and
/// must never be used for transcripts, job records, or settings.
pub fn create_transient_json(path: &Path, value: &Value) -> Result<()> {
    Ok(local_io::create_transient(
        path,
        &serde_json::to_vec(value)?,
    )?)
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    Ok(local_io::write(path, bytes)?)
}
pub(crate) fn atomic_batch(directory: &Path, files: &[(&str, &[u8])]) -> Result<()> {
    local_io::write_batch(directory, files).map_err(|_| {
        Error::internal("The transcript files could not be committed completely. Check local storage before retrying.")
    })
}

/// Files are reached through owned directory descriptors, never a canonicalize
/// followed by a path-based open. All caller-facing errors remain path-free.
#[cfg(unix)]
mod local_io {
    use std::{
        ffi::{CString, OsStr, OsString},
        fs::{File, Metadata},
        io::{self, Read, Seek, SeekFrom, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::MetadataExt},
        },
        path::{Component, Path, PathBuf},
    };
    const LIMIT: u64 = 64 * 1024 * 1024;
    type Signature = (u64, u64, u64, i64, i64, i64, i64, u32);
    fn unsafe_path() -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, "Unsafe or changed local file")
    }
    fn c(value: &OsStr) -> io::Result<CString> {
        CString::new(value.as_bytes()).map_err(|_| unsafe_path())
    }
    fn identity(value: &Metadata) -> (u64, u64) {
        (value.dev(), value.ino())
    }
    fn signature(value: &Metadata) -> Signature {
        (
            value.dev(),
            value.ino(),
            value.len(),
            value.mtime(),
            value.mtime_nsec(),
            value.ctime(),
            value.ctime_nsec(),
            value.mode(),
        )
    }
    fn root() -> io::Result<File> {
        let fd = unsafe {
            libc::open(
                c(OsStr::new("/"))?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    fn child(parent: &File, name: &OsStr) -> io::Result<File> {
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                c(name)?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    fn absolute(path: &Path) -> io::Result<PathBuf> {
        let value = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        if value
            .components()
            .any(|p| matches!(p, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(unsafe_path());
        }
        // macOS exposes these three root-owned compatibility aliases. Translate
        // only the exact immutable OS link, never arbitrary user path symlinks.
        #[cfg(target_os = "macos")]
        for alias in ["var", "tmp", "etc"] {
            let alias_path = Path::new("/").join(alias);
            if let Ok(suffix) = value.strip_prefix(&alias_path) {
                let before = std::fs::symlink_metadata(&alias_path)?;
                if before.file_type().is_symlink() {
                    let expected = Path::new("private").join(alias);
                    let target = std::fs::read_link(&alias_path)?;
                    if before.uid() != 0
                        || (target != expected && target != Path::new("/").join(&expected))
                        || signature(&before) != signature(&std::fs::symlink_metadata(&alias_path)?)
                    {
                        return Err(unsafe_path());
                    }
                    return Ok(Path::new("/").join(expected).join(suffix));
                }
            }
        }
        Ok(value)
    }
    struct Parent {
        directory: File,
        #[cfg(test)]
        path: PathBuf,
        name: OsString,
        ancestors: Vec<(OsString, (u64, u64))>,
        root: (u64, u64),
    }
    impl Parent {
        fn open(path: &Path, create: bool) -> io::Result<Self> {
            let path = absolute(path)?;
            let name = path.file_name().ok_or_else(unsafe_path)?.to_owned();
            let parent = path.parent().ok_or_else(unsafe_path)?;
            let mut directory = root()?;
            let root_id = identity(&directory.metadata()?);
            let mut ancestors = vec![];
            for component in parent.components() {
                let part = match component {
                    Component::RootDir | Component::CurDir => continue,
                    Component::Normal(part) => part,
                    _ => return Err(unsafe_path()),
                };
                let next = match child(&directory, part) {
                    Ok(value) => value,
                    Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                        let result = unsafe {
                            libc::mkdirat(directory.as_raw_fd(), c(part)?.as_ptr(), 0o700)
                        };
                        if result < 0
                            && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                        {
                            return Err(io::Error::last_os_error());
                        }
                        let value = child(&directory, part)?;
                        directory.sync_all()?;
                        value
                    }
                    Err(error) => return Err(error),
                };
                ancestors.push((part.to_owned(), identity(&next.metadata()?)));
                directory = next;
            }
            let result = Self {
                directory,
                #[cfg(test)]
                path,
                name,
                ancestors,
                root: root_id,
            };
            result.current()?;
            Ok(result)
        }
        fn current(&self) -> io::Result<()> {
            let mut directory = root()?;
            if identity(&directory.metadata()?) != self.root {
                return Err(unsafe_path());
            }
            for (name, expected) in &self.ancestors {
                directory = child(&directory, name)?;
                if identity(&directory.metadata()?) != *expected {
                    return Err(unsafe_path());
                }
            }
            if identity(&directory.metadata()?) != identity(&self.directory.metadata()?) {
                return Err(unsafe_path());
            }
            Ok(())
        }
        fn open_file(&self, name: &OsStr, flags: i32) -> io::Result<File> {
            let fd = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    c(name)?.as_ptr(),
                    flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(unsafe { File::from_raw_fd(fd) })
            }
        }
        fn existing(&self, name: &OsStr) -> io::Result<Option<Metadata>> {
            match self.open_file(name, libc::O_RDONLY) {
                Ok(file) => {
                    let info = file.metadata()?;
                    if !info.is_file() {
                        return Err(unsafe_path());
                    }
                    Ok(Some(info))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        }
        fn rename(&self, old: &OsStr, new: &OsStr) -> io::Result<()> {
            if unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    c(old)?.as_ptr(),
                    self.directory.as_raw_fd(),
                    c(new)?.as_ptr(),
                )
            } < 0
            {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        fn unlink(&self, name: &OsStr) -> io::Result<()> {
            if unsafe { libc::unlinkat(self.directory.as_raw_fd(), c(name)?.as_ptr(), 0) } < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        fn discard(&self, name: &OsStr, owned: (u64, u64)) {
            let stage = OsString::from(format!(
                ".scribe-discard-{}.tmp",
                uuid::Uuid::new_v4().simple()
            ));
            if self.rename(name, &stage).is_err() {
                return;
            }
            if self
                .existing(&stage)
                .is_ok_and(|v| v.is_some_and(|v| identity(&v) == owned))
            {
                let _ = self.unlink(&stage);
            } else if let (Ok(from), Ok(to)) = (c(&stage), c(name)) {
                // A different file must survive a cleanup race. linkat refuses
                // an occupied original name; keep the detached file if needed.
                if unsafe {
                    libc::linkat(
                        self.directory.as_raw_fd(),
                        from.as_ptr(),
                        self.directory.as_raw_fd(),
                        to.as_ptr(),
                        0,
                    )
                } == 0
                {
                    let _ = self.unlink(&stage);
                }
            }
        }
    }
    #[cfg(test)]
    type Hook = Box<dyn FnMut(&str, &Path)>;
    #[cfg(test)]
    thread_local! {pub(super) static HOOK:std::cell::RefCell<Option<Hook>>=const{std::cell::RefCell::new(None)};}
    #[cfg(test)]
    fn hook(stage: &str, path: &Path) {
        HOOK.with(|h| {
            if let Some(h) = h.borrow_mut().as_mut() {
                h(stage, path);
            }
        });
    }
    #[cfg(test)]
    thread_local! {
        pub(super) static BATCH_SYNC_FAILURE: std::cell::Cell<Option<usize>> = const {std::cell::Cell::new(None)};
        pub(super) static BATCH_SYNC_LOG: std::cell::RefCell<Vec<bool>> = const {std::cell::RefCell::new(Vec::new())};
    }
    fn batch_sync(file: &File, directory: bool) -> io::Result<()> {
        #[cfg(not(test))]
        let _ = directory;
        #[cfg(test)]
        {
            BATCH_SYNC_LOG.with(|log| log.borrow_mut().push(directory));
            if BATCH_SYNC_FAILURE.with(|left| match left.get() {
                Some(0) => {
                    left.set(None);
                    true
                }
                Some(n) => {
                    left.set(Some(n - 1));
                    false
                }
                None => false,
            }) {
                return Err(io::Error::other("Injected sync failure"));
            }
        }
        file.sync_all()
    }
    struct Staged {
        temporary: OsString,
        destination: OsString,
        before: Option<Signature>,
        before_links: u64,
        shared_before: Option<std::sync::Arc<Vec<u8>>>,
        owned: (u64, u64),
        synced: Option<Signature>,
        published: Option<Signature>,
    }
    fn old_bytes(parent: &Parent, name: &OsStr, expected: Signature) -> io::Result<Vec<u8>> {
        let mut file = parent.open_file(name, libc::O_RDONLY)?;
        let before = file.metadata()?;
        if signature(&before) != expected || before.len() > LIMIT {
            return Err(unsafe_path());
        }
        let mut bytes = Vec::with_capacity(before.len() as usize);
        (&mut file).take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != before.len()
            || signature(&file.metadata()?) != expected
            || parent.existing(name)?.map(|v| signature(&v)) != Some(expected)
        {
            return Err(unsafe_path());
        }
        Ok(bytes)
    }
    fn account_replaced_link(
        parent: &Parent,
        stages: &mut [Staged],
        replaced: usize,
    ) -> io::Result<()> {
        let Some(old) = stages[replaced].before else {
            return Ok(());
        };
        for stage in stages.iter_mut().filter(|s| {
            s.published.is_none() && s.before.is_some_and(|v| (v.0, v.1) == (old.0, old.1))
        }) {
            let expected = stage.before.ok_or_else(unsafe_path)?;
            let metadata = parent
                .existing(&stage.destination)?
                .ok_or_else(unsafe_path)?;
            let current = signature(&metadata);
            // Replacing one known alias decrements the shared old inode's link
            // count and changes ctime. Accept only that effect, with exact old
            // bytes and every other identity/content field still unchanged.
            if (
                current.0, current.1, current.2, current.3, current.4, current.7,
            ) != (
                expected.0, expected.1, expected.2, expected.3, expected.4, expected.7,
            ) || metadata.nlink().checked_add(1) != Some(stage.before_links)
                || old_bytes(parent, &stage.destination, current)?.as_slice()
                    != stage
                        .shared_before
                        .as_deref()
                        .ok_or_else(unsafe_path)?
                        .as_slice()
            {
                return Err(unsafe_path());
            }
            stage.before = Some(current);
            stage.before_links = metadata.nlink();
        }
        Ok(())
    }
    fn stages_current(parent: &Parent, stages: &[Staged]) -> io::Result<()> {
        for stage in stages {
            let expected = stage.published.or(stage.before);
            if parent.existing(&stage.destination)?.map(|v| signature(&v)) != expected {
                return Err(unsafe_path());
            }
            if stage.published.is_none()
                && parent.existing(&stage.temporary)?.map(|v| signature(&v)) != stage.synced
            {
                return Err(unsafe_path());
            }
        }
        Ok(())
    }
    /// Commit a small family in caller-defined order. Each staged file is
    /// synced; the shared directory is synced once after all renames. A later
    /// failure keeps earlier checkpoints and syncs them before returning error.
    pub(super) fn write_batch(directory: &Path, files: &[(&str, &[u8])]) -> io::Result<()> {
        let Some((first, _)) = files.first() else {
            return Err(unsafe_path());
        };
        let mut names = std::collections::HashSet::new();
        for (name, _) in files {
            let mut components = Path::new(name).components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
                || !names.insert(*name)
            {
                return Err(unsafe_path());
            }
        }
        let parent = Parent::open(&directory.join(first), true)?;
        let before = files
            .iter()
            .map(|(name, _)| parent.existing(OsStr::new(name)))
            .collect::<io::Result<Vec<_>>>()?;
        let mut shared = std::collections::HashMap::new();
        for (index, metadata) in before.iter().enumerate() {
            if let Some(metadata) = metadata {
                let pair = identity(metadata);
                if before
                    .iter()
                    .flatten()
                    .filter(|m| identity(m) == pair)
                    .count()
                    > 1
                    && !shared.contains_key(&pair)
                {
                    shared.insert(
                        pair,
                        std::sync::Arc::new(old_bytes(
                            &parent,
                            OsStr::new(files[index].0),
                            signature(metadata),
                        )?),
                    );
                }
            }
        }
        let mut stages = Vec::with_capacity(files.len());
        let result = (|| {
            for ((destination, bytes), before) in files.iter().zip(before) {
                let temporary =
                    OsString::from(format!(".scribe-{}.tmp", uuid::Uuid::new_v4().simple()));
                let mut file =
                    parent.open_file(&temporary, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
                let shared_before = before
                    .as_ref()
                    .and_then(|m| shared.get(&identity(m)).cloned());
                let before_links = before.as_ref().map_or(0, MetadataExt::nlink);
                stages.push(Staged {
                    temporary,
                    destination: OsString::from(destination),
                    before: before.as_ref().map(signature),
                    before_links,
                    shared_before,
                    owned: identity(&file.metadata()?),
                    synced: None,
                    published: None,
                });
                file.write_all(bytes)?;
                batch_sync(&file, false)?;
                stages.last_mut().ok_or_else(unsafe_path)?.synced =
                    Some(signature(&file.metadata()?));
                #[cfg(test)]
                hook("batch_staged", &directory.join(destination));
            }
            parent.current()?;
            stages_current(&parent, &stages)?;
            for index in 0..stages.len() {
                #[cfg(test)]
                hook(
                    "batch_before_rename",
                    &directory.join(&stages[index].destination),
                );
                // The final file is the completion record. Revalidate the
                // parent and every earlier format before it can become visible.
                if index + 1 == stages.len() {
                    parent.current()?;
                }
                stages_current(&parent, &stages)?;
                let stage = &mut stages[index];
                parent.rename(&stage.temporary, &stage.destination)?;
                #[cfg(test)]
                hook("batch_renamed", &directory.join(&stage.destination));
                let committed = parent
                    .existing(&stage.destination)?
                    .ok_or_else(unsafe_path)?;
                let synced = stage.synced.ok_or_else(unsafe_path)?;
                let current = signature(&committed);
                // Rename may change ctime, but must not conceal a write to the
                // published inode between rename and its first validation.
                if identity(&committed) != stage.owned
                    || (current.2, current.3, current.4, current.7)
                        != (synced.2, synced.3, synced.4, synced.7)
                {
                    return Err(unsafe_path());
                }
                stage.published = Some(signature(&committed));
                account_replaced_link(&parent, &mut stages, index)?;
                #[cfg(test)]
                hook(
                    "batch_published",
                    &directory.join(&stages[index].destination),
                );
            }
            batch_sync(&parent.directory, true)?;
            #[cfg(test)]
            hook("batch_synced", directory);
            parent.current()?;
            stages_current(&parent, &stages)
        })();
        if result.is_err() {
            for stage in &stages {
                if stage.published.is_none() {
                    parent.discard(&stage.temporary, stage.owned);
                }
            }
            // Do not roll back or unlink a published file: another writer may
            // already own its name. Flush surviving checkpoints and temp cleanup.
            let _ = batch_sync(&parent.directory, true);
        }
        result
    }
    pub(super) fn read(path: &Path) -> io::Result<Vec<u8>> {
        let parent = Parent::open(path, false)?;
        let mut file = parent.open_file(&parent.name, libc::O_RDONLY)?;
        let before = file.metadata()?;
        if !before.is_file() || before.len() > LIMIT {
            return Err(unsafe_path());
        }
        #[cfg(test)]
        hook("read_opened", &parent.path);
        let mut bytes = Vec::with_capacity(before.len() as usize);
        (&mut file).take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != before.len()
            || signature(&file.metadata()?) != signature(&before)
            || parent
                .existing(&parent.name)?
                .is_none_or(|after| signature(&after) != signature(&before))
        {
            return Err(unsafe_path());
        }
        parent.current()?;
        Ok(bytes)
    }
    pub(super) fn create_transient(path: &Path, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() as u64 > LIMIT {
            return Err(unsafe_path());
        }
        let parent = Parent::open(path, false)?;
        let mut file =
            parent.open_file(&parent.name, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL)?;
        let owned = identity(&file.metadata()?);
        let result = (|| {
            #[cfg(test)]
            hook("transient_opened", &parent.path);
            parent.current()?;
            file.write_all(bytes)?;
            file.flush()?;
            #[cfg(test)]
            hook("transient_written", &parent.path);
            let expected = signature(&file.metadata()?);
            file.seek(SeekFrom::Start(0))?;
            let mut verified = Vec::with_capacity(bytes.len());
            (&mut file).take(LIMIT + 1).read_to_end(&mut verified)?;
            if verified != bytes
                || signature(&file.metadata()?) != expected
                || parent
                    .existing(&parent.name)?
                    .is_none_or(|info| signature(&info) != expected)
            {
                return Err(unsafe_path());
            }
            parent.current()?;
            Ok(())
        })();
        if result.is_err() {
            parent.discard(&parent.name, owned);
        }
        result
    }
    pub(super) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let parent = Parent::open(path, true)?;
        let before = parent.existing(&parent.name)?.map(|v| signature(&v));
        let name = OsString::from(format!(".scribe-{}.tmp", uuid::Uuid::new_v4().simple()));
        let mut file = parent.open_file(&name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
        let owned = identity(&file.metadata()?);
        let result = (|| {
            #[cfg(test)]
            hook("write_opened", &parent.path);
            parent.current()?;
            file.write_all(bytes)?;
            file.sync_all()?;
            #[cfg(test)]
            hook("write_before_commit", &parent.path);
            parent.current()?;
            if parent.existing(&parent.name)?.map(|v| signature(&v)) != before {
                return Err(unsafe_path());
            }
            if parent
                .existing(&name)?
                .is_none_or(|value| identity(&value) != owned)
            {
                return Err(unsafe_path());
            }
            parent.rename(&name, &parent.name)?;
            parent.directory.sync_all()?;
            #[cfg(test)]
            hook("write_committed", &parent.path);
            parent.current()?;
            if parent
                .existing(&parent.name)?
                .is_none_or(|value| identity(&value) != owned)
            {
                return Err(unsafe_path());
            }
            Ok(())
        })();
        if result.is_err() {
            parent.discard(&name, owned);
        }
        result
    }
}

#[cfg(all(test, unix))]
mod io_race_tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};
    fn document(complete: bool) -> Value {
        serde_json::json!({"title":"Offline fixture", "source_url":"https://www.youtube.com/watch?v=abcdefghijk", "complete":complete, "full_text":if complete {"New complete transcript."} else {"Old partial transcript."}})
    }
    fn expected(document: &Value, extension: &str) -> Vec<u8> {
        if extension == "json" {
            serde_json::to_vec_pretty(document).unwrap()
        } else {
            render_document(document, extension).unwrap().into_bytes()
        }
    }
    fn assert_family(directory: &Path, document: &Value) {
        for extension in ["md", "txt", "json"] {
            assert_eq!(
                fs::read(directory.join(format!("transcript.{extension}"))).unwrap(),
                expected(document, extension)
            );
        }
    }
    fn reset_batch() {
        local_io::HOOK.with(|v| *v.borrow_mut() = None);
        local_io::BATCH_SYNC_FAILURE.with(|v| v.set(None));
        local_io::BATCH_SYNC_LOG.with(|v| v.borrow_mut().clear());
    }
    #[test]
    fn batch_syncs_each_format_and_publishes_completion_last() {
        use std::{cell::RefCell, os::unix::fs::PermissionsExt, rc::Rc};
        let temp = tempfile::tempdir().unwrap();
        write_exports(temp.path(), &document(false)).unwrap();
        reset_batch();
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        local_io::HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |event, path| {
                if event == "batch_published" {
                    recorded
                        .borrow_mut()
                        .push(path.file_name().unwrap().to_string_lossy().to_string());
                    if path.extension().unwrap() != "json" {
                        assert!(
                            !serde_json::from_slice::<Value>(
                                &fs::read(path.with_file_name("transcript.json")).unwrap()
                            )
                            .unwrap()["complete"]
                                .as_bool()
                                .unwrap()
                        );
                    } else {
                        assert_eq!(
                            fs::read(path.with_file_name("transcript.md")).unwrap(),
                            expected(&document(true), "md")
                        );
                        assert_eq!(
                            fs::read(path.with_file_name("transcript.txt")).unwrap(),
                            expected(&document(true), "txt")
                        );
                    }
                }
            }))
        });
        write_exports(temp.path(), &document(true)).unwrap();
        local_io::HOOK.with(|v| *v.borrow_mut() = None);
        assert_eq!(
            *events.borrow(),
            ["transcript.md", "transcript.txt", "transcript.json"]
        );
        local_io::BATCH_SYNC_LOG.with(|v| assert_eq!(*v.borrow(), [false, false, false, true]));
        assert_family(temp.path(), &document(true));
        for entry in fs::read_dir(temp.path()).unwrap() {
            assert_eq!(
                entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
    }
    #[test]
    fn failed_stage_sync_never_publishes_and_final_sync_failure_keeps_checkpoints() {
        for failure in 0..4 {
            let temp = tempfile::tempdir().unwrap();
            write_exports(temp.path(), &document(false)).unwrap();
            reset_batch();
            local_io::BATCH_SYNC_FAILURE.with(|v| v.set(Some(failure)));
            assert!(write_exports(temp.path(), &document(true)).is_err());
            assert_family(temp.path(), &document(failure == 3));
            assert_eq!(
                fs::read_dir(temp.path()).unwrap().count(),
                3,
                "Owned temporary files cleaned after failure {failure}"
            );
            local_io::BATCH_SYNC_LOG.with(|v| {
                let log = v.borrow();
                assert_eq!(
                    log.last(),
                    Some(&true),
                    "Recovery always syncs the directory"
                );
                if failure == 3 {
                    assert_eq!(*log, [false, false, false, true, true]);
                }
            });
        }
        reset_batch();
    }
    #[test]
    fn batch_keeps_concurrent_destination_edits_and_never_publishes_completion_after_them() {
        for changed in ["transcript.txt", "transcript.md"] {
            let temp = tempfile::tempdir().unwrap();
            write_exports(temp.path(), &document(false)).unwrap();
            reset_batch();
            local_io::HOOK.with(|h| {
                *h.borrow_mut() = Some(Box::new(move |event, path| {
                    let gate = if changed == "transcript.txt" {
                        "transcript.txt"
                    } else {
                        "transcript.json"
                    };
                    if event == "batch_before_rename" && path.file_name().unwrap() == gate {
                        fs::write(path.with_file_name(changed), "Foreign user edit").unwrap();
                    }
                }))
            });
            assert!(write_exports(temp.path(), &document(true)).is_err());
            local_io::HOOK.with(|v| *v.borrow_mut() = None);
            assert_eq!(
                fs::read_to_string(temp.path().join(changed)).unwrap(),
                "Foreign user edit"
            );
            assert_eq!(
                fs::read(temp.path().join("transcript.json")).unwrap(),
                expected(&document(false), "json")
            );
            assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
            local_io::BATCH_SYNC_LOG.with(|v| assert_eq!(v.borrow().last(), Some(&true)));
        }
    }
    #[test]
    fn batch_parent_swap_keeps_partial_checkpoints_without_touching_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("current");
        let detached = temp.path().join("detached");
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), "untouched").unwrap();
        write_exports(&directory, &document(false)).unwrap();
        reset_batch();
        let old = detached.clone();
        let replacement = outside.clone();
        local_io::HOOK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |event, path| {
                if event == "batch_published" && path.file_name().unwrap() == "transcript.md" {
                    let parent = path.parent().unwrap();
                    fs::rename(parent, &old).unwrap();
                    symlink(&replacement, parent).unwrap();
                }
            }))
        });
        assert!(write_exports(&directory, &document(true)).is_err());
        local_io::HOOK.with(|v| *v.borrow_mut() = None);
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
        assert_eq!(
            fs::read_to_string(outside.join("sentinel")).unwrap(),
            "untouched"
        );
        assert_eq!(
            fs::read(detached.join("transcript.json")).unwrap(),
            expected(&document(false), "json")
        );
        assert_eq!(fs::read_dir(detached).unwrap().count(), 3);
        local_io::BATCH_SYNC_LOG.with(|v| assert_eq!(v.borrow().last(), Some(&true)));
    }
    #[test]
    fn batch_final_validation_detects_foreign_replacement_without_deleting_it() {
        let temp = tempfile::tempdir().unwrap();
        write_exports(temp.path(), &document(false)).unwrap();
        reset_batch();
        local_io::HOOK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |event, path| {
                if event == "batch_synced" {
                    fs::write(path.join("transcript.json"), "Foreign final edit").unwrap();
                }
            }))
        });
        assert!(write_exports(temp.path(), &document(true)).is_err());
        local_io::HOOK.with(|v| *v.borrow_mut() = None);
        assert_eq!(
            fs::read_to_string(temp.path().join("transcript.json")).unwrap(),
            "Foreign final edit"
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
    }
    #[test]
    fn batch_rejects_an_edit_during_publication_without_touching_the_new_content() {
        let temp = tempfile::tempdir().unwrap();
        write_exports(temp.path(), &document(false)).unwrap();
        reset_batch();
        local_io::HOOK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |event, path| {
                if event == "batch_renamed" && path.file_name().unwrap() == "transcript.md" {
                    fs::write(path, "Foreign edit during publication").unwrap();
                }
            }))
        });
        assert!(write_exports(temp.path(), &document(true)).is_err());
        local_io::HOOK.with(|v| *v.borrow_mut() = None);
        assert_eq!(
            fs::read_to_string(temp.path().join("transcript.md")).unwrap(),
            "Foreign edit during publication"
        );
        assert_eq!(
            fs::read(temp.path().join("transcript.json")).unwrap(),
            expected(&document(false), "json")
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
    }
    #[test]
    fn batch_cleanup_preserves_foreign_temporary_name_replacements() {
        let temp = tempfile::tempdir().unwrap();
        write_exports(temp.path(), &document(false)).unwrap();
        reset_batch();
        local_io::HOOK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |event, path| {
                if event == "batch_staged" && path.file_name().unwrap() == "transcript.md" {
                    let parent = path.parent().unwrap();
                    let temporary = fs::read_dir(parent)
                        .unwrap()
                        .filter_map(|e| e.ok())
                        .find(|e| e.file_name().to_string_lossy().starts_with(".scribe-"))
                        .unwrap()
                        .path();
                    fs::rename(&temporary, parent.join("detached-owned.tmp")).unwrap();
                    fs::write(temporary, "Foreign temporary").unwrap();
                }
            }))
        });
        assert!(write_exports(temp.path(), &document(true)).is_err());
        local_io::HOOK.with(|v| *v.borrow_mut() = None);
        assert_family(temp.path(), &document(false));
        let leftovers = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".scribe-"))
            .collect::<Vec<_>>();
        assert_eq!(leftovers.len(), 1);
        assert_eq!(
            fs::read_to_string(leftovers[0].path()).unwrap(),
            "Foreign temporary"
        );
    }
    #[test]
    fn batch_accepts_own_hardlink_changes_but_rejects_shared_user_edits() {
        for edited in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            write_exports(temp.path(), &document(false)).unwrap();
            fs::remove_file(temp.path().join("transcript.txt")).unwrap();
            fs::hard_link(
                temp.path().join("transcript.md"),
                temp.path().join("transcript.txt"),
            )
            .unwrap();
            reset_batch();
            if edited {
                local_io::HOOK.with(|h| {
                    *h.borrow_mut() = Some(Box::new(move |event, path| {
                        if event == "batch_before_rename"
                            && path.file_name().unwrap() == "transcript.md"
                        {
                            fs::write(path, "Changed hardlinked content").unwrap();
                        }
                    }))
                });
            }
            let result = write_exports(temp.path(), &document(true));
            local_io::HOOK.with(|v| *v.borrow_mut() = None);
            if edited {
                assert!(result.is_err());
                assert_eq!(
                    fs::read_to_string(temp.path().join("transcript.txt")).unwrap(),
                    "Changed hardlinked content"
                );
                assert_eq!(
                    fs::read(temp.path().join("transcript.json")).unwrap(),
                    expected(&document(false), "json")
                );
            } else {
                result.unwrap();
                assert_family(temp.path(), &document(true));
            }
            assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
        }
    }
    #[test]
    #[ignore = "isolated fsync benchmark; run explicitly with --ignored --nocapture"]
    fn benchmark_export_commit_paths() {
        use std::time::Instant;
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old");
        let candidate = temp.path().join("candidate");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&candidate).unwrap();
        let document = serde_json::json!({"title":"Synthetic benchmark transcript","channel":"Offline fixture","source_url":"https://www.youtube.com/watch?v=abcdefghijk","complete":true,"full_text":"Every generated benchmark word is retained.\n".repeat(1500),"chunks":[{"text":"Synthetic words only"}]});
        let previous = |directory: &Path| {
            atomic_json(&directory.join("transcript.json"), &document).unwrap();
            for format in ["md", "txt"] {
                atomic_write(
                    &directory.join(format!("transcript.{format}")),
                    render_document(&document, format).unwrap().as_bytes(),
                )
                .unwrap();
            }
        };
        previous(&old);
        write_exports(&candidate, &document).unwrap();
        let mut before = vec![];
        let mut after = vec![];
        for index in 0..20 {
            if index % 2 == 0 {
                let start = Instant::now();
                previous(&old);
                before.push(start.elapsed().as_secs_f64());
                let start = Instant::now();
                write_exports(&candidate, &document).unwrap();
                after.push(start.elapsed().as_secs_f64());
            } else {
                let start = Instant::now();
                write_exports(&candidate, &document).unwrap();
                after.push(start.elapsed().as_secs_f64());
                let start = Instant::now();
                previous(&old);
                before.push(start.elapsed().as_secs_f64());
            }
        }
        for extension in ["json", "md", "txt"] {
            assert_eq!(
                fs::read(old.join(format!("transcript.{extension}"))).unwrap(),
                fs::read(candidate.join(format!("transcript.{extension}"))).unwrap()
            );
        }
        before.sort_by(f64::total_cmp);
        after.sort_by(f64::total_cmp);
        println!(
            "EXPORT_BENCH rounds=20 full_text_bytes={} old_mean_ms={:.3} candidate_mean_ms={:.3} old_median_ms={:.3} candidate_median_ms={:.3}",
            document["full_text"].as_str().unwrap().len(),
            before.iter().sum::<f64>() * 50.,
            after.iter().sum::<f64>() * 50.,
            (before[9] + before[10]) * 500.,
            (after[9] + after[10]) * 500.
        );
    }
    #[test]
    fn transient_failure_cleanup_preserves_foreign_replacement_files() {
        for stage in ["transient_opened", "transient_written"] {
            let temp = tempfile::tempdir().unwrap();
            let base = fs::canonicalize(temp.path()).unwrap();
            let path = base.join("input.json");
            let mut changed = false;
            local_io::HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |point, path| {
                    if point == stage && !changed {
                        changed = true;
                        fs::rename(path, path.with_extension("detached")).unwrap();
                        fs::write(path, b"Foreign replacement").unwrap();
                    }
                }))
            });
            assert!(
                create_transient_json(&path, &serde_json::json!({"input":"all bytes"})).is_err()
            );
            local_io::HOOK.with(|hook| *hook.borrow_mut() = None);
            assert_eq!(fs::read(&path).unwrap(), b"Foreign replacement");
            assert!(base.join("input.detached").is_file());
        }
    }
    #[test]
    fn transient_parent_replacement_is_rejected_and_cleanup_stays_in_owned_parent() {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let parent = base.join("owned");
        let detached = base.join("detached");
        fs::create_dir(&parent).unwrap();
        let old = detached.clone();
        let source = parent.clone();
        local_io::HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |point, _| {
                if point == "transient_opened" {
                    fs::rename(&source, &old).unwrap();
                    fs::create_dir(&source).unwrap();
                    fs::write(source.join("input.json"), b"Keep unrelated replacement").unwrap();
                }
            }))
        });
        assert!(
            create_transient_json(
                &parent.join("input.json"),
                &serde_json::json!({"private":"input"})
            )
            .is_err()
        );
        local_io::HOOK.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(
            fs::read(parent.join("input.json")).unwrap(),
            b"Keep unrelated replacement"
        );
        assert_eq!(fs::read_dir(detached).unwrap().count(), 0);
    }
    #[test]
    fn replaced_parent_is_detected_before_read_or_write_can_claim_success() {
        for stage in [
            "read_opened",
            "write_opened",
            "write_before_commit",
            "write_committed",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let base = fs::canonicalize(temp.path()).unwrap();
            let current = base.join("current");
            let old = base.join("detached");
            let outside = base.join("other");
            fs::create_dir(&current).unwrap();
            fs::create_dir(&outside).unwrap();
            fs::write(current.join("job.json"), br#"{"original":true}"#).unwrap();
            fs::write(outside.join("job.json"), br#"{"untouched":true}"#).unwrap();
            let from = current.clone();
            let detached = old.clone();
            let other = outside.clone();
            local_io::HOOK.with(|h| {
                *h.borrow_mut() = Some(Box::new(move |event, _| {
                    if event == stage {
                        fs::rename(&from, &detached).unwrap();
                        symlink(&other, &from).unwrap();
                    }
                }))
            });
            let result = if stage == "read_opened" {
                read_json(&current.join("job.json")).map(|_| ())
            } else {
                atomic_json(
                    &current.join("job.json"),
                    &serde_json::json!({"replacement":true}),
                )
            };
            local_io::HOOK.with(|h| *h.borrow_mut() = None);
            assert!(result.is_err(), "Must detect parent replacement at {stage}");
            assert_eq!(
                fs::read(outside.join("job.json")).unwrap(),
                br#"{"untouched":true}"#
            );
            assert_eq!(
                fs::read_dir(&old).unwrap().count(),
                1,
                "No owned temporary files should remain"
            );
            if stage != "write_committed" {
                assert_eq!(
                    fs::read(old.join("job.json")).unwrap(),
                    br#"{"original":true}"#
                );
            }
        }
    }
    #[test]
    fn concurrent_leaf_edit_is_not_overwritten_and_changed_reads_are_rejected() {
        for stage in ["read_opened", "write_before_commit"] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("job.json");
            atomic_json(&path, &serde_json::json!({"old":true})).unwrap();
            local_io::HOOK.with(|h| {
                *h.borrow_mut() = Some(Box::new(move |event, path| {
                    if event == stage {
                        fs::write(path, br#"{"concurrent":"user edit"}"#).unwrap();
                    }
                }))
            });
            let result = if stage == "read_opened" {
                read_json(&path).map(|_| ())
            } else {
                atomic_json(&path, &serde_json::json!({"new":true}))
            };
            local_io::HOOK.with(|h| *h.borrow_mut() = None);
            assert!(result.is_err());
            assert_eq!(read_json(&path).unwrap()["concurrent"], "user edit");
        }
    }
    #[test]
    fn rollback_keeps_an_unowned_replacement_of_the_temporary_filename() {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let path = base.join("job.json");
        atomic_json(&path, &serde_json::json!({"old":true})).unwrap();
        local_io::HOOK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |event, path| {
                if event == "write_before_commit" {
                    let parent = path.parent().unwrap();
                    let temporary = fs::read_dir(parent)
                        .unwrap()
                        .filter_map(|v| v.ok())
                        .find(|v| v.file_name().to_string_lossy().starts_with(".scribe-"))
                        .unwrap()
                        .path();
                    fs::rename(&temporary, parent.join("detached-owned.tmp")).unwrap();
                    fs::write(temporary, "Foreign replacement must survive").unwrap();
                }
            }))
        });
        assert!(atomic_json(&path, &serde_json::json!({"new":true})).is_err());
        local_io::HOOK.with(|h| *h.borrow_mut() = None);
        assert_eq!(read_json(&path).unwrap(), serde_json::json!({"old":true}));
        let leftovers = fs::read_dir(base)
            .unwrap()
            .filter_map(|v| v.ok())
            .filter(|v| v.file_name().to_string_lossy().starts_with(".scribe-"))
            .collect::<Vec<_>>();
        assert_eq!(leftovers.len(), 1);
        assert_eq!(
            fs::read_to_string(leftovers[0].path()).unwrap(),
            "Foreign replacement must survive"
        );
    }
}

#[cfg(not(unix))]
mod local_io {
    use std::{io, path::Path};
    // The shipped helper requires macOS. Unsupported platforms fail closed
    // instead of falling back to path traversal with weaker guarantees.
    pub(super) fn read(_: &Path) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Secure local IO requires Unix",
        ))
    }
    pub(super) fn write(_: &Path, _: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Secure local IO requires Unix",
        ))
    }
    pub(super) fn write_batch(_: &Path, _: &[(&str, &[u8])]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Secure local IO requires Unix",
        ))
    }
}
pub fn canonical_video_url(value: &str) -> Result<(String, String)> {
    let error = || {
        Error::bad_request(
            "Use a YouTube video URL (watch, Shorts, embed, or youtu.be), not a playlist or another website.",
        )
    };
    let u = url::Url::parse(value.trim()).map_err(|_| error())?;
    if !["https", "http"].contains(&u.scheme())
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some_and(|p| p != 80 && p != 443)
    {
        return Err(error());
    }
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    let path = u.path().trim_end_matches('/');
    let id = if host == "youtu.be" || host == "www.youtu.be" {
        path.trim_start_matches('/').to_string()
    } else if [
        "youtube.com",
        "www.youtube.com",
        "m.youtube.com",
        "music.youtube.com",
    ]
    .contains(&host.as_str())
    {
        if path == "/watch" {
            let values: Vec<_> = u
                .query_pairs()
                .filter(|(k, _)| k == "v")
                .map(|(_, v)| v.into_owned())
                .collect();
            if values.len() != 1 {
                return Err(error());
            }
            values[0].clone()
        } else {
            let parts: Vec<_> = path.split('/').collect();
            if parts.len() != 3 || !["shorts", "embed", "live"].contains(&parts[1]) {
                return Err(error());
            }
            parts[2].to_string()
        }
    } else {
        return Err(error());
    };
    if id.len() != 11
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(error());
    }
    Ok((id.clone(), format!("https://www.youtube.com/watch?v={id}")))
}
