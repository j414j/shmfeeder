use shmbroadcast::consumer::{Consumer, ConsumerBuilder};

#[derive(Debug, Copy, Clone)]
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
      b: i + 1,
      c: i + 2,
      d: i + 3,
    }
  }
}

#[test]
fn main() {
  let consumer = ConsumerBuilder::new("/test-queue");
  if consumer.is_err() {
    eprintln!("error during init: {:?}", consumer.err());
    return;
  }

  let builder = consumer.unwrap();
  let builder = builder
    .with_magic(0x7887_7887)
    .with_version(1)
    .with_liveness_tolerance(900000)
    .build(19000);

  if builder.is_err() {
    eprintln!("error during build: {:?}", builder.err());
    return;
  }

  let mut consumer: Consumer<D, 64> = builder.unwrap();
  let mut last_read_ms = std::time::Instant::now();
  loop {
    if let Some(r) = consumer.try_read() {
      println!("read: {r:?} at ptr {r:p}");
      last_read_ms = std::time::Instant::now();
    }
    if last_read_ms.elapsed().as_millis() > 10000 {
      println!("consumer exiting due to no data");
      break;
    }
  }
}
