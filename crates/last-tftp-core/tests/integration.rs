//! 端到端集成测试：in-process client ↔ mock server 互通。

use std::sync::Arc;
use std::time::Duration;

use last_tftp_core::client::Client;
use last_tftp_core::error::TftpErrorCode;
use last_tftp_core::options::Negotiation;
use last_tftp_core::packet::Packet;
use last_tftp_core::session::ClientConfig;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

const SERVER_TIMEOUT: Duration = Duration::from_secs(5);

/// mock server recv，超时返回 Err。
async fn recv_with_timeout(
    sock: &Mutex<UdpSocket>,
    buf: &mut Vec<u8>,
) -> Result<(usize, std::net::SocketAddr), String> {
    tokio::time::timeout(SERVER_TIMEOUT, sock.lock().await.recv_from(buf))
        .await
        .map_err(|_| "server recv timeout".to_string())?
        .map_err(|e| format!("server recv error: {e}"))
}

/// 简易 mock server：只服务一个 client，先发 OACK 协商，再传文件。
async fn mock_server_rrq(content: Arc<Vec<u8>>, blksize: u16) -> (u16, tokio::task::JoinHandle<()>) {
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
        send_packet_sync(&sock, &oack, client).await;

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
            send_packet_sync(&sock, &data, client).await;
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
            if end - offset < blksize as usize {
                break;
            }
            offset = end;
            block += 1;
        }
    });
    (port, handle)
}

async fn send_packet_sync(sock: &Arc<Mutex<UdpSocket>>, pkt: &Packet, dest: std::net::SocketAddr) {
    let bytes = pkt.encode();
    sock.lock().await.send_to(&bytes, dest).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_get_with_oack() {
    let content = Arc::new(vec![0xAB; 10_000]);
    let (server_port, server_handle) = mock_server_rrq(content.clone(), 1024).await;

    let tmp = std::env::temp_dir().join("last_tftp_iget.bin");
    let _ = std::fs::remove_file(&tmp);

    let cfg = ClientConfig {
        blksize: 512,
        timeout_secs: 3,
        windowsize: 1,
        retries: 4,
        resume_from_block: 0,
    };
    let client = Client::new(cfg);
    let stats = client.get("127.0.0.1:hello.bin", server_port, &tmp).await.expect("get");
    let _ = server_handle.await;

    assert_eq!(stats.bytes, 10_000);
    let got = std::fs::read(&tmp).unwrap();
    assert_eq!(got.len(), 10_000);
    assert!(got.iter().all(|b| *b == 0xAB));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_put_with_oack() {
    let payload: Vec<u8> = (0..20_000u32).map(|i| (i * 7) as u8).collect();
    let src = std::env::temp_dir().join("last_tftp_iput_src.bin");
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
        let pkt = Packet::parse(&buf[..n]).unwrap();
        let (filename, options, total_size) = match pkt {
            Packet::Wrq { filename, mode: _, options } => {
                let total = options.tsize().unwrap_or(0);
                (filename, options, total)
            }
            _ => panic!("expected WRQ"),
        };
        assert_eq!(filename, "fw.bin");
        let _blksize = options.blksize().unwrap_or(512);
        let neg = Negotiation::defaults().apply_oack(&options);
        let oack = Packet::Oack { options };
        send_packet_sync(&sock, &oack, client).await;

        let mut received = Vec::new();
        let mut expected_block: u16 = 1;
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
                    let last = (data.len() as u64) < u64::from(neg.blksize);
                    send_packet_sync(&sock, &Packet::Ack { block }, client).await;
                    if last {
                        break;
                    }
                    expected_block += 1;
                }
                _ => panic!("unexpected"),
            }
        }
        let _ = total_size;
        assert_eq!(received, payload_for_test);
    });

    let cfg = ClientConfig {
        blksize: 512,
        timeout_secs: 3,
        windowsize: 1,
        retries: 4,
        resume_from_block: 0,
    };
    let client = Client::new(cfg);
    client.put(&src, "127.0.0.1:fw.bin", server_port).await.expect("put");
    let _ = server_handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_error_propagates() {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_port = sock.local_addr().unwrap().port();
    let sock = Arc::new(Mutex::new(sock));

    let server_handle = tokio::spawn(async move {
        let sock = sock;
        let mut buf = vec![0u8; 65536];
        let (_n, client) = match recv_with_timeout(&sock, &mut buf).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[mock server] {e}");
                return;
            }
        };
        let err = Packet::Error { code: TftpErrorCode::FileNotFound, message: "nope".into() };
        send_packet_sync(&sock, &err, client).await;
    });

    let tmp = std::env::temp_dir().join("last_tftp_err.bin");
    let _ = std::fs::remove_file(&tmp);
    let cfg = ClientConfig {
        blksize: 512,
        timeout_secs: 2,
        windowsize: 1,
        retries: 2,
        resume_from_block: 0,
    };
    let client = Client::new(cfg);
    let res = client.get("127.0.0.1:missing.bin", server_port, &tmp).await;
    let _ = server_handle.await;
    assert!(matches!(res, Err(last_tftp_core::TftpError::Remote { .. })));
}
/// 丢包模拟的 mock server：每隔 `drop_every_n` 个包就丢掉一个不发。
async fn mock_server_rrq_with_loss(
    content: Arc<Vec<u8>>,
    blksize: u16,
    drop_every_n: u32,
) -> (u16, tokio::task::JoinHandle<()>) {
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
        send_packet_sync(&sock, &oack, client).await;

        let (n, from) = match recv_with_timeout(&sock, &mut buf).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[mock server] {e}");
                return;
            }
        };
        assert_eq!(from, client);
        assert!(matches!(Packet::parse(&buf[..n]).unwrap(), Packet::Ack { block: 0 }));

        let neg = Negotiation::defaults().apply_oack(&options);
        let mut offset = 0usize;
        let mut block: u16 = 1;
        let total = content.len();
        let mut sent_count: u32 = 0;
        loop {
            let end = (offset + blksize as usize).min(total);
            let chunk = &content[offset..end];
            sent_count += 1;
            if drop_every_n > 0 && sent_count % drop_every_n == 0 {
                // 丢包：直接读 ACK，client 收到跳号会重 ACK prev
            } else {
                let data = Packet::Data {
                    block,
                    data: bytes::Bytes::copy_from_slice(chunk),
                };
                send_packet_sync(&sock, &data, client).await;
            }
            let (n, from) = match recv_with_timeout(&sock, &mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[mock server] {e}");
                    return;
                }
            };
            assert_eq!(from, client);
            match Packet::parse(&buf[..n]).unwrap() {
                Packet::Ack { block: b } => {
                    if b == block {
                        if end - offset < blksize as usize {
                            break;
                        }
                        offset = end;
                        block += 1;
                    } else if b + 1 == block {
                        // client 重 ACK prev，请求 server 重发当前 block
                        let data = Packet::Data {
                            block,
                            data: bytes::Bytes::copy_from_slice(chunk),
                        };
                        send_packet_sync(&sock, &data, client).await;
                    } else {
                        panic!("unexpected ACK({b}), want {block} or {}", block - 1);
                    }
                }
                other => panic!("expected ACK, got {other:?}"),
            }
        }
    });
    (port, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "long-running, run with: cargo test -- --ignored"]
async fn end_to_end_get_with_packet_loss() {
    // 20MB 内容，blksize=1468，每 17 个 block 丢一个（约 1.1% 丢包率）
    // 验证 client 能通过 RFC 1350 ACK 语义从丢包中恢复。
    let content: Arc<Vec<u8>> =
        Arc::new((0..20u32 * 1024 * 1024).map(|i| (i * 31) as u8).collect());
    let (server_port, server_handle) =
        mock_server_rrq_with_loss(content.clone(), 1468, 137).await;

    let tmp = std::env::temp_dir().join("last_tftp_loss.bin");
    let _ = std::fs::remove_file(&tmp);

    let cfg = ClientConfig {
        blksize: 1468,
        timeout_secs: 3,
        windowsize: 1,
        retries: 6,
        resume_from_block: 0,
    };
    let client = Client::new(cfg);
    let stats = client
        .get("127.0.0.1:fw.bin", server_port, &tmp)
        .await
        .expect("get with loss");
    let _ = server_handle.await;

    assert_eq!(stats.bytes as usize, content.len());
    let got = std::fs::read(&tmp).unwrap();
    assert_eq!(got.len(), content.len());
    assert_eq!(got, *content);
}
