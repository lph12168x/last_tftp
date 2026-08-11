# last-tftp 协议实现说明

## RFC 覆盖矩阵

| RFC | 名称 | 实现状态 |
|-----|------|----------|
| RFC 1350 | TFTP Protocol (Revision 2) | ✅ 完整 |
| RFC 2347 | Option Extension | ✅ 完整 |
| RFC 2348 | blksize 选项 | ✅ 完整 |
| RFC 2349 | timeout / tsize 选项 | ✅ 完整 |
| RFC 7440 | windowsize 选项 | ✅ client + server |
| RFC 2090 | TFTP Multicast | ❌ 未实现 |

## 报文格式

所有报文以 2 字节大端 opcode 开头；字符串字段以 `\0` 结尾。

### RRQ / WRQ（opcode=1 / 2）

```
+-----+--------+------+--------+----- ... +------+
|  1  |  fname \0     |  mode  \0  |  options...   |
+-----+--------+------+--------+----- ... +------+
  u16      string           string        可选
```

Options 格式（RFC 2347）：每个选项为 `name\0value\0`。

### DATA（opcode=3）

```
+-----+------+---------- ...
|  3  | blk# |  data ...
+-----+------+---------- ...
  u16   u16     N bytes
```

`blk#` 从 1 开始；最后一块 data < blksize 时标识传输结束。

### ACK（opcode=4）

```
+-----+------+
|  4  | blk# |
+-----+------+
  u16   u16
```

`blk# = 0` 用于确认 OACK。

### ERROR（opcode=5）

```
+-----+------+------- \0
|  5  | code |  msg
+-----+------+------- \0
  u16   u16    string
```

错误码（RFC 1350 §5 + RFC 2347）：
- 1 FileNotFound, 2 AccessViolation, 3 DiskFull
- 4 IllegalOperation, 5 UnknownTransferId, 6 FileAlreadyExists
- 7 NoSuchUser, 8 OptionNegotiation

### OACK（opcode=6）

```
+-----+---------- ...
|  6  |  options...
+-----+---------- ...
  u16    name\0value\0 ...
```

## 协商流程

```mermaid
sequenceDiagram
    participant C as Client
    participant S as Server

    C->>S: RRQ "file" octet [blksize=N timeout=M window=K]
    alt server 接受选项
        S->>C: OACK [blksize=N' timeout=M' window=K']
        C->>S: ACK(0)   ; RFC 2347
        S->>C: DATA(1) [+ ... + DATA(N) 最后一帧 < N']
        C->>S: ACK(N')  ; 窗口累积模式下 N' = last block
        S->>C: DATA(N'+1) ...
    else server 不调整选项
        S->>C: DATA(1) [+ ... + DATA(N) 最后一帧]
        C->>S: ACK(N)
    end
```

## 客户端 server 地址跟随

RFC 1350 §4：TID（Transfer ID）由对端首次响应的 source port 决定。客户端必须把后续报文发往 server 的 ephemeral port，而不是初始的 69。

代码实现（client.rs `negotiate_get`）：

```rust
loop {
    let (pkt, from) = recv_packet(&sock, timeout).await?;
    if from.port() != 0 {
        server.set_port(from.port());  // 跟随 server 实际端口
    }
    ...
}
```

## RFC 7440 滑动窗口

### RRQ 方向（client 攒 ACK）

```mermaid
sequenceDiagram
    participant C as Client
    participant S as Server

    Note over S: 服务器按 windowsize 块成批发
    S->>C: DATA(1) ... DATA(window)
    Note over C: 攒满窗口才发 ACK
    C->>S: ACK(window)
    S->>C: DATA(window+1) ... DATA(2*window)
    C->>S: ACK(2*window)
    ...
    S->>C: DATA(last)  ; < blksize
    C->>S: ACK(last)
```

### WRQ 方向（client 攒发）

```mermaid
sequenceDiagram
    participant C as Client
    participant S as Server

    Note over C: 连续发 windowsize 块
    C->>S: DATA(1) ... DATA(window)
    Note over S: 攒满窗口才发 ACK
    S->>C: ACK(window)
    C->>S: DATA(window+1) ...
    S->>C: ACK(2*window)
    ...
    C->>S: DATA(last)  ; < blksize
    S->>C: ACK(last)
```

## 服务器 socket 模型

主监听 socket 接受 RRQ/WRQ；每个 transfer 任务独立 bind ephemeral port，避免主循环与 task 在同一 socket 上抢 datagram：

```rust
// 主循环：只处理 RRQ/WRQ
loop {
    let (n, peer) = sock.recv_from(&mut buf).await?;
    let pkt = Packet::parse(&buf[..n])?;
    if !matches!(pkt, Packet::Rrq | Packet::Wrq) { continue; }
    tokio::spawn(handle_one(peer, pkt, cfg));
}

// 每连接独立 socket
async fn handle_one(peer, pkt, cfg) {
    let sock = UdpSocket::bind("[::]:0").await?;
    sock.connect(peer).await?;  // 后续 send 不指定目的地址
    serve_read(sock, peer, ...).await;
}
```

peer 比较时把 v4 地址视作 v4-mapped-v6，因为 dual-stack socket 在 v4 peer 上收到的 from 是 `[::ffff:127.0.0.1]:port`。

## 重传与超时

`transfer::with_retry` 实现指数退避：

- 起始 delay = `timeout`
- 每次失败 delay × 2，封顶 `timeout * 8`
- 最多 `retries` 次（默认 6）

仅对 `Protocol(...)` / `Timeout` 类错误重试；`Io` / `Remote` 错误立即返回。

## V1 已知限制

- **服务器 V1 整文件读**：`serve_read` 用 `tokio::fs::read` 一次性读入文件。中等文件（≤ 1GB）可接受；大文件需改造为流式 + spawn_blocking。
- **客户端 PUT 整窗口 ACK**：put 端 windowsize=1