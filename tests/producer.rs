use std::time::{Duration, UNIX_EPOCH};

#[cfg(not(feature = "no-heartbeats"))]
use shmfeeder::ShmError;
use shmfeeder::{Producer, ProducerBuilder};

#[derive(Copy, Clone)]
#[repr(C)]
pub struct D {
  a: i64,
  b: i64,
  c: i64,
  d: i64,
}

impl D {
  pub fn new(i: i64) -> Self {
    Self {
      a: i,
      b: i << 1,
      c: i << 2,
      d: i << 3,
    }
  }
}
pub fn now() -> u64 {
  std::time::SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_micros() as u64
}

#[test]
fn main() {
  let producer = ProducerBuilder::new("/test-queue", 4);
  if producer.is_err() {
    eprintln!("error during init: {:?}", producer.err());
    return;
  }

  let builder = producer.unwrap();
  let builder = builder.with_magic(0x7887_7887).with_version(1);
  #[cfg(not(feature = "no-heartbeats"))]
  let builder = {
    let builder = builder.with_liveness_tolerance(10_000_000); // 10 second liveness check

    #[cfg(not(feature = "no-consumer-heartbeat"))]
    let builder = builder.with_max_consumers(2);

    builder.build(now())
  };
  #[cfg(feature = "no-heartbeats")]
  let builder = builder.build();

  if builder.is_err() {
    eprintln!("error during build: {:?}", builder.err());
    return;
  }

  let mut producer: Producer<D> = builder.unwrap();
  for i in 0..9000 {
    let buffer = producer.get_next_buffer();
    unsafe { buffer.write(D::new(i)) };
    #[cfg(not(feature = "no-heartbeats"))]
    if let Err(ShmError::NoActiveConsumer) = producer.commit_next_slot(now()) {
      println!("no active consumer, exiting now");
      break;
    }
    #[cfg(feature = "no-heartbeats")]
    let _ = producer.commit_next_slot();
    println!("wrote: {i} at ptr {buffer:p}");
    producer.debug_print_queue();
    std::thread::sleep(Duration::from_millis(100));
  }
}
