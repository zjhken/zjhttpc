# HTTP Client Review - Issues & Fix Plan

## P0 - Critical
- [ ] `ChunkedDecoderStream::poll_read` 中 `block_on` 死锁 (response.rs:202,225)
- [ ] `Content-Type` 头不发送 (client.rs send_header 函数)

## P1 - High
- [ ] 代理连接不查池 (client.rs:159-173)
- [ ] 池归还竞态: check-then-insert 应改用 entry API (client.rs:745-757)
- [ ] `BodyFixedLengthStream` 静默吞掉截断, 应返回 UnexpectedEof (response.rs:378-379)
- [ ] 代理 CONNECT 响应读取不完整, 单次 read 不保证读完整响应 (proxy.rs:197-212)

## P2 - Medium
- [ ] `read_until` 逐字节读, 应改为 buffered reader (client.rs:665-685)
- [ ] `read_until` 无大小限制, 恶意服务端可导致 OOM
- [ ] `Box::leak` 内存泄漏, content_type 应改为 String (requestx.rs:273-276)
- [ ] TLS 配置每次重建, 应缓存 Arc<ClientConfig> (client.rs:219-220)

## P3 - Low
- [ ] 池容量 off-by-one: `len <= 30` 应为 `len < 30` (client.rs:747)
- [ ] 池只增不减, 无 TTL/空闲淘汰机制
- [ ] 取连接时检测到关闭只移除一个, 应批量清理
- [ ] 多值请求头只发送第一个值 (client.rs:356-365)
- [ ] `is_stream_closed` peek+timeout 策略不可靠 → 已移除, 改为直接尝试使用

## Done
- [x] 第14点: `is_stream_closed` 改为 lazy validation + send 中自动重试
  - 移除 `is_stream_closed` (peek+timeout 不可靠且最多阻塞1秒)
  - `pick_or_connect_stream` 返回 `(BoxedStream, bool)` 标记是否来自池
  - 提取 `connect_fresh_tcp` / `connect_fresh_tls` / `connect_fresh_stream` 创建新连接
  - `send` 中：如果 `send_header` 失败且连接来自池，静默创建新连接重试一次
  - 重试安全：只在 send_header 阶段重试（body 尚未消费）
