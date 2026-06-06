# HTTP Client Review - Issues & Fix Plan

## P0 - Critical

- [X] `ChunkedDecoderStream::poll_read` 中 `block_on` 死锁 (response.rs:202,225)
  - 已改为正确的 async poll 实现，无 block_on 调用
- [X] `Content-Type` 头不发送 (client.rs send_header 函数)
  - form/multipart 自动设置 Content-Type；raw body (body_string/body_slice) 需手动设置
  - 残留: 对 raw body 无默认 Content-Type（如 text/plain 或 application/octet-stream）

## P1 - High

- [X] 代理连接不查池 (client.rs:159-173)
  - 代理连接现在也通过 try_pick_from_pool 查池复用
- [X] 池归还竞态: check-then-insert 应改用 entry API (client.rs:745-757)
  - 已改用 DashMap entry API，消除竞态
- [X] `BodyFixedLengthStream` 静默吞掉截断, 应返回 UnexpectedEof (response.rs:378-379)
  - 已返回 UnexpectedEof 错误
- [X] 代理 CONNECT 响应读取不完整, 单次 read 不保证读完整响应 (proxy.rs:197-212)
  - 已改为 loop 读取直到收到完整 \r\n\r\n 结尾的响应

## P2 - Medium

- [X] `read_until` 逐字节读, 应改为 buffered reader (client.rs:665-685)
  - 已改为 4096 字节缓冲读取 (commit fd482da)
- [X] `read_until` 无大小限制, 恶意服务端可导致 OOM
  - 已添加 max_bytes 限制 (commit fd482da)
- [X] `Box::leak` 内存泄漏, content_type 应改为 String (requestx.rs:273-276)
  - 已改为 Cow::Owned，不再内存泄漏 (commit 38bbadd)
- [X] TLS 配置每次重建, 应缓存 Arc`<ClientConfig>` (client.rs:219-220)
  - 已用 OnceLock 缓存 (commit c9916da)

## P3 - Low

- [X] 池容量 off-by-one: `len <= 30` 应为 `len < 30` (client.rs:747)
  - 已修复为 `len < max_per_key`
- [X] 池只增不减, 无 TTL/空闲淘汰机制
  - 已添加 idle_timeout + 自动淘汰
- [X] 取连接时检测到关闭只移除一个, 应批量清理
  - 连接池重写后通过 pop 循环 + idle 超时自然淘汰
- [ ] 多值请求头只发送第一个值 (client.rs:592)
  - 仍未修复，代码仍用 `values.first().unwrap()`
- [X] `is_stream_closed` peek+timeout 策略不可靠 → 已移除, 改为 lazy validation + send 中自动重试

## Done

- [X] 第14点: `is_stream_closed` 改为 lazy validation + send 中自动重试
  - 移除 `is_stream_closed` (peek+timeout 不可靠且最多阻塞1秒)
  - `pick_or_connect_stream` 返回 `(BoxedStream, bool)` 标记是否来自池
  - 提取 `connect_fresh_tcp` / `connect_fresh_tls` / `connect_fresh_stream` 创建新连接
  - `send` 中：如果 `send_header` 失败且连接来自池，静默创建新连接重试一次
  - 重试安全：只在 send_header 阶段重试（body 尚未消费）

## Still Open (2026-06-06 复查)

1. **多值请求头只发第一个** — `client.rs:592` 仍用 `values.first().unwrap()`，TODO 注释还在
2. **raw body 无默认 Content-Type** — `set_body_string()` / `set_body_slice()` 不自动设置 Content-Type
