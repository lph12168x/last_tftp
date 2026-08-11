# last-tftp 产品方案设计

## 1. 目标

跨平台现代 TFTP 工具，提供桌面 GUI、内置 TFTP 服务器、CLI 客户端三种形态共用同一协议库核心。覆盖 RFC 1350 / 2347 / 2348 / 2349 / 7440，IPv4 + IPv6 双栈，断点续传，进度条与吞吐统计。

## 2. 用户与场景

| 角色 | 场景 | 形态 |
|------|------|------|
| 嵌入式/网络设备工程师 | 给交换机、路由器、IoT 网关上下载/上传固件 | GUI + CLI |
| 运维 | 临时在内网拉取 PXE 镜像、配置文件 | CLI |
| 实验室 | 一台机器当 server，另一台当 client 自联调 | GUI server + CLI client |
| 普通用户 | 点按钮完成一次文件收发 | GUI |

## 3. 技术栈

- 语言：**Rust 2021 edition**（安全、性能、单二进制分发）
- 异步运行时：**tokio**（UDP 收发、超时、重试）
- 协议核心：`last-tftp-core`（无 GUI 依赖的纯协议库）
- CLI：`clap` v4（derive 风格，子命令：`get` / `put` / `server` / `probe`）
- GUI：**egui + eframe**（即时模式，与 tokio 同进程，无需 webview）
- 进度条：`indicatif`（CLI 端），egui 端用 `ProgressBar`
- 日志：`tracing` + `tracing-subscriber`
- 测试：`tokio::test` + 真实回环 UDP 套接字；外加 wire-format 字节级断言

## 4. 仓库结构

```
last-tftp/
├── Cargo.toml                  # workspace 根
├── DESIGN.md                   # 本文档
├── README.md
├── crates/
│   ├── last-tftp-core/         # 协议核心库（无 IO 之外依赖）
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── packet.rs       # 报文编解码
│   │   │   ├── opcode.rs
│   │   │   ├── options.rs      # 选项协商
│   │   │   ├── error.rs
│   │   │   └── session.rs      # client/server 会话状态机
│   │   └── tests/
│   └── last-tftp/              # 可执行二进制（CLI + GUI）
│       ├── src/
│       │   ├── main.rs         # 入口分派
│       │   ├── cli.rs          # clap 命令定义
│       │   ├── commands/       # get / put / server / probe 实现
│       │   ├── gui/            # eframe 应用
│       │   └── progress.rs     # indicatif + GUI 适配
│       └── tests/
└── docs/
    └── protocol-notes.md       # 报文格式与抓包样本
```

## 5. 协议设计

### 5.1 报文（packet.rs）

| Opcode | 方向 | 载荷 | 说明 |
|--------|------|------|------|
| RRQ    | C→S | string filename, string mode, options... | 读请求 |
| WRQ    | C→S | string filename, string mode, options... | 写请求 |
| DATA   | ↔   | u16 block#, u8[N] data | N ≤ negotiated blksize |
| ACK    | ↔   | u16 block# | |
| ERROR  | ↔   | u16 code, string msg | |
| OACK   | S→C | options... | 选项应答 |

字段编码：2 字节 opcode（big-endian） + 字符串（NUL 结尾） + 选项对（name\0value\0）。

### 5.2 选项（options.rs）

按 RFC 2347 OACK 协商：

| 选项 | 默认 | 范围 | 用途 |
|------|------|------|------|
| blksize   | 512 | 8..=65464 | 单块字节数 |
| timeout   | 5   | 1..=255   | 重传秒数 |
| tsize     | -   | ≥ 0       | 传输总字节（put 时由 client 宣告，get 时由 server 宣告） |
| windowsize | 1  | 1..=65535 | 一次未确认块数（RFC 7440） |

协商规则：客户端请求 → 服务端 OACK 应答，仅接受服务端裁定的值；任一不识别选项 → 忽略该选项继续传输。

### 5.3 状态机（session.rs）

```
Client Read:    SendRRQ → RecvOACK? → RecvDATA* → RecvDATA(len<blksize) → Done
Client Write:   SendWRQ → RecvOACK? → SendDATA* → SendDATA(len<blksize) → RecvACK → Done
Server Read:    RecvRRQ → SendOACK? → RecvDATA* → RecvDATA(len<blksize) → SendACK → Done
Server Write:   RecvWRQ → SendOACK? → SendDATA* → SendDATA(len<blksize) → RecvACK → Done
```

超时与重传：使用指数退避（1×timeout, 2×, 4×…，上限 64×），最多 6 次后报 TimeoutError。

### 5.4 断点续传

基于块编号：客户端维护 `.tftp-state.json`，记录 `(remote_file, peer, last_ack_block)`。续传时：

- 读：发 RRQ，若服务端支持则 OACK 中含 `tsize`，从 `last_ack_block+1` 起请求（通过对端协商扩展；先实现"整文件重传"，扩展点留给 V2）。
- 写：发 RRQ 时携带 `tsize=remaining`，从 offset 0 开始继续写完剩余部分。**V1 仅实现"读侧整文件缓存重传 + 写侧剩余大小续传"。**

### 5.5 IPv6

socket 创建时优先尝试 dual-stack：

```rust
let std = UdpSocket::bind("[::]:0")?;
std.set_only_v6(false)?;  // Linux/macOS
```

服务器监听 `[::]:69`（或 fallback 0.0.0.0:69）。客户端默认同时解析 A 与 AAAA，按 Happy Eyeballs 简化版依次尝试。

## 6. CLI 设计

```
last-tftp server [--root DIR] [--port 69] [--bind ADDR] [--ipv6] [--allow-write]
last-tftp get   HOST:FILE [-o OUT] [--port 69] [--blksize 1468] [--timeout 5]
                [--window 1] [--ipv6] [--resume] [--no-progress] [--json]
last-tftp put   LOCAL_FILE HOST:FILE [--port 69] [--blksize 1468] [--timeout 5]
                [--window 1] [--ipv6] [--resume] [--no-progress] [--json]
last-tftp probe HOST [--port 69] [--ipv6]      # 仅协商一次 OACK，探测能力
last-tftp gui                                       # 启动 eframe 桌面应用
```

`--json` 输出机器可读统计 `{bytes, blocks, duration_ms, throughput_bps}`。

## 7. GUI 设计（egui）

主窗口分四区：

```
┌──────────────────────────────────────────────────────────────┐
│ [Server] [Client] [Transfers] [Log]                          │
├──────────────────────────────────────────────────────────────┤
│ Server Tab:   root dir picker, bind addr, port, allow-write  │
│               ▶ Start / ⏹ Stop   status: Listening on [::]:69│
├──────────────────────────────────────────────────────────────┤
│ Client Tab:   remote host, remote file, local save           │
│               blksize / timeout / window spinners             │
│               [Get] [Put] [Probe]                             │
├──────────────────────────────────────────────────────────────┤
│ Transfers:    live table: id | direction | file | progress | │
│               throughput | ETA | status                       │
├──────────────────────────────────────────────────────────────┤
│ Log:          tracing 输出滚动面板                            │
└──────────────────────────────────────────────────────────────┘
```

每条 transfer 通过 tokio channel 把进度事件喂给 UI 线程，egui 即时重绘。

## 8. 验收标准（按你选定的验证手段）

1. **单元测试覆盖状态机**：`last-tftp-core/tests/` 中：
   - 报文编解码往返
   - 选项协商（接受/拒绝/忽略）
   - 丢包模拟：跳过块 N 验证重传
   - 超时模拟：故意延迟应答验证指数退避
   - windowsize=1/8/65535 三档正确性
2. **自包含联调**：`cargo run -- server --root ./test_data &` 然后 `cargo run -- get 127.0.0.1:big.bin -o /tmp/out`，与 tftpd64/atftpd 互通不验证但留出 hook。
3. **抓包对比**：用 tcpdump 录一段 RRQ+OACK+DATA+ACK 流，导出 pcap 摘要写到 `docs/protocol-notes.md`。
4. **大文件与边界**：1KB / 512B 整块 / 513B 多一块 / 整 4GB 极限、blksize=8 与 65464 都跑通。

## 9. 实现阶段（每阶段一个 git 提交单位）

| 阶段 | 内容 | 结束条件 |
|------|------|----------|
| P0 仓库骨架 | workspace + crate 结构 + DESIGN/README | `cargo build` 通过 |
| P1 报文编解码 | packet.rs/opcode.rs + 单元测试 | `cargo test -p last-tftp-core` 全绿 |
| P2 选项协商 | options.rs + OACK 流程 | 选项协商测试全绿 |
| P3 客户端 GET | session.rs + cli `get` 命令 | 自包含 server → client get 1MB 文件成功 |
| P4 客户端 PUT | cli `put` 命令 | 双向互通 |
| P5 服务器 | cli `server` 命令 + 目录权限 | 与 atftp/tftp-hpa 互通（若环境有装） |
| P6 windowsize | RFC 7440 滑动窗口 | window=8 吞吐对比 window=1 明显提升 |
| P7 IPv6 | dual-stack + A/AAAA 解析 | `[::1]` server 与 `[::1]` client 互通 |
| P8 进度条与统计 | indicatif + 吞吐计算 | CLI 与 GUI 都可见进度与 ETA |
| P9 GUI | eframe 应用 + tokio 后台 + 通道喂数据 | 启动后能看到 server start、client get 的实时进度 |
| P10 断点续传 | state 持久化 + resume | kill 中途后重新命令能继续 |
| P11 验证收口 | 大文件/边界/抓包样本 | 所有验收项签字 |

## 10. 风险与对策

| 风险 | 对策 |
|------|------|
| 69 端口需 root | 默认 6969，提供 `--port` 任意指定；root 下用 raw socket 兼容 |
| eframe 在 Wayland 下中文输入异常 | 阶段 P9 验收优先 X11/Wayland 启动；输入框用纯 ASCII 命令行示例 |
| 大文件内存爆炸 | 流式 IO + 固定 64KiB 写缓冲；4GB 极限场景测试时监控 RSS |
| 抓包工具不在容器内 | 把 tcpdump 调用做成可选；不可用时降级为 hexdump 文本 |
| 不同 OS 的 `set_only_v6` 行为差异 | 抽象 `DualStackUdp` 封装，win/mac/linux 各自走对应 API |

## 11. 非目标（明确不做）

- 不实现 TFTP 安全扩展（TSP 安全会话，明文口令），保持与现存服务兼容
- 不做组播 RFC 2090
- 不做 HTTPS / SFTP 替换品，TFTP 场景限定
- 不做固件签名验证，由上层应用负责

## 12. 仓库合规

- 中文注释、英文 git 提交信息（按 `AGENTS.md`）
- 不主动推送 Git；待你指令合并提交
- 每次只动必须动的文件，命名遵循本方案