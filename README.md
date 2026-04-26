# shmfeeder

`shmfeeder` is a lock-free, single-producer, multi-consumer broadcast ring
buffer backed by POSIX shared memory.

It is designed for low-latency IPC between processes on the same machine. One
producer creates and owns a named shared-memory queue, writes fixed-layout
`Copy` values into a power-of-two ring buffer, and one or more consumers attach
to read the stream independently.

## Features

- Single producer with multiple independent consumers.
- Data lives directly in shared memory; reads can either copy values out or
  borrow them zero-copy.
- Producer and consumer liveness checks through heartbeats by default.
- Compatibility guards through application-defined magic and version fields.
- POSIX shared-memory implementation using `shm_open`, `mmap`, and
  `ftruncate`.

## Platform Support

`shmfeeder` uses Unix/POSIX APIs through `libc`. It is intended for Unix-like
systems that provide POSIX shared memory.

## Install

```toml
[dependencies]
shmfeeder = "0.1"
```

## Payload Types

Payloads are copied directly into shared memory. Use plain fixed-layout data:

- Derive or implement `Copy`.
- Prefer `#[repr(C)]` for producer/consumer ABI stability.
- Do not store process-local pointers, references, heap-owning types, file
  descriptors, or other values that are only meaningful inside one process.
- Build producer and consumer binaries with matching payload definitions,
  magic numbers, and versions.

## Queue Names and Sizing

Queue names are passed to `shm_open`; on typical Unix systems they should start
with `/`, for example `"/prices"`.

The producer configures the ring length. It must be a non-zero power of two:

```rust
let producer = shmfeeder::ProducerBuilder::new("/prices", 1024)?;
# Ok::<(), shmfeeder::ShmError>(())
```

Slow consumers can miss old items when the producer wraps the ring and
overwrites slots.

## Heartbeats

Heartbeats are enabled by default.

- Producers refuse to take over a queue while another producer appears alive.
- Consumers can detect a dead producer.
- Producers can detect when no consumers are alive.

Timestamp arguments are plain `u64` values. The crate does not choose a clock;
all producers and consumers should use the same units. The examples use
microseconds since the Unix epoch.

Feature flags:

- `no-consumer-heartbeat`: keep producer heartbeats but disable consumer
  tracking.
- `no-heartbeats`: disable all heartbeat support.

When `no-heartbeats` is enabled, `build`, `commit_next_slot`, and read methods
do not take timestamp arguments.

## Producer Example

Run:

```sh
cargo run --example producer
```

Minimal producer:

```rust
use std::time::{SystemTime, UNIX_EPOCH};
use shmfeeder::{Producer, ProducerBuilder};

#[derive(Debug, Copy, Clone)]
#[repr(C)]
struct Tick {
  sequence: u64,
  bid: f64,
  ask: f64,
}

fn now_micros() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_micros() as u64
}

let mut producer: Producer<Tick> = ProducerBuilder::new("/shmfeeder-ticks", 1024)?
  .with_magic(0x5449_434b)
  .with_version(1)
  .with_max_consumers(8)
  .with_liveness_tolerance(2_000_000)
  .build(now_micros())?;

let slot = producer.get_next_buffer();
unsafe {
  slot.write(Tick {
    sequence: 1,
    bid: 101.25,
    ask: 101.30,
  });
}
producer.commit_next_slot(now_micros())?;
# Ok::<(), shmfeeder::ShmError>(())
```

## Consumer Example

Run in another terminal after starting the producer:

```sh
cargo run --example consumer
```

Minimal consumer:

```rust
use std::time::{SystemTime, UNIX_EPOCH};
use shmfeeder::{Consumer, ConsumerBuilder, ShmError};

#[derive(Debug, Copy, Clone)]
#[repr(C)]
struct Tick {
  sequence: u64,
  bid: f64,
  ask: f64,
}

fn now_micros() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_micros() as u64
}

let mut consumer: Consumer<Tick> = ConsumerBuilder::new("/shmfeeder-ticks")?
  .with_magic(0x5449_434b)
  .with_version(1)
  .with_liveness_tolerance(2_000_000)
  .build(now_micros())?;

match consumer.try_read(now_micros()) {
  Ok(tick) => println!("{tick:?}"),
  Err(ShmError::NoData) => {}
  Err(err) => return Err(err),
}
# Ok::<(), shmfeeder::ShmError>(())
```

For the lowest copy overhead, use `unsafe Consumer::try_read_zero_copy`. The
returned reference points directly into the shared-memory ring and may be
overwritten by the producer at any time, so it should only be used for very
short operations.

## Error Handling

Most APIs return `ShmResult<T>`, an alias for `Result<T, ShmError>`. Common
recoverable states include:

- `ShmError::NoData`: no unread item is currently available.
- `ShmError::NoActiveProducer`: the producer heartbeat is stale.
- `ShmError::NoActiveConsumer`: the producer did not find any live consumers.
- `ShmError::QueueAlreadyAcquired(pid)`: another producer appears to own the
  queue.

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT license

at your option.
