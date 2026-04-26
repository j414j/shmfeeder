#[cfg(not(feature = "no-heartbeats"))]
use std::time::{SystemTime, UNIX_EPOCH};
use std::{thread, time::Duration};

use shmfeeder::{Producer, ProducerBuilder};

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
  let builder = ProducerBuilder::new(QUEUE_NAME, 1024)?
    .with_magic(MAGIC)
    .with_version(VERSION);

  #[cfg(not(feature = "no-consumer-heartbeat"))]
  let builder = builder.with_max_consumers(8);

  #[cfg(not(feature = "no-heartbeats"))]
  let builder = builder.with_liveness_tolerance(2_000_000);

  #[cfg(not(feature = "no-heartbeats"))]
  let mut producer: Producer<Quote> = builder.build(now_micros())?;

  #[cfg(feature = "no-heartbeats")]
  let mut producer: Producer<Quote> = builder.build()?;

  for sequence in 0.. {
    let quote = Quote {
      sequence,
      bid: 100.0 + sequence as f64 * 0.01,
      ask: 100.05 + sequence as f64 * 0.01,
    };

    producer.write(quote);

    #[cfg(not(feature = "no-heartbeats"))]
    producer.update_heartbeat(now_micros());

    #[cfg(not(feature = "no-consumer-heartbeat"))]
    if producer.check_any_consumer_alive(now_micros()).is_err() {
      eprintln!("published {quote:?}; no active consumers are currently attached");
    }

    println!("published {quote:?}");
    thread::sleep(Duration::from_millis(250));
  }

  Ok(())
}
