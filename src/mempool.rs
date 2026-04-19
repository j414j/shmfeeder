use core::{
  cell::UnsafeCell,
  mem::MaybeUninit,
  sync::atomic::{AtomicUsize, Ordering},
};

const INDEX_BITS: usize = usize::BITS as usize / 2;
const INDEX_MASK: usize = (1usize << INDEX_BITS) - 1;

const fn pack(tag: usize, index: usize) -> usize {
  tag << INDEX_BITS | index
}

const fn unpack(packed: usize) -> (usize, usize) {
  (packed >> INDEX_BITS, packed & INDEX_MASK)
}

pub enum TryAllocFailReason {
  BufferFull,
  CASFailed,
}

struct Slot<T> {
  ref_cnt: AtomicUsize,
  elem: UnsafeCell<MaybeUninit<T>>,
  next: AtomicUsize,
}

pub struct MemPool<T> {
  buf: *mut Slot<T>,
  cap: usize,
  head: AtomicUsize,
}

impl<T> MemPool<T> {
  /// Creates a pool with space for at most `size` concurrently allocated slots.
  pub fn new(size: usize) -> Self {
    assert!(size < INDEX_MASK);
    let mut buf = Vec::with_capacity(size);
    for i in 0..size {
      buf.push(Slot {
        ref_cnt: AtomicUsize::new(0),
        elem: UnsafeCell::new(MaybeUninit::uninit()),
        next: AtomicUsize::new(i + 1),
      })
    }
    let ptr = buf.into_boxed_slice().as_mut_ptr();

    Self {
      buf: ptr,
      cap: size,
      head: AtomicUsize::new(pack(0, 0)),
    }
  }

  pub unsafe fn from_raw_buffer(buf: *mut u8, len: usize) -> Option<Self> {
    let num_slots = std::mem::size_of::<Slot<T>>() / len;
    if num_slots < 1 {
      None
    } else {
      let buf = buf as *mut Slot<T>;
      for i in 0..num_slots {
        unsafe {
          buf.add(i).write(Slot {
            elem: UnsafeCell::new(MaybeUninit::uninit()),
            ref_cnt: AtomicUsize::new(0),
            next: AtomicUsize::new(i + 1),
          })
        };
      }

      Some(Self {
        buf,
        cap: num_slots,
        head: AtomicUsize::new(0),
      })
    }
  }

  /// Allocates one slot from the pool.
  ///
  /// The returned handle starts out uninitialized. Call [`SlotHandle::init`]
  /// before using [`SlotHandle::get`] or [`SlotHandle::get_mut`].
  ///
  /// Returns `None` if the pool is exhausted.
  pub fn alloc<'a>(&'a self, handle: &mut SlotHandle<'a, T>) -> bool {
    loop {
      match self.try_alloc(handle) {
        None => return true,
        Some(e) => match e {
          TryAllocFailReason::BufferFull => return false,
          TryAllocFailReason::CASFailed => continue,
        },
      }
    }
  }

  pub fn try_alloc<'a>(&'a self, handle: &mut SlotHandle<'a, T>) -> Option<TryAllocFailReason> {
    let head = self.head.load(Ordering::Acquire);
    if unpack(head).1 >= self.cap {
      return Some(TryAllocFailReason::BufferFull);
    }

    if unsafe { self.try_alloc_unchecked_with_head_into_buf(head, handle) } {
      None
    } else {
      Some(TryAllocFailReason::CASFailed)
    }
  }

  unsafe fn try_alloc_unchecked_with_head_into_buf<'a>(
    &'a self,
    head: usize,
    handle: &mut SlotHandle<'a, T>,
  ) -> bool {
    let (tag, index) = unpack(head);
    let next_head = unsafe { &*self.buf.add(index) }
      .next
      .load(Ordering::Acquire);
    if self
      .head
      .compare_exchange(
        head,
        pack(tag + 1, next_head),
        Ordering::AcqRel,
        Ordering::Relaxed,
      )
      .is_err()
    {
      return false;
    }
    let slot = unsafe { self.buf.add(index) };

    *handle = SlotHandle {
      idx: index,
      elem: unsafe { &*slot }.elem.get(),
      pool: &self,
    };

    true
  }

  pub unsafe fn try_alloc_unchecked<'a>(&'a self, handle: &mut SlotHandle<'a, T>) -> bool {
    let head = self.head.load(Ordering::Acquire);
    unsafe { self.try_alloc_unchecked_with_head_into_buf(head, handle) }
  }

  /// Allocates one slot without checking whether the pool is exhausted.
  ///
  /// # Safety
  ///
  /// The caller must ensure that the pool still contains at least one free
  /// slot before calling this function.
  pub unsafe fn alloc_unchecked<'a>(&'a self, handle: &mut SlotHandle<'a, T>) {
    loop {
      if unsafe { self.try_alloc_unchecked(handle) } {
        return;
      }
    }
  }

  fn try_free_unchecked(&self, slot_idx: usize) -> bool {
    let head = self.head.load(Ordering::Acquire);
    let (tag, index) = unpack(head);
    unsafe { &*self.buf.add(slot_idx) }
      .next
      .store(index, Ordering::Release);
    self
      .head
      .compare_exchange(
        head,
        pack(tag + 1, slot_idx),
        Ordering::AcqRel,
        Ordering::Relaxed,
      )
      .is_ok()
  }

  /// Returns a slot to the freelist.
  ///
  /// This is used internally from [`Drop`] for [`SlotHandle`].
  fn free_unchecked(&self, slot_idx: usize) {
    while !self.try_free_unchecked(slot_idx) {}
  }
}

unsafe impl<T> Sync for MemPool<T> {}

pub struct SlotHandle<'a, T> {
  idx: usize,
  elem: *mut MaybeUninit<T>,
  pool: &'a MemPool<T>,
}

impl<'a, T> SlotHandle<'a, T> {
  /// Initializes the value stored in this slot.
  ///
  pub unsafe fn init(&mut self, value: T) {
    unsafe { &mut (*self.elem) }.write(value);
  }

  /// Returns an immutable reference to the initialized value.
  ///
  /// # Safety
  ///
  /// The caller must ensure the slot has already been initialized with
  /// [`SlotHandle::init`].
  pub unsafe fn get(&self) -> &T {
    unsafe { &*((*self.elem).as_ptr()) }
  }
}

impl<'a, T> Drop for SlotHandle<'a, T> {
  fn drop(&mut self) {
    let ref_cnt = unsafe { &*self.pool.buf.add(self.idx) }
      .ref_cnt
      .fetch_sub(1, Ordering::AcqRel);
    if ref_cnt == 0 {
      unsafe {
        (*self.elem).assume_init_drop();
      }
      self.pool.free_unchecked(self.idx);
    }
  }
}

impl<'a, T> Clone for SlotHandle<'a, T> {
  fn clone(&self) -> Self {
    unsafe { &*self.pool.buf.add(self.idx) }
      .ref_cnt
      .fetch_add(1, Ordering::AcqRel);

    SlotHandle {
      idx: self.idx,
      elem: self.elem.clone(),
      pool: self.pool,
    }
  }
}
