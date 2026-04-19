use std::{
  cell::UnsafeCell,
  sync::atomic::{AtomicUsize, Ordering},
};

#[repr(align(64))]
struct Slot<T> {
  seq: AtomicUsize,
  data: UnsafeCell<T>,
}

pub struct BroadCastQueue<T> {
  buf: *mut Slot<T>,
  last_committed_slot: AtomicUsize,
  len_mask: usize,
}

pub struct QueueLayout {
  pub size: usize,
  pub align: usize,
}

#[derive(Clone, Copy, Debug)]
pub enum QueueError {
  UnalignedPtr,
  LenNotPowerOfTwo,
}

impl<T> BroadCastQueue<T> {
  pub const fn layout() -> QueueLayout {
    QueueLayout {
      size: std::mem::size_of::<Slot<T>>(),
      align: std::mem::align_of::<Slot<T>>(),
    }
  }
  pub fn new(size: usize) -> Result<Self, QueueError> {
    let layout = Self::layout();
    let bytes_len = layout.size * size;
    let buf = unsafe {
      std::alloc::alloc(std::alloc::Layout::from_size_align(bytes_len, layout.align).unwrap())
    };
    unsafe { Self::from_raw_parts(buf, bytes_len) }
  }

  pub unsafe fn from_raw_parts(buf: *mut u8, len: usize) -> Result<Self, QueueError> {
    let align: usize = std::mem::align_of::<Slot<T>>();
    if buf as usize % align != 0 {
      return Err(QueueError::UnalignedPtr);
    }

    let n_slots = len / std::mem::size_of::<Slot<T>>();

    if n_slots >= 1 && n_slots.is_power_of_two() {
      let this = Self {
        buf: buf as *mut Slot<T>,
        len_mask: n_slots - 1,
        last_committed_slot: AtomicUsize::new(usize::MAX),
      };

      for i in 0..n_slots {
        unsafe {
          let ptr = this.buf.add(i);
          ptr.write(Slot {
            data: std::mem::zeroed(),
            seq: AtomicUsize::new(0),
          });
        };
      }

      Ok(this)
    } else {
      Err(QueueError::LenNotPowerOfTwo)
    }
  }
}

unsafe impl<T> Send for BroadCastQueue<T> {}
unsafe impl<T> Sync for BroadCastQueue<T> {}

pub struct WriteCursor<'a, T> {
  queue: &'a mut BroadCastQueue<T>,
  seq: usize,
}

impl<'a, T> WriteCursor<'a, T> {
  pub fn new(queue: &'a mut BroadCastQueue<T>) -> Self {
    Self { queue, seq: 0 }
  }

  pub fn get_next_buffer(&mut self) -> *mut T {
    let next_idx = (self
      .queue
      .last_committed_slot
      .load(Ordering::Relaxed)
      .wrapping_add(1))
      & (self.queue.len_mask);
    let next_slot = unsafe { &*self.queue.buf.add(next_idx) };

    // we are about to overwrite an element, call its destructor
    if self.seq - next_slot.seq.load(Ordering::Relaxed) == self.queue.len_mask + 1 {
      unsafe {
        std::ptr::drop_in_place(next_slot.data.get());
      }
    }
    next_slot.data.get()
  }

  pub fn commit_next_slot(&mut self) {
    let next_idx = (self
      .queue
      .last_committed_slot
      .load(Ordering::Relaxed)
      .wrapping_add(1))
      & (self.queue.len_mask);
    let next_slot = unsafe { &*self.queue.buf.add(next_idx) };

    self.seq = self.seq.wrapping_add(1);
    next_slot.seq.store(self.seq, Ordering::Release);
    self
      .queue
      .last_committed_slot
      .store(next_idx, Ordering::Release);
  }
}

pub struct ReadCursor<'a, T> {
  queue: &'a BroadCastQueue<T>,
  cursor: usize,
  seq: usize,
}

impl<'a, T> ReadCursor<'a, T> {
  pub fn new(queue: &'a BroadCastQueue<T>) -> Self {
    let last_committed_slot_idx = queue.last_committed_slot.load(Ordering::Acquire);
    Self {
      queue,
      seq: 0,
      cursor: if last_committed_slot_idx == usize::MAX {
        0
      } else {
        last_committed_slot_idx
      },
    }
  }

  /// unsafe because the slot can be overwritten by the producer at any point
  /// a slow consumer can face data races.
  pub unsafe fn try_read(&mut self) -> Option<&'a T> {
    let slot = unsafe { &*self.queue.buf.add(self.cursor) };
    let slot_seq = slot.seq.load(Ordering::Acquire);

    if slot_seq > self.seq {
      self.seq = slot_seq;
      self.cursor = (self.cursor + 1) & self.queue.len_mask;

      Some(unsafe { &*slot.data.get() })
    } else {
      None
    }
  }
}
