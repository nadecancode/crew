use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Result};
use tokio::net::{TcpListener, UnixListener};

use super::service::{lock, Gateway};

struct Files {
    socket: PathBuf,
    token: PathBuf,
    socket_inode: u64,
    _lock: File,
}

pub(crate) fn acquire_lock(runtime: &std::path::Path) -> Result<File> {
    crew_core::paths::ensure_private_dir(runtime)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(runtime.join("native.lock"))?;
    let metadata = file.metadata()?;
    if metadata.uid() != crew_core::paths::uid()
        || metadata.permissions().mode() & 0o777 != 0o600
        || !metadata.is_file()
        || metadata.nlink() != 1
    {
        bail!("invalid native lock file");
    }
    // SAFETY: live descriptor and constant lock operation.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("native gateway already running");
    }
    Ok(file)
}
impl Drop for Files {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.socket)
            .is_ok_and(|m| m.ino() == self.socket_inode && m.uid() == crew_core::paths::uid())
        {
            let _ = std::fs::remove_file(&self.socket);
            let _ = std::fs::remove_file(&self.token);
        }
    }
}

impl Gateway {
    /// Serve an explicitly configured endpoint. Tests supply isolated runtime
    /// directories and loopback addresses; this never starts a team or web hub.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        crew_core::client::refuse_in_agent_context()?;
        let lock_file = lock(&self.startup_lock)
            .take()
            .ok_or_else(|| anyhow::anyhow!("native gateway already served"))?;
        let socket = self.config.runtime.join("native.sock");
        let token_path = self.config.runtime.join("native.token");
        if let Ok(metadata) = std::fs::symlink_metadata(&socket) {
            if !metadata.file_type().is_socket() || metadata.uid() != crew_core::paths::uid() {
                bail!("invalid native control socket path");
            }
            std::fs::remove_file(&socket)?;
        }
        let token = super::fresh::<32>()?.to_hex();
        crew_core::paths::write_private_file(&token_path, token.as_bytes())?;
        let control = UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let files = Files {
            socket_inode: std::fs::symlink_metadata(&socket)?.ino(),
            socket,
            token: token_path,
            _lock: lock_file,
        };
        let listener = TcpListener::bind(self.config.listen).await?;
        let control_task = tokio::spawn(super::control::accept(self.clone(), control, token));
        let members_task = tokio::spawn(super::members::run(self.clone()));
        let mut stop = self.stopping.subscribe();
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                _ = terminate.recv() => { let _ = self.stop().await; break; },
                _ = interrupt.recv() => { let _ = self.stop().await; break; },
                accepted = listener.accept() => {
                    let Ok((stream, address)) = accepted else { let _ = self.stop().await; break; };
                    let Ok(permit) = self.unauthenticated.clone().try_acquire_owned() else { continue; };
                    let Some(connection) = self.allocate_connection(address.ip()) else { continue; };
                    let gateway = self.clone(); let connection_task = connection.clone();
                    let finished = super::service::Finished { gateway: gateway.clone(), connection: connection.clone() };
                    let task = tokio::spawn(async move { let _finished = finished; super::ws::serve(gateway, stream, connection_task, permit).await; });
                    *lock(&connection.task) = Some(task);
                }
            }
        }
        // Connections must end before the listener and private control files.
        let _ = self.stop().await;
        drop(listener);
        let _ = control_task.await;
        let _ = members_task.await;
        drop(files);
        Ok(())
    }
}

pub fn foreground(config: super::Config) -> Result<()> {
    crew_core::client::refuse_in_agent_context()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(Gateway::open(config)?.run())
}
