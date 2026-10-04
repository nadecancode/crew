//! Regressions found in the first web server review.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reviewer_port_zero_does_not_report_an_unusable_address() {
    let (_temp, server) = server().await;
    let opened = server.web.open(Some(0)).await;
    server.web.stop(None).await;
    if let Ok(info) = opened {
        assert_ne!(
            info.port, 0,
            "web_open reported success with an unusable port"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reviewer_unauthenticated_deadline_survives_backpressure() {
    use std::os::fd::AsRawFd;
    let (_temp, server) = server().await;
    server.web.set_binder(Arc::new(|addr| {
        let listener = bind_listener(addr)?;
        let size: libc::c_int = 4096;
        // SAFETY: `size` is a valid integer socket option, kept alive for this call.
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    listener.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&size as *const libc::c_int).cast(),
                    std::mem::size_of_val(&size) as _,
                )
            },
            0
        );
        Ok(listener)
    }));
    server.web.open(None).await.unwrap();
    let serving = server.web.serving().unwrap();
    let mut socket = connect(&serving, V4).await;
    let request = json!({"id":1,"method":"x".repeat(3000)}).to_string();
    // Unknown methods receive an error. Both the request and its error are
    // bounded, but enough unread errors block the writer and then dispatch.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        for _ in 0..2000 {
            if socket.send(Message::text(request.clone())).await.is_err() {
                break;
            }
        }
    })
    .await;
    tokio::time::sleep(Duration::from_secs(8)).await;
    let count = server.web.registry().unauthenticated();
    server.web.stop(None).await;
    assert_eq!(
        count, 0,
        "the 5 s hello deadline stopped being checked while a reply was blocked"
    );
}
