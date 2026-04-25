use shmfeeder::{
  consumer::{Consumer, ConsumerBuilder},
  error::ShmError,
};

#[derive(Debug, Copy, Clone)]
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
      b: i + 1,
      c: i + 2,
      d: i + 3,
    }
  }
}
pub fn now() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_micros() as u64
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
    .with_liveness_tolerance(10_000_000) // 10 seconds liveness tolerance
    .build(now());

  if builder.is_err() {
    eprintln!("error during build: {:?}", builder.err());
    return;
  }

  let mut consumer: Consumer<D, 64> = builder.unwrap();
  loop {
    match consumer.try_read(now()) {
      Ok(r) => {
        println!("read: {r:?} at ptr {r:p}");
      }
      Err(ShmError::NoActiveProducer) => {
        println!("consumer exiting due to no data");
        break;
      }
      Err(ShmError::NoData) => {}
      Err(err) => {
        println!("unexpected error: {err:?}");
      }
    }
  }
}
