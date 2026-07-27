//! Happy Eyeballs (RFC 8305) connection racing.
//!
//! Given a list of candidate `SocketAddr`s (typically the result of a DNS
//! resolution that returned both IPv4 and IPv6 records), `select_happy_eyeballs`
//! starts connection attempts in order with a small delay between them, and
//! returns the first success. If one attempt fails, the next is started
//! immediately (no extra delay). If all attempts fail, the last error is
//! propagated.
//!
//! This avoids the long stalls that happen when a hostname resolves to an
//! unreachable address (e.g. `localhost` resolving to `::1` on a host where
//! only `127.0.0.1` is listened on) while still preferring IPv6 per RFC 8305.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use futures::future::{BoxFuture, FutureExt};
use futures::stream::{FuturesUnordered, StreamExt};

/// Configuration for [`select_happy_eyeballs`].
///
/// Defaults follow RFC 8305: IPv6 tried first, 250 ms between consecutive
/// connection attempts.
#[derive(Debug, Clone, Copy)]
pub struct HappyEyeballsConfig {
	/// Try IPv6 candidates before IPv4 (RFC 8305 §2).
	pub ipv6_first: bool,
	/// Delay between starting consecutive attempts (RFC 8305 §4.1 recommends
	/// 250 ms; trade-off: smaller = faster failure recovery, larger = less
	/// network burst).
	pub connection_attempt_delay: Duration,
}

impl Default for HappyEyeballsConfig {
	fn default() -> Self {
		Self {
			ipv6_first: true,
			connection_attempt_delay: Duration::from_millis(250),
		}
	}
}

/// Run Happy Eyeballs over `addrs`.
///
/// `make_future` is invoked once per candidate, lazily, as attempts are
/// started. The returned future may borrow from the calling scope via
/// lifetime `'a`; the boxed future is consumed by the race before
/// `select_happy_eyeballs` returns.
///
/// Returns the successful value together with the winning `SocketAddr`. If
/// every candidate fails, the last seen error is returned.
///
/// # Panics
/// Panics if `addrs` is empty — callers must ensure at least one candidate.
pub async fn select_happy_eyeballs<'a, T, E, F>(
	addrs: Vec<SocketAddr>, cfg: HappyEyeballsConfig, make_future: F,
) -> Result<(T, SocketAddr), E>
where
	T: 'a,
	E: 'a,
	F: Fn(SocketAddr) -> BoxFuture<'a, Result<T, E>>,
{
	assert!(
		!addrs.is_empty(),
		"select_happy_eyeballs requires at least one candidate address",
	);

	let mut sorted = addrs;
	sort_in_place(&mut sorted, cfg.ipv6_first);

	let first_addr = sorted[0];
	let mut remaining: VecDeque<SocketAddr> = sorted[1..].iter().copied().collect();
	let mut next_attempt_at = Instant::now() + cfg.connection_attempt_delay;

	let mut active: FuturesUnordered<BoxFuture<'a, (Result<T, E>, SocketAddr)>> =
		FuturesUnordered::new();
	active.push(
		make_future(first_addr)
			.map(move |r| (r, first_addr))
			.boxed(),
	);

	let mut last_err: Option<E> = None;

	loop {
		if !remaining.is_empty() {
			let delay = next_attempt_at.saturating_duration_since(Instant::now());
			match async_std::future::timeout(delay, active.next()).await {
				Ok(Some((Ok(value), addr))) => return Ok((value, addr)),
				Ok(Some((Err(e), _))) => {
					// Failure on one candidate — keep waiting for the others
					// (or the next attempt timer to start a new one).
					last_err = Some(e);
				}
				Ok(None) => {
					// active drained while candidates remain — start the next
					// one immediately rather than waiting for the timer
					// (RFC 8305 §5: a failed attempt tries the next address
					// without delay).
					push_next_addr(&mut active, &mut remaining, &make_future);
					next_attempt_at = Instant::now() + cfg.connection_attempt_delay;
				}
				Err(_) => {
					// Timer fired — start the next candidate.
					push_next_addr(&mut active, &mut remaining, &make_future);
					next_attempt_at = Instant::now() + cfg.connection_attempt_delay;
				}
			}
		} else {
			// No more candidates to schedule — drain active.
			match active.next().await {
				Some((Ok(value), addr)) => return Ok((value, addr)),
				Some((Err(e), _)) => {
					last_err = Some(e);
					if active.is_empty() {
						return Err(last_err.expect("at least one future was started"));
					}
				}
				None => return Err(last_err.expect("at least one future was started")),
			}
		}
	}
}

fn push_next_addr<'a, T, E, F>(
	active: &mut FuturesUnordered<BoxFuture<'a, (Result<T, E>, SocketAddr)>>,
	remaining: &mut VecDeque<SocketAddr>, make_future: &F,
) where
	T: 'a,
	E: 'a,
	F: Fn(SocketAddr) -> BoxFuture<'a, Result<T, E>>,
{
	if let Some(addr) = remaining.pop_front() {
		active.push(make_future(addr).map(move |r| (r, addr)).boxed());
	}
}

/// Reorder `addrs` so the preferred family leads, with the two families
/// interleaved (RFC 8305 §4: don't exhaust one family before trying the
/// other).
fn sort_in_place(addrs: &mut Vec<SocketAddr>, ipv6_first: bool) {
	let taken = std::mem::take(addrs);
	let (mut primary, mut secondary): (Vec<_>, Vec<_>) =
		taken.into_iter().partition(|a| a.is_ipv6() == ipv6_first);
	let mut out = Vec::with_capacity(primary.len() + secondary.len());
	while !primary.is_empty() || !secondary.is_empty() {
		if let Some(a) = primary.pop() {
			out.push(a);
		}
		if let Some(a) = secondary.pop() {
			out.push(a);
		}
	}
	*addrs = out;
}

#[cfg(test)]
mod tests {
	use super::*;
	use async_std::sync::Mutex;
	use std::sync::Arc;

	#[test]
	fn sort_ipv6_first() {
		let mut addrs: Vec<SocketAddr> = vec![
			"127.0.0.1:80".parse().unwrap(),
			"[::1]:80".parse().unwrap(),
			"127.0.0.2:80".parse().unwrap(),
		];
		sort_in_place(&mut addrs, true);
		assert!(addrs[0].is_ipv6(), "first should be IPv6, got {:?}", addrs);
		assert!(addrs[1].is_ipv4(), "second should be IPv4, got {:?}", addrs);
		assert!(addrs[2].is_ipv4(), "third should be IPv4, got {:?}", addrs);
	}

	#[test]
	fn sort_ipv4_first() {
		let mut addrs: Vec<SocketAddr> = vec![
			"[::1]:80".parse().unwrap(),
			"127.0.0.1:80".parse().unwrap(),
			"[::2]:80".parse().unwrap(),
		];
		sort_in_place(&mut addrs, false);
		assert!(addrs[0].is_ipv4(), "first should be IPv4, got {:?}", addrs);
		assert!(addrs[1].is_ipv6(), "second should be IPv6, got {:?}", addrs);
		assert!(addrs[2].is_ipv6(), "third should be IPv6, got {:?}", addrs);
	}

	#[async_std::test]
	async fn returns_first_success() {
		// Two candidates; the first sleeps longer than the attempt delay,
		// so the second wins.
		let addrs: Vec<SocketAddr> = vec![
			"203.0.113.1:80".parse().unwrap(),
			"127.0.0.1:80".parse().unwrap(),
		];
		let cfg = HappyEyeballsConfig::default();
		let make = |addr: SocketAddr| {
			Box::pin(async move {
				if addr.ip().to_string() == "127.0.0.1" {
					Ok::<SocketAddr, std::io::Error>(addr)
				} else {
					async_std::task::sleep(Duration::from_secs(5)).await;
					Ok(addr)
				}
			}) as BoxFuture<'static, Result<SocketAddr, std::io::Error>>
		};

		let (won, _) = select_happy_eyeballs(addrs, cfg, make).await.unwrap();
		assert_eq!(won.ip().to_string(), "127.0.0.1");
	}

	#[async_std::test]
	async fn returns_last_error_when_all_fail() {
		let addrs: Vec<SocketAddr> = vec![
			"127.0.0.1:80".parse().unwrap(),
			"127.0.0.2:80".parse().unwrap(),
		];
		let cfg = HappyEyeballsConfig {
			ipv6_first: true,
			connection_attempt_delay: Duration::from_millis(1),
		};
		let counter = Arc::new(Mutex::new(0u32));
		let make = {
			let counter = counter.clone();
			move |_addr: SocketAddr| {
				let counter = counter.clone();
				Box::pin(async move {
					let mut g = counter.lock().await;
					*g += 1;
					Err(std::io::Error::new(
						std::io::ErrorKind::ConnectionRefused,
						"refused",
					))
				}) as BoxFuture<'static, Result<(), std::io::Error>>
			}
		};
		let result: Result<((), SocketAddr), std::io::Error> =
			select_happy_eyeballs(addrs, cfg, make).await;
		assert!(result.is_err());
		assert_eq!(
			result.unwrap_err().kind(),
			std::io::ErrorKind::ConnectionRefused
		);
		assert_eq!(*counter.lock().await, 2, "both candidates should have run");
	}

	#[async_std::test]
	#[should_panic(expected = "at least one candidate")]
	async fn empty_addrs_panics() {
		let cfg = HappyEyeballsConfig::default();
		let _: Result<((), SocketAddr), std::io::Error> =
			select_happy_eyeballs(vec![], cfg, |_| Box::pin(async { Ok(()) })).await;
	}
}
