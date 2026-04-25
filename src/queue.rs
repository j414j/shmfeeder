use std::{
  cell::UnsafeCell,
  sync::atomic::{AtomicUsize, Ordering},
};

use crate::error::{ShmError, ShmResult};

#[repr(C, align(64))]
pub(crate) struct Slot<T>
where
  T: Copy,
{
  pub(crate) seq: AtomicUsize,
  data: UnsafeCell<T>,
}

pub struct BroadCastQueue<T>
where
  T: Copy,
{
  buf: *mut Slot<T>,
  last_committed_slot: *mut AtomicUsize,
  len_mask: usize,
}

pub struct SlotLayout {
  pub size: usize,
  pub align: usize,
}

impl<T> BroadCastQueue<T>
where
  T: Copy,
{
  pub const fn slot_layout() -> SlotLayout {
    SlotLayout {
      size: std::mem::size_of::<Slot<T>>(),
      align: std::mem::align_of::<Slot<T>>(),
    }
  }

  pub unsafe fn from_raw_parts(
    buf: *mut u8,
    len: usize,
    last_committed_slot: *mut AtomicUsize,
  ) -> ShmResult<Self> {
    let align: usize = std::mem::align_of::<Slot<T>>();
    if buf as usize % align != 0 {
      return Err(ShmError::UnalignedPtr);
    }

    let n_slots = len / std::mem::size_of::<Slot<T>>();

    if n_slots >= 1 && n_slots.is_power_of_two() {
      let this = Self {
        buf: buf as *mut Slot<T>,
        len_mask: n_slots - 1,
        last_committed_slot,
      };

      Ok(this)
    } else {
      Err(ShmError::LengthNotPowerOfTwo)
    }
  }
}

unsafe impl<T> Send for BroadCastQueue<T> where T: Copy {}
unsafe impl<T> Sync for BroadCastQueue<T> where T: Copy {}

pub struct BroadcastWriteHandle<T>
where
  T: Copy,
{
  queue: BroadCastQueue<T>,
  seq: usize,
}

impl<T> BroadcastWriteHandle<T>
where
  T: Copy,
{
  pub fn new(queue: BroadCastQueue<T>) -> Self {
    Self { queue, seq: 0 }
  }

  pub fn get_next_buffer(&mut self) -> *mut T {
    let next_idx = ((unsafe { &*self.queue.last_committed_slot })
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
    let next_idx = ((unsafe { &*self.queue.last_committed_slot })
      .load(Ordering::Relaxed)
      .wrapping_add(1))
      & (self.queue.len_mask);
    let next_slot = unsafe { &*self.queue.buf.add(next_idx) };

    self.seq = self.seq.wrapping_add(1);
    next_slot.seq.store(self.seq, Ordering::Release);
    (unsafe { &*self.queue.last_committed_slot }).store(next_idx, Ordering::Release);
  }
}

pub struct BroadcastReadHandle<T>
where
  T: Copy,
{
  queue: BroadCastQueue<T>,
  cursor: usize,
  seq: usize,
}

impl<T> BroadcastReadHandle<T>
where
  T: Copy,
{
  pub fn new(queue: BroadCastQueue<T>) -> Self {
    let last_committed_slot_idx = unsafe { &*queue.last_committed_slot }.load(Ordering::Acquire);
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
  pub unsafe fn try_read(&mut self) -> Option<&T> {
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
