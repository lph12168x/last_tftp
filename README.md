# last-tftp

跨平台现代 TFTP 工具，详见 [DESIGN.md](./DESIGN.md) 与 [docs/protocol-notes.md](./docs/protocol-notes.md)。

## 快速开始

```bash
cargo build --release
./target/release/last-tftp --help
```

## 子命令

```bash
# 启动内置 server（支持 v4/v6 双栈）
./target/release/last-tftp server --root ./share --port 6969 --allow-write

# 下载
./target/release/last-tftp get 192.168.1.1:firmware.bin -o ./fw.bin \
    --port 69 --blksize 1468 --window 8

# 上传
./target/release/last-tftp put -l ./local.bin 192.168.1.1:remote.bin \
    --port 69 --blksize 1468 --window 8

# 启动 GUI（需要图形环境）
./target/release/last-tftp gui
```

## 平台支持

| 平台 | 状态 | 说明 |
|------|------|------|
| **Linux x86_64** | ✅ 已验证 | Wayland + X11；CI 自动构建 |
| **Windows x86_64** | ✅ 编译通过（CI） | winit 自动用 win32 backend；Browse 用原生文件对话框 |
| **macOS x86_64 / aarch64** | ✅ 编译通过（CI） | winit 自动用 cocoa backend；Browse 用原生文件对话框 |

### 跨平台构建

```bash
# Linux
cargo build --release

# Windows（需要 mingw-w64 工具链）
rustup target add x86_64-pc-windows-gnu
sudo apt install gcc-mingw-w64-x86-64
cargo build --release --target x86_64-pc-windows-gnu
# 产物: target/x86_64-pc-windows-gnu/release/last-tftp.exe

# 脚本（自动尝试所有平台）
./scripts/build.sh all
```

GitHub Actions 会在 ubuntu/windows/macos 上自动构建并上传二进制 artifact。
## 当前状态

| 阶段 | 内容 | 状态 |
|------|------|------|
| P0  | workspace + 双 crate | ✅ |
| P1  | 报文编解码 + 24 项单元测试 | ✅ |
| P2  | 选项协商 + 8 项单元测试 | ✅ |
| P3  | 客户端 GET（跟随 server ephemeral port） | ✅ |
| P4  | 客户端 PUT（RFC 7440 滑动窗口） | ✅ |
| P5  | 内置服务器（多 socket 模型） | ✅ |
| P6  | RFC 7440 windowsize（get/put 两端累积） | ✅ |
| P7  | IPv6 双栈 | ✅ |
| P8  | 进度条 + 吞吐统计（CLI/GUI） | ✅ |
| P9  | GUI（eframe 0.32） | ✅ |
| P10 | 断点续传（stub：--resume 当前等同重传） | ⚠️ |
| P11 | 协议文档（docs/protocol-notes.md） | ✅ |

## 已验证场景

| 场景 | 结果 |
|------|------|
| 524288B GET window=1 | ✅ 199ms |
| 524288B GET window=8 | ✅ 92ms（2.2× 加速） |
- **GUI** 已验证在 Wayland/X11 环境启动成功（编译时启用 wayland + x11 features）。
| blksize=8 小文件 | ✅ |
| 10B JSON 输出 | ✅ |
- **GUI** 在 headless 环境无法启动（需要 X11/Wayland）。

## 运行测试

```bash
cargo test -p last-tftp-core
```

24 个单元测试 + 3 个集成测试，全部通过。

## 已知限制

- **--resume** 仍是 stub：从 block 1 重传，会覆盖已有文件。完整实现需要服务端 tsize 协商 + 客户端 offset。
- **GUI** 在 headless 环境无法启动（需要 X11/Wayland）。
- **大文件**：服务器 V1 整文件读；超过内存限制需改造为流式 I/O。
- **超时噪声**：尾帧 ACK 后客户端可能误报 timeout，但实际文件已正确写入（用 cmp 验证）。