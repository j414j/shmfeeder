use std::time::Duration;

use shmbroadcast::producer::{Producer, ProducerBuilder};
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
  let producer = ProducerBuilder::new("/test-queue", 64);
  if producer.is_err() {
    eprintln!("error during init: {:?}", producer.err());
    return;
  }

  let builder = producer.unwrap();
  let builder = builder.with_magic(0x7887_7887).with_version(1).build(4000);

  if builder.is_err() {
    eprintln!("error during build: {:?}", builder.err());
    return;
  }

  let mut producer: Producer<D, 64> = builder.unwrap();
  for i in 0..900 {
    let buffer = producer.get_next_buffer();
    unsafe { buffer.write(D::new(i)) };
    producer.commit_next_slot();
    println!("wrote: {i} at ptr {buffer:p}");
    producer.print_shm();
    std::thread::sleep(Duration::from_millis(100));
  }
}
