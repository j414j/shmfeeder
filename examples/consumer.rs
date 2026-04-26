#[cfg(not(feature = "no-heartbeats"))]
use std::time::{SystemTime, UNIX_EPOCH};
use std::{thread, time::Duration};

use shmfeeder::{Consumer, ConsumerBuilder, ShmError};

const QUEUE_NAME: &str = "/shmfeeder-example";
const MAGIC: u64 = 0x5348_4d46;
const VERSION: u64 = 1;

#[derive(Debug, Copy, Clone)]
#[repr(C)]
struct Quote {
  sequence: u64,
  bid: f64,
  ask: f64,
}

#[cfg(not(feature = "no-heartbeats"))]
fn now_micros() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_micros() as u64
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let builder = ConsumerBuilder::new(QUEUE_NAME)?
    .with_magic(MAGIC)
    .with_version(VERSION);

  #[cfg(not(feature = "no-heartbeats"))]
  let builder = builder.with_liveness_tolerance(2_000_000);

  #[cfg(not(feature = "no-heartbeats"))]
  let mut consumer: Consumer<Quote> = builder.build(now_micros())?;

  #[cfg(feature = "no-heartbeats")]
  let mut consumer: Consumer<Quote> = builder.build()?;

  loop {
    #[cfg(not(feature = "no-heartbeats"))]
    let result = consumer.try_read(now_micros());

    #[cfg(feature = "no-heartbeats")]
    let result = consumer.try_read();

    match result {
      Ok(quote) => println!("received {quote:?}"),
      Err(ShmError::NoData) => thread::sleep(Duration::from_millis(10)),
      Err(ShmError::NoActiveProducer) => {
        eprintln!("producer heartbeat is stale");
        break;
      }
      Err(err) => return Err(err.into()),
    }
  }

  Ok(())
}
