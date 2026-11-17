//! Descriptor-relative storage prevents pathname substitution after validation.

use std::{
    ffi::CString,
    fs::{DirBuilder, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
    time::{Duration, Instant},
};

use super::{AuthError, DYNAMIC_CLIENT, MAX_BODY, Result, SavedState};

pub(super) struct Directory(File);

impl Directory {
    pub(super) fn open(path: &Path) -> Result<Self> {
        match DirBuilder::new().recursive(true).mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(AuthError::Storage),
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| AuthError::UnsafeStorage)?;
        let metadata = file.metadata().map_err(|_| AuthError::Storage)?;
        // SAFETY: geteuid has no preconditions and returns the process UID.
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o700
            || !metadata.is_dir()
        {
            return Err(AuthError::UnsafeStorage);
        }
        Ok(Self(file))
    }

    fn open_file(&self, name: &str, flags: i32) -> Result<File> {
        let name = CString::new(name).map_err(|_| AuthError::Storage)?;
        // SAFETY: the directory FD remains owned by self; name is a fixed,
        // NUL-terminated basename. O_NOFOLLOW refuses final symlinks.
        let descriptor = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0o600 as libc::c_uint,
            )
        };
        if descriptor < 0 {
            return Err(AuthError::Storage);
        }
        // SAFETY: openat returned a fresh, valid descriptor owned by this File.
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = file.metadata().map_err(|_| AuthError::Storage)?;
        // SAFETY: geteuid has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600
            || !metadata.is_file()
            || metadata.nlink() != 1
        {
            return Err(AuthError::UnsafeStorage);
        }
        Ok(file)
    }

    pub(super) async fn lock(&self) -> Result<File> {
        let file = self.open_file("openai.lock", libc::O_RDWR | libc::O_CREAT)?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= Duration::from_secs(30) {
                        return Err(AuthError::Busy);
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(_) => return Err(AuthError::Storage),
            }
        }
    }

    pub(super) fn read(&self) -> Result<Option<SavedState>> {
        let mut file = match self.open_file("openai.json", libc::O_RDONLY) {
            Ok(file) => file,
            Err(AuthError::Storage) => {
                // Distinguish a missing file using fstatat without following
                // symlinks. Other errors never become an empty registration.
                let name = c"openai.json";
                let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
                // SAFETY: metadata points to writable stat storage; basename
                // and directory descriptor are valid for this call.
                let result = unsafe {
                    libc::fstatat(
                        self.0.as_raw_fd(),
                        name.as_ptr(),
                        metadata.as_mut_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                };
                if result < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
                {
                    return Ok(None);
                }
                return Err(AuthError::Storage);
            }
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take((MAX_BODY + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| AuthError::Storage)?;
        if bytes.len() > MAX_BODY {
            return Err(AuthError::Storage);
        }
        let saved: SavedState = serde_json::from_slice(&bytes).map_err(|_| AuthError::Storage)?;
        let valid_host = saved
            .host_id
            .strip_prefix("urn:uuid:")
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .is_some_and(|id| id.get_version_num() == 4);
        if saved.version != 1
            || !valid_host
            || saved.registration.as_ref().is_some_and(|registration| {
                registration.client_id.is_empty()
                    || registration.client_id == DYNAMIC_CLIENT
                    || registration.client_id.len() > 512
                    || registration.subject.is_empty()
                    || registration.issuer.is_empty()
            })
        {
            return Err(AuthError::Storage);
        }
        Ok(Some(saved))
    }

    pub(super) fn write(&self, saved: &SavedState) -> Result<()> {
        let bytes = serde_json::to_vec(saved).map_err(|_| AuthError::Storage)?;
        if bytes.len() > MAX_BODY {
            return Err(AuthError::Storage);
        }
        let name = format!(".openai-{}.tmp", uuid::Uuid::new_v4());
        let name_c = CString::new(name.as_str()).map_err(|_| AuthError::Storage)?;
        let result = (|| {
            let mut file = self.open_file(&name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
            file.write_all(&bytes).map_err(|_| AuthError::Storage)?;
            file.sync_all().map_err(|_| AuthError::Storage)?;
            // SAFETY: both basenames and the held directory descriptor are
            // valid. Atomic replacement cannot follow a destination symlink.
            let result = unsafe {
                libc::renameat(
                    self.0.as_raw_fd(),
                    name_c.as_ptr(),
                    self.0.as_raw_fd(),
                    c"openai.json".as_ptr(),
                )
            };
            if result != 0 {
                return Err(AuthError::Storage);
            }
            self.0.sync_all().map_err(|_| AuthError::Storage)
        })();
        if result.is_err() {
            // SAFETY: remove only this operation's random temporary basename
            // relative to our still-held directory descriptor.
            unsafe { libc::unlinkat(self.0.as_raw_fd(), name_c.as_ptr(), 0) };
        }
        result
    }
}
