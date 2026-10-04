//! A workspace connection's daemon lines, batched (notes/web-multi.md
//! §multi-transport "Batches", "A slow browser, and what the server holds
//! for it"; §multi-tests "Batches"). The stand-in daemon writes what each
//! test says in one write; the browser is a real WebSocket on loopback, read
//! as messages or, where what the server wrote matters to the byte, as the
//! raw stream.

use super::*;

use crew_core::MAX_FRAME_BYTES;
use tokio::io::AsyncReadExt;

/// The read buffer of a daemon connection (`daemons::say_hello`).
const READ_BUFFER: usize = 64 * 1024;
/// What a stand-in daemon's pipe holds (`Fake::dial`).
const PIPE: usize = 64 * 1024;
/// At most this many lines make a batch (`page::BATCH`).
const BATCH: usize = 256;

/// A daemon, its workspace joined, and a browser's connection to it.
struct Piped {
    fake: Arc<Fake>,
    stand: Arc<Stand>,
    served: Served,
    token: String,
    ws: Ws,
}

async fn piped() -> Piped {
    let fake = Fake::new();
    let stand = fake.start("a", 100);
    let served = Served::new(&fake).await;
    served.open("a").await;
    let token = served.pair().await;
    let ws = served.workspace(&token, "a").await;
    Piped {
        fake,
        stand,
        served,
        token,
        ws,
    }
}

/// `n` lines, each unlike the others, with what an unparsed pipe must keep:
/// spaces at both ends, a tab, multibyte characters, and no JSON.
fn lines(tag: &str, n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("  {tag} {i}/{n}\tdéjà 文 {}  ", "·".repeat(i % 7)))
        .collect()
}

/// A line of exactly `MAX_FRAME_BYTES` bytes, multibyte characters in it.
fn longest(tag: char) -> String {
    let mut line = String::with_capacity(MAX_FRAME_BYTES);
    for c in [tag, 'é', '文', ' '].into_iter().cycle() {
        if line.len() + c.len_utf8() > MAX_FRAME_BYTES {
            break;
        }
        line.push(c);
    }
    while line.len() < MAX_FRAME_BYTES {
        line.push('x');
    }
    line
}

/// What a daemon sends in one write.
fn one_write(lines: &[String]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

/// The next text message.
async fn text(ws: &mut Ws) -> String {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a message within 5 s")
            .expect("the socket is open")
            .expect("a message");
        match message {
            Message::Text(text) => return text.as_str().to_string(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("not a text message: {other:?}"),
        }
    }
}

/// Every text message until the connection ends, and its close code.
async fn until_end(ws: &mut Ws) -> (Vec<String>, Option<u16>) {
    let mut texts = Vec::new();
    let code = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => texts.push(text.as_str().to_string()),
                Some(Ok(Message::Close(frame))) => return frame.map(|f| u16::from(f.code)),
                None | Some(Err(_)) => return None,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .expect("the end within 5 s");
    (texts, code)
}

/// Bytes the kernel holds between the server and the browser: what the
/// server's socket has not had acknowledged and what the browser's has not
/// read (`/proc/net/tcp`).
fn in_the_kernel(server_port: u16, browser_port: u16) -> usize {
    let table = std::fs::read_to_string("/proc/net/tcp").expect("/proc/net/tcp");
    let port =
        |address: &str| u16::from_str_radix(address.rsplit(':').next().unwrap(), 16).unwrap();
    let mut sides = (None, None);
    for row in table.lines().skip(1) {
        let fields: Vec<&str> = row.split_whitespace().collect();
        let (local, remote) = (port(fields[1]), port(fields[2]));
        let (tx, rx) = fields[4].split_once(':').unwrap();
        let (tx, rx) = (
            usize::from_str_radix(tx, 16).unwrap(),
            usize::from_str_radix(rx, 16).unwrap(),
        );
        if (local, remote) == (server_port, browser_port) {
            sides.0 = Some(tx);
        } else if (local, remote) == (browser_port, server_port) {
            sides.1 = Some(rx);
        }
    }
    sides.0.expect("the server's socket") + sides.1.expect("the browser's socket")
}

/// Until the stand-in daemon's flood stops being taken: what it wrote.
async fn stalled(stand: &Stand) -> usize {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let now = stand.flooded.load(Ordering::SeqCst) as usize;
        if now > 0 && now == last {
            return now;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon's writes were still taken after 5 s: {now} bytes"
        );
        last = now;
    }
}

/// A frame the server wrote, and where in the stream it ends.
#[derive(Debug)]
struct Frame {
    opcode: u8,
    payload: Vec<u8>,
    end: usize,
}

/// The whole frames in what the server wrote (unmasked, unfragmented), and
/// whether a cut one follows them.
fn frames(stream: &[u8]) -> (Vec<Frame>, bool) {
    let mut frames = Vec::new();
    let mut at = 0;
    while at < stream.len() {
        let Some(&[first, second]) = stream.get(at..at + 2) else {
            return (frames, true);
        };
        assert_eq!(first & 0x80, 0x80, "a final frame at {at}");
        assert_eq!(second & 0x80, 0, "the server masks nothing");
        let (len, header) = match second & 0x7f {
            126 => match stream.get(at + 2..at + 4) {
                Some(b) => (u16::from_be_bytes([b[0], b[1]]) as usize, 4),
                None => return (frames, true),
            },
            127 => match stream.get(at + 2..at + 10) {
                Some(b) => (u64::from_be_bytes(b.try_into().unwrap()) as usize, 10),
                None => return (frames, true),
            },
            len => (len as usize, 2),
        };
        let Some(payload) = stream.get(at + header..at + header + len) else {
            return (frames, true);
        };
        at += header + len;
        frames.push(Frame {
            opcode: first & 0x0f,
            payload: payload.to_vec(),
            end: at,
        });
    }
    (frames, false)
}

/// The batch bound of "A slow browser" for lines of `line` bytes, `\n`
/// included: one line, then complete lines the read buffer held, 255 of
/// them at most.
fn batch_bound(line: usize) -> usize {
    line + ((BATCH - 1) * line).min(READ_BUFFER)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lines_sent_in_one_write_arrive_as_as_many_messages_in_order_byte_for_byte() {
    let Piped { stand, mut ws, .. } = piped().await;
    for n in [1, 2, 256, 257, 1000] {
        let sent = lines(&format!("n{n}"), n);
        stand.write_raw(one_write(&sent));
        for (i, line) in sent.iter().enumerate() {
            assert_eq!(&text(&mut ws).await, line, "line {i} of {n}");
        }
    }
    // Nothing else came: the next line is the next message.
    stand.write_raw(b"after\n".to_vec());
    assert_eq!(text(&mut ws).await, "after");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_as_long_as_a_frame_may_be_passes_first_last_and_in_the_middle_of_a_write() {
    let Piped { stand, mut ws, .. } = piped().await;
    let short = |tag: &str| lines(tag, 300);
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("first", [vec![longest('f')], short("f")].concat()),
        ("last", [short("l"), vec![longest('l')]].concat()),
        (
            "middle",
            [short("m1"), vec![longest('m')], short("m2")].concat(),
        ),
        (
            "every place",
            [
                vec![longest('a')],
                short("e1"),
                vec![longest('b')],
                short("e2"),
                vec![longest('c')],
            ]
            .concat(),
        ),
    ];
    for (case, sent) in cases {
        stand.write_raw(one_write(&sent));
        for (i, line) in sent.iter().enumerate() {
            let got = text(&mut ws).await;
            assert!(
                got == *line,
                "{case}: line {i}: {} bytes, not the {} sent",
                got.len(),
                line.len()
            );
        }
    }
    stand.write_raw(b"after\n".to_vec());
    assert_eq!(text(&mut ws).await, "after");
}

/// Time stands still while this lives, for 20 s of real time at most: a
/// blocking task keeps a paused clock from advancing by itself.
struct Frozen {
    _thaw: std::sync::mpsc::Sender<()>,
}

fn freeze() -> Frozen {
    let (thaw, frozen) = std::sync::mpsc::channel::<()>();
    tokio::task::spawn_blocking(move || {
        let _ = frozen.recv_timeout(std::time::Duration::from_secs(20));
    });
    Frozen { _thaw: thaw }
}

#[tokio::test(start_paused = true)]
async fn a_line_sent_alone_is_delivered_at_once_with_no_timer() {
    let frozen = freeze();
    let since = Instant::now();
    let Piped { stand, mut ws, .. } = piped().await;
    // With the clock still, a line that waited for a timer would wait for
    // ever (and, the clock freed after 20 s, fail `text`'s 5 s).
    for line in ["alone", "and another, alone"] {
        stand.write_raw(format!("{line}\n").into_bytes());
        assert_eq!(text(&mut ws).await, line);
    }
    assert_eq!(Instant::now(), since, "no time passed");
    drop(frozen);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_that_is_not_utf8_ends_the_connection_after_the_lines_before_it() {
    let Piped {
        fake,
        stand,
        served,
        token,
        ws,
    } = piped().await;
    drop(ws);
    // Before it: none, a few (one batch), more than a batch holds.
    for (before, after) in [(0, 5), (10, 10), (300, 300)] {
        let mut ws = served.workspace(&token, "a").await;
        let conn = *web_conns(&fake, 100).last().unwrap();
        let first = lines(&format!("before{before}"), before);
        let rest = lines(&format!("after{before}"), after);
        let mut bytes = one_write(&first);
        bytes.extend_from_slice(b"not \xff\xfe UTF-8\n");
        bytes.extend_from_slice(&one_write(&rest));
        stand.write_raw(bytes);
        let (texts, code) = until_end(&mut ws).await;
        assert_eq!(texts.len(), before, "{before} lines before it");
        assert_eq!(texts, first);
        assert_eq!(code, Some(CLOSE_WORKSPACE_ENDED));
        eventually("the daemon's side let go", || fake.let_go(conn).is_some()).await;
    }
}

/// The browser reads nothing while the daemon writes lines of `len` bytes
/// as fast as they are taken: they stop being taken, and what the server
/// holds is within the bound.
async fn a_browser_that_does_not_read(len: usize) {
    let Piped {
        stand,
        served,
        token,
        mut ws,
        ..
    } = piped().await;
    let browser_port = ws.get_ref().local_addr().unwrap().port();
    let request = json!({"id": 2, "method": "flood_lines", "params": {"len": len}});
    ws.send(Message::text(request.to_string())).await.unwrap();
    let taken = stalled(&stand).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        stand.flooded.load(Ordering::SeqCst) as usize,
        taken,
        "the daemon's writes are no longer taken"
    );
    // The server still answers.
    let mut index = served.ws().await;
    assert!(hello(&mut index, &token, None).await["result"]["web_proof"].is_string());
    assert_eq!(
        call(&mut index, 2, "ping", json!({})).await["result"]["pong"],
        true
    );
    // What the kernel holds, in the daemon's bytes: its frames are read
    // (and no further), each line's `\n` counted back in.
    let kernel = in_the_kernel(served.serving.port, browser_port);
    let mut stream = Vec::new();
    let mut buf = vec![0; 256 * 1024];
    while stream.len() < kernel {
        let n = ws.get_mut().read(&mut buf).await.unwrap();
        assert!(n > 0, "the connection stands");
        stream.extend_from_slice(&buf[..n]);
    }
    stream.truncate(kernel);
    let (whole, cut) = frames(&stream);
    let mut in_kernel = 0;
    for (seq, frame) in whole.iter().enumerate() {
        assert_eq!(frame.opcode, 1, "only the daemon's lines");
        assert!(frame.payload == flood_line(seq as u64, len).into_bytes());
        in_kernel += frame.payload.len() + 1;
    }
    if cut {
        // The start of one more line: its payload, past its header.
        let rest = &stream[whole.last().map_or(0, |f| f.end)..];
        let header = match rest.get(1).map(|b| b & 0x7f) {
            Some(126) => 4,
            Some(127) => 10,
            _ => 2,
        };
        in_kernel += rest.len().saturating_sub(header);
    }
    let bound = 3 * batch_bound(len + 1) + READ_BUFFER;
    let held = taken - in_kernel;
    assert!(
        held <= bound + PIPE,
        "{taken} bytes taken from the daemon, {in_kernel} of them in the kernel: \
         {held} held, more than the server's {bound} and the pipe's {PIPE}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_browser_that_does_not_read_bounds_what_is_held_for_short_lines() {
    a_browser_that_does_not_read(100).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_browser_that_does_not_read_bounds_what_is_held_for_the_longest_lines() {
    a_browser_that_does_not_read(MAX_FRAME_BYTES).await;
}

/// The browser stops reading; the daemon's lines fill what the server may
/// hold, so that its connection waits to queue a batch; then the workspace
/// is removed. `reads_again`: the browser reads again as soon as the
/// revocation is made, or only once the removal answered (the forced end).
/// Either way the server writes at most two batches after the revocation,
/// then the end; and reads nothing more from the daemon.
async fn revoked_while_waiting_to_queue_a_batch(reads_again: bool) {
    let Piped {
        fake,
        stand,
        served,
        mut ws,
        ..
    } = piped().await;
    let conn = web_conns(&fake, 100)[0];
    let browser_port = ws.get_ref().local_addr().unwrap().port();
    // Longer than the read buffer: one line a batch.
    let len = READ_BUFFER + 4096;
    let request = json!({"id": 2, "method": "flood_lines", "params": {"len": len}});
    ws.send(Message::text(request.to_string())).await.unwrap();
    let taken = stalled(&stand).await;
    let started = Instant::now();
    let removal = tokio::spawn({
        let hub = served.hub();
        let web = served.server.web.clone();
        async move {
            let result = hub.remove(&id("a"), &web).await;
            (result, Instant::now())
        }
    });
    let hub = served.hub();
    eventually("the revocation", || hub.listing().joined.is_empty()).await;
    let held = in_the_kernel(served.serving.port, browser_port);
    if !reads_again {
        eventually("the removal's answer", || removal.is_finished()).await;
    }
    let mut stream = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        let mut buf = vec![0; 256 * 1024];
        while let Ok(n @ 1..) = ws.get_mut().read(&mut buf).await {
            stream.extend_from_slice(&buf[..n]);
        }
    })
    .await;
    assert!(read.is_ok(), "the connection ended within 5 s");
    let (result, answered) = removal.await.unwrap();
    result.unwrap();
    assert!(fake.let_go(conn).expect("let go") <= answered);
    let took = answered - started;
    if !reads_again {
        assert!(took >= LET_GO_WITHIN, "the forced end: {took:?}");
        assert!(took < LET_GO_WITHIN + Duration::from_secs(1), "{took:?}");
    }
    assert_eq!(
        stand.flooded.load(Ordering::SeqCst) as usize,
        taken,
        "nothing was read from the daemon after the revocation"
    );
    let (frames, cut) = frames(&stream);
    let mut texts = 0;
    let mut closed = None;
    for frame in &frames {
        assert!(closed.is_none(), "a frame after the close: {frame:?}");
        match frame.opcode {
            1 => {
                assert!(
                    frame.payload == flood_line(texts as u64, len).into_bytes(),
                    "line {texts} as the daemon sent it"
                );
                texts += 1;
            }
            8 => closed = Some(u16::from_be_bytes([frame.payload[0], frame.payload[1]])),
            9 | 10 => {}
            opcode => panic!("opcode {opcode}"),
        }
    }
    let after: Vec<&Frame> = frames
        .iter()
        .filter(|f| f.opcode == 1 && f.end > held)
        .collect();
    assert!(
        after.len() <= 2,
        "{} batches written after the revocation, of {texts} lines; {held} bytes were in the kernel",
        after.len()
    );
    if reads_again {
        assert!(!cut, "whole frames, then the close");
        assert_eq!(closed, Some(CLOSE_WORKSPACE_REMOVED));
    } else {
        // Cancelled: its writer was ended too, perhaps in a frame.
        assert_eq!(closed, None);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoked_while_waiting_to_queue_a_batch_it_is_cancelled_and_sends_no_more() {
    revoked_while_waiting_to_queue_a_batch(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoked_while_waiting_to_queue_a_batch_a_browser_reading_again_gets_two_batches_at_most() {
    revoked_while_waiting_to_queue_a_batch(true).await;
}
