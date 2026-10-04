//! Regressions found in the first web server review.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reviewer_port_zero_does_not_report_an_unusable_address() {
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    let opened = daemon.web.open(&daemon, Some(0)).await;
    daemon.web.stop(&daemon, None).await;
    if let Ok(info) = opened {
        assert_ne!(
            info.port, 0,
            "web_open reported success with an unusable port"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reviewer_stop_reclaims_a_subscriber_blocked_on_its_reply() {
    use std::os::fd::AsRawFd;
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    daemon.web.set_binder(Arc::new(|addr| {
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
    let info = daemon.web.open(&daemon, None).await.unwrap();
    let serving = daemon.web.serving().unwrap();
    let mut socket = connect(&serving, V4).await;
    let paired = call(&mut socket, 1, "web_pair", json!({"code": code_of(&info)})).await;
    let token = paired["result"]["token"].as_str().unwrap();
    assert!(hello(&mut socket, token).await["result"].is_object());
    let before = daemon.push.receiver_count();
    call(&mut socket, 2, "subscribe", json!({})).await;
    for i in 0..2200 {
        daemon.push(crew_core::rpc::Push::Pty {
            data: "x".repeat(16 * 1024),
        });
        if i % 32 == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The reply waits for space while the peer reads nothing.
    socket
        .send(Message::text(json!({"id":3,"method":"ping"}).to_string()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(daemon.web.stop(&daemon, None).await);
    assert!(daemon.web.registry().conns.is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    daemon.push(crew_core::rpc::Push::Pty {
        data: "REVIEW_AFTER_STOP".into(),
    });
    let delivered_after_stop = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Text(text) = message {
                if text.contains("REVIEW_AFTER_STOP") {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(
        !delivered_after_stop,
        "web subscriber received a new push after web.stop returned"
    );
    assert_eq!(
        daemon.push.receiver_count(),
        before,
        "web stop released listeners and registration, but the subscribed forwarder survives"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reviewer_failed_hello_write_does_not_consume_auth_capacity() {
    use crate::daemon::server::{Session, Transport};
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    let info = daemon.web.open(&daemon, None).await.unwrap();
    let token = daemon.web.pair(&daemon, &code_of(&info)).unwrap().token;
    let admitted = daemon.web.admit(&daemon).unwrap();
    let (mut session, frames) = Session::new(&daemon, Transport::Web);
    drop(frames);
    assert!(
        !session
            .request(
                &json!({"id":1,"method":"hello","params":{
        "role":"operator","token":token,"protocol":PROTOCOL_VERSION}})
                .to_string()
            )
            .await
    );
    // This is ws::run's `request == false` branch: no authenticated(id) call.
    session.close();
    drop(admitted);
    let authenticated = daemon.web.registry().authenticated;
    daemon.web.stop(&daemon, None).await;
    assert_eq!(
        authenticated, 0,
        "a failed reply consumed an authenticated slot forever"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reviewer_unauthenticated_deadline_survives_backpressure() {
    use std::os::fd::AsRawFd;
    let (_temp, running) = daemon().await;
    let daemon = running.daemon.clone();
    daemon.web.set_binder(Arc::new(|addr| {
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
    daemon.web.open(&daemon, None).await.unwrap();
    let serving = daemon.web.serving().unwrap();
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
    let count = daemon.web.registry().unauthenticated;
    daemon.web.stop(&daemon, None).await;
    assert_eq!(
        count, 0,
        "the 5 s hello deadline stopped being checked while a reply was blocked"
    );
}
