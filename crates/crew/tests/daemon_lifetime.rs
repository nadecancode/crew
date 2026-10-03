//! The daemon's start and teardown in-process: what a start publishes, what
//! dropping it takes back, and how long the workspace lock lasts. One test
//! only: it points this process's XDG directories at a temporary root.

use std::os::fd::AsRawFd;
use std::path::Path;

/// Whether another process could take the workspace lock now.
fn lock_is_free(state_dir: &Path) -> bool {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(state_dir.join("daemon.lock"))
        .unwrap();
    // SAFETY: flock on a descriptor this function owns.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[test]
fn an_unserved_start_withdraws_its_files_and_live_handles_keep_the_lock() {
    let root = tempfile::Builder::new()
        .prefix("crew-l")
        .tempdir_in("/tmp")
        .unwrap();
    for (name, dir) in [
        ("HOME", "home"),
        ("XDG_RUNTIME_DIR", "run"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CONFIG_HOME", "config"),
    ] {
        let path = root.path().join(dir);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(name, path);
    }
    std::env::set_var("CREW_GLOBAL_CONFIG", root.path().join("config.toml"));
    std::env::remove_var("CREW_IDENTITY");
    std::env::remove_var("CREW_SOCKET");
    let workspace = root.path().join("ws");
    std::fs::create_dir(&workspace).unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let running = runtime
        .block_on(crew::daemon::start(crew::daemon::DaemonOptions {
            workspace: workspace.clone(),
            crew_bin: env!("CARGO_BIN_EXE_crew").into(),
        }))
        .unwrap();
    let state_dir = workspace.canonicalize().unwrap().join(".crew");
    let socket = running.socket().to_path_buf();
    let token = crew_core::paths::operator_token_path(&socket);
    let info = crew_core::paths::daemon_info_path(&state_dir);
    assert!(socket.exists() && token.exists() && info.exists());
    assert!(!lock_is_free(&state_dir));

    // A handle that outlives the start (as a connection task can).
    let handle = running.daemon.clone();
    drop(running);
    assert!(!socket.exists(), "the socket outlived its start");
    assert!(!token.exists(), "the token outlived its start");
    assert!(!info.exists(), "the registration outlived its start");
    assert!(
        !lock_is_free(&state_dir),
        "the lock was released while a daemon handle still uses the database"
    );

    drop(handle);
    assert!(lock_is_free(&state_dir));
}
