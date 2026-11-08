use fs2::FileExt;
#[cfg(feature = "engine")]
use scribe_common::Settings;
use scribe_common::{Error, Result};
#[cfg(feature = "engine")]
use scribe_engine::Engine;
#[cfg(feature = "engine")]
use scribe_library::Library;
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

pub struct HelperLock {
    _file: File,
}
/// Clean only the dedicated transient audio tree while its owner lock is held.
/// This runs before parsing library state, so corrupt history cannot retain audio.
pub fn cleanup_work(_lock: &HelperLock, data_dir: &Path) -> Result<()> {
    let work = data_dir.join("work");
    match std::fs::symlink_metadata(&work) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(Error::conflict(
                "The temporary audio folder is unsafe. Check the helper data folder.",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&work)?;
        }
        Err(error) => return Err(error.into()),
    }
    for item in std::fs::read_dir(&work)? {
        let item = item?;
        if item.file_type()?.is_dir() {
            std::fs::remove_dir_all(item.path())?;
        } else {
            std::fs::remove_file(item.path())?;
        }
    }
    Ok(())
}
impl HelperLock {
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        if std::fs::symlink_metadata(data_dir)?
            .file_type()
            .is_symlink()
        {
            return Err(Error::conflict(
                "The helper data folder must not be a symbolic link.",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let file = options.open(data_dir.join(".helper.lock"))?;
        file.try_lock_exclusive().map_err(|_| {
            Error::conflict("Another helper is already running for this data folder.")
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(feature = "engine")]
pub async fn serve(settings: Settings, port: u16) -> Result<()> {
    use std::sync::Arc;
    // Cross-language flock interlocks with the previous Python helper. No
    // library restoration or temporary audio cleanup occurs before it is held.
    let _lock = HelperLock::acquire(&settings.data_dir)?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|_| Error::conflict("The local helper port is already in use or unavailable."))?;
    cleanup_work(&_lock, &settings.data_dir)?;
    let library = Arc::new(Library::open(&settings.data_dir)?);
    let settings = Arc::new(parking_lot::RwLock::new(settings));
    let engine = Arc::new(Engine::open(settings.clone(), library.clone()).await?);
    let state = crate::api::AppState::new(settings, library, engine.clone());
    let result = axum::serve(listener, crate::api::router(state.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    state.shutdown().await;
    let shutdown = engine.shutdown().await;
    result.map_err(|_| Error::internal("The local helper server stopped unexpectedly."))?;
    shutdown
}
#[cfg(feature = "engine")]
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_ = tokio::signal::ctrl_c()=>{},_ = terminate.recv()=>{}}
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
