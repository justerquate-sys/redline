//! Descriptor-relative, no-follow filesystem operations. Never traverse a symlink.
use scribe_common::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString},
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) fn safe(message: &str) -> Error {
    Error::new(400, message)
}
pub(crate) fn io_error() -> Error {
    safe(
        "The folder could not be read completely or changed while being copied. Try again; nothing was copied.",
    )
}
fn c(value: &str) -> Result<CString> {
    CString::new(value).map_err(|_| io_error())
}
fn errno_ptr() -> *mut libc::c_int {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::__error()
    }
    #[cfg(not(target_os = "macos"))]
    unsafe {
        libc::__errno_location()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct Identity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: i128,
    pub ctime: i128,
    pub mode: u32,
}
impl Identity {
    pub fn metadata(m: &std::fs::Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            size: m.len(),
            mtime: m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128,
            ctime: m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128,
            mode: m.mode(),
        }
    }
    pub fn pair(&self) -> serde_json::Value {
        serde_json::json!([self.dev, self.ino])
    }
    pub fn is_file(&self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
    }
    pub fn is_dir(&self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
    }
}
#[derive(Debug)]
pub(crate) struct Dir(pub File);
#[cfg(test)]
thread_local! { static FAIL_SYNC: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
pub(crate) fn fail_sync_after(count: usize) {
    FAIL_SYNC.set(Some(count));
}
#[cfg(test)]
thread_local! {static AFTER_READ:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=const {std::cell::RefCell::new(None)};}
impl Dir {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(io_error());
        }
        let fd = unsafe {
            libc::open(
                c("/")?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io_error());
        }
        let mut out = Self(unsafe { File::from_raw_fd(fd) });
        for part in path.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(s) => {
                    let name = s.to_str().ok_or_else(io_error)?;
                    out = out.child(name)?
                }
                _ => return Err(io_error()),
            }
        }
        Ok(out)
    }
    pub fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
    pub fn identity(&self) -> Result<Identity> {
        Ok(Identity::metadata(
            &self.0.metadata().map_err(|_| io_error())?,
        ))
    }
    pub fn clone_dir(&self) -> Result<Self> {
        Ok(Self(self.0.try_clone().map_err(|_| io_error())?))
    }
    pub fn child(&self, name: &str) -> Result<Self> {
        let fd = unsafe {
            libc::openat(
                self.fd(),
                c(name)?.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io_error())
        } else {
            Ok(Self(unsafe { File::from_raw_fd(fd) }))
        }
    }
    pub fn mkdir(&self, name: &str) -> Result<Self> {
        let code = unsafe { libc::mkdirat(self.fd(), c(name)?.as_ptr(), 0o700) };
        if code < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
            return Err(io_error());
        }
        self.child(name)
    }
    pub fn entries(&self) -> Result<Vec<(String, Identity)>> {
        let duplicate = unsafe { libc::dup(self.fd()) };
        if duplicate < 0 {
            return Err(io_error());
        }
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            unsafe { libc::close(duplicate) };
            return Err(io_error());
        }
        // A duplicate descriptor shares the directory offset, so explicitly rewind.
        unsafe { libc::rewinddir(stream) };
        let result = (|| {
            let mut entries = Vec::new();
            loop {
                unsafe {
                    *errno_ptr() = 0;
                }
                let item = unsafe { libc::readdir(stream) };
                if item.is_null() {
                    if unsafe { *errno_ptr() } != 0 {
                        return Err(io_error());
                    }
                    break;
                }
                let bytes = unsafe { CStr::from_ptr((*item).d_name.as_ptr()) }.to_bytes();
                let name = std::str::from_utf8(bytes).map_err(|_| io_error())?;
                if name == "." || name == ".." {
                    continue;
                }
                entries.push((name.to_owned(), self.stat(name)?));
                if entries.len() > 26001 {
                    return Err(io_error());
                }
            }
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(entries)
        })();
        unsafe { libc::closedir(stream) };
        result
    }
    pub fn stat(&self, name: &str) -> Result<Identity> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.fd(),
                c(name)?.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            return Err(io_error());
        }
        let st = unsafe { st.assume_init() };
        #[cfg(target_os = "macos")]
        let (mtime, ctime) = (
            st.st_mtime as i128 * 1_000_000_000 + st.st_mtime_nsec as i128,
            st.st_ctime as i128 * 1_000_000_000 + st.st_ctime_nsec as i128,
        );
        #[cfg(not(target_os = "macos"))]
        let (mtime, ctime) = (
            st.st_mtime as i128 * 1_000_000_000 + st.st_mtime_nsec as i128,
            st.st_ctime as i128 * 1_000_000_000 + st.st_ctime_nsec as i128,
        );
        Ok(Identity {
            dev: st.st_dev as u64,
            ino: st.st_ino,
            size: st.st_size.max(0) as u64,
            mtime,
            ctime,
            mode: st.st_mode as u32,
        })
    }
    pub fn read(&self, name: &str, limit: u64, expected: Option<&Identity>) -> Result<Vec<u8>> {
        let fd = unsafe {
            libc::openat(
                self.fd(),
                c(name)?.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let before = Identity::metadata(&file.metadata().map_err(|_| io_error())?);
        if !before.is_file() || before.size > limit || expected.is_some_and(|e| e != &before) {
            return Err(io_error());
        }
        let mut bytes = Vec::with_capacity(before.size as usize);
        (&mut file)
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| io_error())?;
        #[cfg(test)]
        if let Some(callback) = AFTER_READ.with(|hook| hook.borrow_mut().take()) {
            callback();
        }
        if bytes.len() as u64 != before.size
            || Identity::metadata(&file.metadata().map_err(|_| io_error())?) != before
            || self.stat(name)? != before
        {
            return Err(io_error());
        }
        Ok(bytes)
    }
    pub fn create(&self, name: &str, bytes: &[u8]) -> Result<Identity> {
        let fd = unsafe {
            libc::openat(
                self.fd(),
                c(name)?.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let owned = Identity::metadata(&file.metadata().map_err(|_| io_error())?);
        let result = (|| {
            file.write_all(bytes).map_err(|_| io_error())?;
            file.sync_all().map_err(|_| io_error())?;
            self.sync()?;
            Ok(Identity::metadata(
                &file.metadata().map_err(|_| io_error())?,
            ))
        })();
        if result.is_err() {
            self.discard_owned(name, &owned);
        }
        result
    }
    pub fn sync(&self) -> Result<()> {
        #[cfg(test)]
        if FAIL_SYNC.with(|remaining| match remaining.get() {
            Some(0) => {
                remaining.set(None);
                true
            }
            Some(n) => {
                remaining.set(Some(n - 1));
                false
            }
            None => false,
        }) {
            return Err(io_error());
        }
        self.0.sync_all().map_err(|_| io_error())
    }
    pub fn rename(&self, old: &str, target: &Dir, new: &str) -> Result<()> {
        if unsafe { libc::renameat(self.fd(), c(old)?.as_ptr(), target.fd(), c(new)?.as_ptr()) } < 0
        {
            Err(io_error())
        } else {
            Ok(())
        }
    }
    pub fn unlink(&self, name: &str, directory: bool) -> Result<()> {
        if unsafe {
            libc::unlinkat(
                self.fd(),
                c(name)?.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        } < 0
        {
            Err(io_error())
        } else {
            Ok(())
        }
    }
    pub fn restore_link(&self, from: &str, to: &str) {
        if let (Ok(a), Ok(b)) = (c(from), c(to))
            && unsafe { libc::linkat(self.fd(), a.as_ptr(), self.fd(), b.as_ptr(), 0) } == 0
        {
            let _ = self.unlink(from, false);
        }
    }
    pub fn discard_owned(&self, name: &str, owned: &Identity) {
        let Ok(current) = self.stat(name) else {
            return;
        };
        if current.dev != owned.dev || current.ino != owned.ino {
            return;
        }
        let stage = format!(".discard-{}", uuid::Uuid::new_v4().simple());
        if self.rename(name, self, &stage).is_err() {
            return;
        }
        if self
            .stat(&stage)
            .is_ok_and(|i| i.dev == owned.dev && i.ino == owned.ino && i.is_file())
        {
            let _ = self.unlink(&stage, false);
        } else {
            self.restore_link(&stage, name);
        }
    }
    pub fn atomic_json(&self, name: &str, value: &serde_json::Value) -> Result<()> {
        let tmp = format!(".write-{}", uuid::Uuid::new_v4().simple());
        let owned = self.create(&tmp, &serde_json::to_vec(value)?)?;
        let result = self.rename(&tmp, self, name).and_then(|_| self.sync());
        if result.is_err() {
            self.discard_owned(&tmp, &owned);
        }
        result
    }
    pub fn nested(&self, parts: &[String]) -> Result<Self> {
        let mut dir = self.clone_dir()?;
        for name in parts {
            dir = dir.child(name)?
        }
        Ok(dir)
    }
}

pub(crate) fn read_path(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(io_error)?;
    Dir::open(path.parent().ok_or_else(io_error)?)?.read(name, limit, None)
}
pub(crate) fn finder_metadata(name: &str) -> bool {
    [".DS_Store", ".localized", "Icon\r"].contains(&name) || name.starts_with("._")
}
pub(crate) fn safe_part(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\', '\0'])
        && !(name.as_bytes().get(1) == Some(&b':') && name.as_bytes()[0].is_ascii_alphabetic())
}
pub(crate) fn exclusions(names: &[String]) -> Result<BTreeSet<String>> {
    if names.len() > 1000
        || names
            .iter()
            .any(|s| !safe_part(s) || s.len() > 255 || s.chars().any(|c| c < ' ' || c == '\x7f'))
    {
        return Err(safe(
            "Choose valid folder names from the All folder list (at most 1,000).",
        ));
    }
    Ok(names.iter().cloned().collect())
}
pub(crate) struct Tree {
    pub base: PathBuf,
    pub root: Dir,
    pub inventory: BTreeMap<Vec<String>, Identity>,
    pub strict: bool,
    pub skip_finder: bool,
    pub excluded: BTreeSet<String>,
    started: Instant,
    last_parent: RefCell<Option<(Vec<String>, Dir)>>,
}
impl Tree {
    pub fn scan(
        base: &Path,
        strict: bool,
        skip_finder: bool,
        excluded: BTreeSet<String>,
    ) -> Result<Self> {
        let mut t = Self {
            base: base.to_owned(),
            root: Dir::open(base)?,
            inventory: BTreeMap::new(),
            strict,
            skip_finder,
            excluded,
            started: Instant::now(),
            last_parent: RefCell::new(None),
        };
        let dir = t.root.clone_dir()?;
        t.inventory = t.walk(&dir, &[])?;
        Ok(t)
    }
    pub fn check_limit(&self) -> Result<()> {
        if self.started.elapsed() > Duration::from_secs(20) {
            Err(safe(
                "This folder is too large to copy in one request. Choose a smaller folder; nothing was copied.",
            ))
        } else {
            Ok(())
        }
    }
    fn walk(&self, dir: &Dir, relative: &[String]) -> Result<BTreeMap<Vec<String>, Identity>> {
        self.check_limit()?;
        if relative.len() > 128 {
            return Err(io_error());
        }
        let before = dir.identity()?;
        let mut out = BTreeMap::from([(relative.to_vec(), before.clone())]);
        let children = dir.entries()?;
        if relative.is_empty() {
            for excluded in &self.excluded {
                if !children.iter().any(|(n, m)| n == excluded && m.is_dir()) {
                    return Err(Error::new(
                        409,
                        "An excluded folder changed. Refresh the folder list and try again; nothing was copied.",
                    ));
                }
            }
        }
        for (name, info) in children {
            self.check_limit()?;
            if relative.is_empty() && self.excluded.contains(&name) {
                continue;
            }
            if self.skip_finder && finder_metadata(&name) {
                continue;
            }
            let mut key = relative.to_vec();
            key.push(name.clone());
            if info.is_dir() {
                let child = dir.child(&name)?;
                if child.identity()? != info {
                    return Err(io_error());
                }
                out.extend(self.walk(&child, &key)?)
            } else if info.is_file() {
                out.insert(key, info);
            } else if self.strict {
                return Err(safe(
                    "This folder contains a link or unreadable entry. Nothing was copied.",
                ));
            }
            if out.len() > 25000 {
                return Err(safe(
                    "This folder contains too many entries. Choose a smaller folder.",
                ));
            }
        }
        if dir.identity()? != before {
            return Err(io_error());
        }
        Ok(out)
    }
    pub fn unchanged(&self) -> Result<()> {
        let mut seen = 0;
        self.validate_walk(&self.root, &mut Vec::new(), &mut seen)?;
        if seen != self.inventory.len()
            || Dir::open(&self.base)?.identity()? != self.inventory[&Vec::new()]
        {
            return Err(io_error());
        }
        Ok(())
    }
    fn validate_walk(&self, dir: &Dir, relative: &mut Vec<String>, seen: &mut usize) -> Result<()> {
        self.check_limit()?;
        if relative.len() > 128 {
            return Err(io_error());
        }
        let before = dir.identity()?;
        if self.inventory.get(relative) != Some(&before) {
            return Err(io_error());
        }
        *seen += 1;
        let children = dir.entries()?;
        if relative.is_empty() {
            for excluded in &self.excluded {
                if !children
                    .iter()
                    .any(|(name, info)| name == excluded && info.is_dir())
                {
                    return Err(io_error());
                }
            }
        }
        for (name, info) in children {
            self.check_limit()?;
            if (relative.is_empty() && self.excluded.contains(&name))
                || (self.skip_finder && finder_metadata(&name))
            {
                continue;
            }
            relative.push(name.clone());
            if info.is_dir() {
                let child = dir.child(&name)?;
                if child.identity()? != info {
                    return Err(io_error());
                }
                self.validate_walk(&child, relative, seen)?;
            } else if info.is_file() {
                if self.inventory.get(relative) != Some(&info) {
                    return Err(io_error());
                }
                *seen += 1;
            } else if self.strict {
                return Err(io_error());
            }
            relative.pop();
        }
        if dir.identity()? != before {
            return Err(io_error());
        }
        Ok(())
    }
    /// A bounded, request-local descriptor cursor. Consecutive files in a
    /// directory share one opened parent; the final inventory walk still checks
    /// every name and inode, including replaced ancestors, before any success.
    pub fn with_parent<T>(
        &self,
        parts: &[String],
        action: impl FnOnce(&Dir) -> Result<T>,
    ) -> Result<T> {
        if parts.is_empty() {
            return action(&self.root);
        }
        let mut cached = self.last_parent.borrow_mut();
        if cached.as_ref().is_none_or(|(old, _)| old != parts) {
            let parent = self.root.nested(parts)?;
            if self.inventory.get(parts) != Some(&parent.identity()?) {
                return Err(io_error());
            }
            *cached = Some((parts.to_vec(), parent));
        }
        action(&cached.as_ref().ok_or_else(io_error)?.1)
    }
    pub fn read(&self, relative: &[String], limit: u64) -> Result<Vec<u8>> {
        let (name, parent) = relative.split_last().ok_or_else(io_error)?;
        self.with_parent(parent, |dir| {
            dir.read(name, limit, self.inventory.get(relative))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};
    #[test]
    fn descriptor_read_rejects_a_replaced_leaf_even_when_the_open_inode_is_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        fs::write(root.join("file"), "Owned bytes").unwrap();
        fs::write(root.join("outside"), "Foreign bytes").unwrap();
        let path = root.clone();
        AFTER_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(path.join("file"), path.join("detached")).unwrap();
                symlink(path.join("outside"), path.join("file")).unwrap();
            }))
        });
        assert!(Dir::open(&root).unwrap().read("file", 100, None).is_err());
        assert_eq!(
            fs::read_to_string(root.join("outside")).unwrap(),
            "Foreign bytes"
        );
    }
    #[test]
    fn request_local_parent_cursor_cannot_hide_replaced_ancestors_or_new_entries() {
        for change in ["ancestor", "added", "edited"] {
            let temp = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(temp.path()).unwrap();
            let base = root.join("Tree");
            fs::create_dir_all(base.join("nested")).unwrap();
            fs::write(base.join("nested/first"), "First").unwrap();
            fs::write(base.join("nested/second"), "Second").unwrap();
            let tree = Tree::scan(&base, true, false, BTreeSet::new()).unwrap();
            assert_eq!(
                tree.read(&["nested".into(), "first".into()], 100).unwrap(),
                b"First"
            );
            match change {
                "ancestor" => {
                    let outside = root.join("Outside");
                    fs::create_dir(&outside).unwrap();
                    fs::write(outside.join("second"), "Foreign").unwrap();
                    fs::rename(base.join("nested"), root.join("Detached")).unwrap();
                    symlink(&outside, base.join("nested")).unwrap();
                    // Reading an accepted descriptor may still return its old
                    // bytes, but the operation cannot publish a successful ZIP.
                    assert_eq!(
                        tree.read(&["nested".into(), "second".into()], 100).unwrap(),
                        b"Second"
                    );
                }
                "added" => {
                    fs::write(base.join("nested/late"), "Late").unwrap();
                }
                _ => {
                    fs::write(base.join("nested/second"), "Edited").unwrap();
                    assert!(tree.read(&["nested".into(), "second".into()], 100).is_err());
                }
            }
            assert!(tree.unchanged().is_err());
        }
    }
}
