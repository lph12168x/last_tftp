//! GUI 端到端测试：headless 渲染 + in-process mock server。


use std::sync::Arc;
use std::time::Duration;

#[path = "../src/gui.rs"]
mod gui;

use gui::App;
use last_tftp_core::options::Negotiation;
use last_tftp_core::packet::Packet;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

const SERVER_TIMEOUT: Duration = Duration::from_secs(5);

fn render(app: &mut App) {
    let ctx = egui::Context::default();
    let _ = ctx.run(Default::default(), |_| {
        app.update_inner(&ctx);
    });
}

fn wait_transfer_done(app: &mut App, transfer_id: u64, timeout: Duration) -> (u64, Option<String>) {
    let ctx = egui::Context::default();
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > timeout {
            return (0, Some("test timeout".into()));
        }
        let _ = ctx.run(Default::default(), |_| {
            app.update_inner(&ctx);
        });
        if let Some(t) = app.transfers.iter().find(|t| t.id == transfer_id) {
            if t.done {
                return (t.bytes, t.error.clone());
            }
        } else {
            return (0, Some("transfer vanished".into()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn recv_with_timeout(sock: &Mutex<UdpSocket>, buf: &mut Vec<u8>) -> Result<(usize, std::net::SocketAddr), String> {
    tokio::time::timeout(SERVER_TIMEOUT, sock.lock().await.recv_from(buf))
        .await
        .map_err(|_| "server recv timeout".to_string())?
        .map_err(|e| format!("server recv error: {e}"))
}

async fn mock_server_rrq(content: Vec<u8>, blksize: u16) -> (u16, tokio::task::JoinHandle<()>) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = sock.local_addr().unwrap().port();
    let sock = Arc::new(Mutex::new(sock));
    let handle = tokio::spawn(async move {
        let sock = sock;
        let mut buf = vec![0u8; 65536];
        let (n, client) = match recv_with_timeout(&sock, &mut buf).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[mock server] {e}");
                return;
            }
        };
        let pkt = Packet::parse(&buf[..n]).unwrap();
        let mut options = match pkt {
            Packet::Rrq { options, .. } => options,
            _ => panic!("expected RRQ"),
        };
        options.set_blksize(blksize);
        let oack = Packet::Oack { options: options.clone() };
        send_to(&sock, &oack, client).await;

        let (n, from) = match recv_with_timeout(&sock, &mut buf).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[mock server] {e}");
                return;
            }
        };
        assert_eq!(from, client);
        assert!(matches!(Packet::parse(&buf[..n]).unwrap(), Packet::Ack { block: 0 }));

        let _neg = Negotiation::defaults().apply_oack(&options);
        let mut offset = 0;
        let mut block: u16 = 1;
        let total = content.len();
        loop {
            let end = (offset + blksize as usize).min(total);
            let chunk = &content[offset..end];
            let data = Packet::Data { block, data: bytes::Bytes::copy_from_slice(chunk) };
            send_to(&sock, &data, client).await;
            let (n, from) = match recv_with_timeout(&sock, &mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[mock server] {e}");
                    return;
                }
            };
            assert_eq!(from, client);
            match Packet::parse(&buf[..n]).unwrap() {
                Packet::Ack { block: b } if b == block => {}
                other => panic!("expected ACK({block}), got {other:?}"),
            }
            if (end - offset) < blksize as usize {
                break;
            }
            offset = end;
            block += 1;
        }
    });
    (port, handle)
}

async fn send_to(sock: &Arc<Mutex<UdpSocket>>, pkt: &Packet, dest: std::net::SocketAddr) {
    let bytes = pkt.encode();
    sock.lock().await.send_to(&bytes, dest).await.unwrap();
}

#[test]
fn gui_render_empty_state() {
    let mut app = App::headless_new();
    for _ in 0..5 {
        render(&mut app);
    }
    assert!(app.transfers.is_empty());
    assert!(app.log.is_empty());
}

#[test]
fn gui_get_end_to_end() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let payload: Vec<u8> = (0..20_000u32).map(|i| (i & 0xFF) as u8).collect();
        let (server_port, _server_handle) = mock_server_rrq(payload.clone(), 1468).await;

        let mut app = App::headless_new();
        app.remote_host = "127.0.0.1".into();
        app.remote_port = server_port;
        app.remote_file = "data.bin".into();
        app.local_file_str = std::env::temp_dir().join("last_tftp_gui_input_get.bin").display().to_string();
        app.blksize = 1468;
        app.window = 1;

        app.start_transfer_get_for_test();
        let (bytes, err) = wait_transfer_done(&mut app, 1, Duration::from_secs(10));


        assert!(err.is_none(), "GUI GET err: {err:?}");
        assert_eq!(bytes, payload.len() as u64);
        let got = std::fs::read(&app.local_file).unwrap();
        assert_eq!(got, payload);
    });
}

#[test]
fn gui_put_end_to_end() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let payload: Vec<u8> = (0..10_000u32).map(|i| ((i * 7) & 0xFF) as u8).collect();
        let src = std::env::temp_dir().join("last_tftp_gui_input_put_src.bin");
        std::fs::write(&src, &payload).unwrap();

        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_port = sock.local_addr().unwrap().port();
        let sock = Arc::new(Mutex::new(sock));

        let payload_for_test = payload.clone();
        let sock_clone = sock.clone();
        let server_handle = tokio::spawn(async move {
            let sock = sock_clone;
            let mut buf = vec![0u8; 65536];
            let (n, client) = match recv_with_timeout(&sock, &mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[mock server] {e}");
                    return;
                }
            };
            let options = match Packet::parse(&buf[..n]).unwrap() {
                Packet::Wrq { options, .. } => options,
                _ => panic!("expected WRQ"),
            };
            let oack = Packet::Oack { options };
            send_to(&sock, &oack, client).await;

            let mut received = Vec::new();
            let mut expected_block: u16 = 1;
            let blksize = 1024u16;
            loop {
                let (n, from) = match recv_with_timeout(&sock, &mut buf).await {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("[mock server] {e}");
                        break;
                    }
                };
                assert_eq!(from, client);
                match Packet::parse(&buf[..n]).unwrap() {
                    Packet::Data { block, data } => {
                        assert_eq!(block, expected_block);
                        received.extend_from_slice(&data);
                        let last = (data.len() as u16) < blksize;
                        send_to(&sock, &Packet::Ack { block }, client).await;
                        if last {
                            break;
                        }
                        expected_block += 1;
                    }
                    _ => panic!("unexpected"),
                }
            }
            assert_eq!(received, payload_for_test);
        });

        let mut app = App::headless_new();
        app.remote_host = "127.0.0.1".into();
        app.remote_port = server_port;
        app.remote_file = "remote.bin".into();
        app.local_file = src.clone();
        app.local_file_str = src.display().to_string();
        app.blksize = 1024;
        app.window = 1;

        app.start_transfer_put_for_test();
        let (bytes, err) = wait_transfer_done(&mut app, 1, Duration::from_secs(10));
        let _ = server_handle.await;

        assert!(err.is_none(), "GUI PUT err: {err:?}");
        assert_eq!(bytes, payload.len() as u64);
    });
}

#[test]
fn all_text_edit_singleline_targets_are_stable_string() {
    let src = std::fs::read_to_string("src/gui.rs").unwrap();
    let normalized: String = src.split_whitespace().collect::<Vec<_>>().join(" ");
    let fields = [
        "TextEdit::singleline(&mut self.remote_host)",
        "TextEdit::singleline(&mut self.remote_file)",
        "TextEdit::singleline(&mut self.server_root_str)",
        "TextEdit::singleline(&mut self.local_file_str)",
    ];
    for field in &fields {
        assert!(normalized.contains(field), "TextEdit must bind to stable String field: {field}");
    }
}