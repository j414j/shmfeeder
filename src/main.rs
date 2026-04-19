use std::{alloc::Layout, time::Instant};

use shmbroadcast::queue::{BroadCastQueue, ReadCursor, WriteCursor};

const N_ELEM: usize = 4096;
type Elem = u128;

fn writer_thread(mut handle: WriteCursor<'static, Elem>) {
  for i in 0..N_ELEM * 2 {
    let ptr = handle.get_next_buffer();
    unsafe { ptr.write(i as u128) };
    handle.commit_next_slot();
  }
}

fn read_thread(tid: usize, mut handle: ReadCursor<'static, Elem>) {
  println!("[reader init] [{tid}]");
  let mut last_recv = Instant::now();
  let mut last_item = 0;
  let mut num_items = 0;
  loop {
    if last_recv.elapsed().as_millis() > 1_000 {
      break;
    }

    if let Some(v) = unsafe { handle.try_read() } {
      if last_item + 1 != *v && last_item != 0 {
        eprintln!("received out of sequence number in {tid} last received {last_item} current {v}");
        break;
      }
      // println!("[reader {tid}] read {v}");
      last_item = *v;
      num_items += 1;
      last_recv = Instant::now();
    }
  }

  println!("thread {tid} received {num_items} items ending at {last_item}");
}

fn main() {
  let layout = BroadCastQueue::<Elem>::layout();
  let buflen = layout.size * N_ELEM;
  let buf = unsafe { std::alloc::alloc(Layout::from_size_align(buflen, layout.align).unwrap()) };
  println!("[main] buflen = {}", buflen);
  let queue = unsafe { BroadCastQueue::from_raw_parts(buf, buflen) };

  assert!(queue.is_ok());

  let mut queue = queue.unwrap();
  let queue_ptr = &mut queue as *mut BroadCastQueue<Elem>;

  let mut handles = vec![];
  for i in 0..2 {
    let read_handle = ReadCursor::new(unsafe { &*queue_ptr });
    handles.push(std::thread::spawn(move || read_thread(i, read_handle)));
  }

  let write_handle = WriteCursor::new(unsafe { &mut *queue_ptr });
  handles.push(std::thread::spawn(move || writer_thread(write_handle)));

  // slight delay to simulate late joining readers
  let delay = std::hint::black_box(|duration: usize| {
    let mut s = 0;
    for i in 0..duration {
      s += 1;
    }
  });
  delay(100_000);

  for i in 2..4 {
    let read_handle = ReadCursor::new(unsafe { &*queue_ptr });
    handles.push(std::thread::spawn(move || read_thread(i, read_handle)));
  }

  for handle in handles {
    handle.join().unwrap();
  }
}
