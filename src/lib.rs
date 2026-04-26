//! A lock-free single-producer, multi-consumer broadcast queue backed by POSIX
//! shared memory.
//!
//! `shmfeeder` is intended for low-latency IPC between processes running on the
//! same host. One [`Producer`] owns a named shared-memory queue and publishes
//! fixed-layout `Copy` values into a power-of-two ring buffer. Any number of
//! [`Consumer`]s, up to the configured limit, can attach to the same queue and
//! read the newest committed items independently.
//!
//! The queue stores values directly in shared memory, so payload types should be
//! `#[repr(C)]`, contain no process-local pointers or references, and remain
//! layout-compatible across producer and consumer binaries. The producer and
//! consumers must use the same Rust type, magic number, and version.
//!
//! # Platform
//!
//! This crate uses POSIX shared-memory APIs (`shm_open`, `mmap`, `ftruncate`)
//! through `libc`, so it is primarily useful on Unix-like platforms with POSIX
//! shared memory support.
//!
//! # Heartbeats
//!
//! Heartbeats are enabled by default. Producers reject takeover while another
//! producer appears alive, consumers can detect a dead producer, and producers
//! can detect when no consumers are alive. Runtime heartbeat maintenance is
//! explicit: call the heartbeat update and check methods on the cadence required
//! by your application. Disable heartbeat support with the `no-heartbeats`
//! feature, or only disable consumer heartbeats with `no-consumer-heartbeat`.
//!
//! # Example
//!
//! Run the included examples in separate terminals:
//!
//! ```text
//! cargo run --example producer
//! cargo run --example consumer
//! ```
//!
//! A minimal producer setup looks like this:
//!
//! ```no_run
//! use std::time::{SystemTime, UNIX_EPOCH};
//! use shmfeeder::{Producer, ProducerBuilder};
//!
//! #[derive(Copy, Clone)]
//! #[repr(C)]
//! struct Tick {
//!   sequence: u64,
//!   price: f64,
//! }
//!
//! fn now_micros() -> u64 {
//!   SystemTime::now()
//!     .duration_since(UNIX_EPOCH)
//!     .unwrap()
//!     .as_micros() as u64
//! }
//!
//! let producer = ProducerBuilder::new("/ticks", 1024)?
//!   .with_magic(0x5449_434b)
//!   .with_version(1);
//!
//! #[cfg(not(feature = "no-consumer-heartbeat"))]
//! let producer = producer.with_max_consumers(8);
//!
//! #[cfg(not(feature = "no-heartbeats"))]
//! let mut producer: Producer<Tick> = producer.build(now_micros())?;
//!
//! #[cfg(feature = "no-heartbeats")]
//! let mut producer: Producer<Tick> = producer.build()?;
//!
//! producer.write(Tick {
//!   sequence: 1,
//!   price: 101.25,
//! });
//!
//! #[cfg(not(feature = "no-heartbeats"))]
//! producer.update_heartbeat(now_micros());
//!
//! #[cfg(not(feature = "no-consumer-heartbeat"))]
//! producer.check_any_consumer_alive(now_micros())?;
//! # Ok::<(), shmfeeder::ShmError>(())
//! ```
//!
//! A matching consumer can read copied values:
//!
//! ```no_run
//! use std::time::{SystemTime, UNIX_EPOCH};
//! use shmfeeder::{Consumer, ConsumerBuilder, ShmError};
//!
//! #[derive(Copy, Clone)]
//! #[repr(C)]
//! struct Tick {
//!   sequence: u64,
//!   price: f64,
//! }
//!
//! fn now_micros() -> u64 {
//!   SystemTime::now()
//!     .duration_since(UNIX_EPOCH)
//!     .unwrap()
//!     .as_micros() as u64
//! }
//!
//! let consumer = ConsumerBuilder::new("/ticks")?
//!   .with_magic(0x5449_434b)
//!   .with_version(1);
//!
//! #[cfg(not(feature = "no-heartbeats"))]
//! let mut consumer: Consumer<Tick> = consumer.build(now_micros())?;
//!
//! #[cfg(feature = "no-heartbeats")]
//! let mut consumer: Consumer<Tick> = consumer.build()?;
//!
//! #[cfg(not(feature = "no-heartbeats"))]
//! let now = now_micros();
//!
//! #[cfg(not(feature = "no-heartbeats"))]
//! consumer.check_producer_alive(now)?;
//!
//! #[cfg(not(feature = "no-consumer-heartbeat"))]
//! consumer.update_heartbeat(now);
//!
//! match consumer.try_read() {
//!   Ok(tick) => println!("{} {}", tick.sequence, tick.price),
//!   Err(ShmError::NoData) => {}
//!   Err(err) => return Err(err),
//! }
//! # Ok::<(), shmfeeder::ShmError>(())
//! ```

mod consumer;
mod error;
#[cfg(not(feature = "no-heartbeats"))]
mod heartbeats;
mod layout;
mod producer;
mod queue;

pub use consumer::{Consumer, ConsumerBuilder};
pub use error::{ShmError, ShmResult};
pub use layout::ShmState;
pub use producer::{Producer, ProducerBuilder};
