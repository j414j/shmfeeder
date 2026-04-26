use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use libc::pid_t;
#[cfg(not(feature = "no-consumer-heartbeat"))]
use std::sync::atomic::AtomicUsize;

#[repr(C)]
pub struct ItemHeartbeat {
  pid: AtomicI32,
  last_update_us: AtomicU64,
}
#[repr(C)]
#[cfg(not(feature = "no-consumer-heartbeat"))]
pub struct ConsumerHeartbeat {
  heartbeat: ItemHeartbeat,
  sequence: AtomicUsize,
}

#[repr(C)]
#[cfg(not(feature = "no-consumer-heartbeat"))]
pub struct ConsumerHeartbeatsMeta {
  pub prev_consumer: AtomicUsize,
  pub buffer_offset: usize,
  pub max_consumers: usize,
}

#[repr(C)]
pub struct ProducerHeartbeat {
  pub heartbeat: ItemHeartbeat,
}

#[repr(C)]
pub struct Heartbeats {
  pub producer: ProducerHeartbeat,
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  pub consumers: ConsumerHeartbeatsMeta,
}

impl ItemHeartbeat {
  pub fn new(pid: pid_t) -> Self {
    Self {
      pid: AtomicI32::new(pid),
      last_update_us: AtomicU64::new(0),
    }
  }

  #[inline]
  pub fn update(&self, val: u64) {
    self.last_update_us.store(val, Ordering::Relaxed)
  }

  #[inline]
  pub fn get_value(&self) -> u64 {
    self.last_update_us.load(Ordering::Relaxed)
  }

  #[inline]
  pub fn is_alive(&self, now: u64, tolerance: u64) -> bool {
    now.saturating_sub(self.get_value()) < tolerance
  }

  #[inline]
  pub fn get_pid(&self) -> pid_t {
    self.pid.load(Ordering::Acquire)
  }

  #[inline]
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  fn set_pid(&self, pid: pid_t) {
    self.pid.store(pid, Ordering::Release);
  }
}

#[cfg(not(feature = "no-consumer-heartbeat"))]
impl ConsumerHeartbeat {
  #[inline]
  pub fn get_heartbeat(&self) -> &ItemHeartbeat {
    &self.heartbeat
  }

  #[inline]
  pub fn try_reserve(&self, now: u64, tolerance: u64) -> bool {
    // if the heartbeat is dead, we proceed to reserve this slot
    // by first updating the heartbeat and then doing a CAS on
    // the sequence number
    if !self.heartbeat.is_alive(now, tolerance) {
      self.heartbeat.update(now);
      let curr_seq = self.sequence.load(Ordering::Acquire);
      self
        .sequence
        .compare_exchange(
          curr_seq,
          curr_seq.wrapping_add(1),
          Ordering::AcqRel,
          Ordering::Relaxed,
        )
        .is_ok()
    } else {
      false
    }
  }
}

#[cfg(not(feature = "no-consumer-heartbeat"))]
impl ConsumerHeartbeatsMeta {
  pub fn new(max_consumers: usize, buffer_offset: usize) -> Self {
    Self {
      prev_consumer: AtomicUsize::new(max_consumers),
      max_consumers,
      buffer_offset,
    }
  }

  #[inline]
  unsafe fn get_buffer_item(&self, index: usize) -> *mut ConsumerHeartbeat {
    unsafe {
      ((self as *const Self as *const u8 as *mut u8).byte_add(self.buffer_offset)
        as *mut ConsumerHeartbeat)
        .add(index)
    }
  }

  pub fn new_consumer(&self, pid: pid_t, now: u64, tolerance: u64) -> Option<usize> {
    for i in 0..self.max_consumers {
      let consumer = unsafe { &mut *self.get_buffer_item(i) };
      if consumer.try_reserve(now, tolerance) {
        let heartbeat = consumer.get_heartbeat();
        heartbeat.set_pid(pid);
        heartbeat.update(now);
        self.prev_consumer.store(i, Ordering::Release);
        return Some(i);
      }
    }

    return None;
  }

  #[inline]
  pub fn drop_consumer(&self, id: usize) -> bool {
    if id >= self.max_consumers {
      return false;
    }

    // mark the slot stale
    unsafe { &*self.get_buffer_item(id) }
      .get_heartbeat()
      .update(0);
    true
  }

  #[inline]
  pub fn update_heartbeat(&self, id: usize, now: u64) {
    if id >= self.max_consumers {
      return;
    }

    unsafe { &*self.get_buffer_item(id) }
      .get_heartbeat()
      .update(now);
  }

  fn find_next_consumer(&self, now: u64, tolerance: u64) -> usize {
    let mut ret = self.max_consumers;
    for i in 0..self.max_consumers {
      if unsafe { &*self.get_buffer_item(i) }
        .get_heartbeat()
        .is_alive(now, tolerance)
      {
        ret = i;
        break;
      }
    }

    self.prev_consumer.store(ret, Ordering::Release);
    ret
  }

  pub fn is_any_consumer_alive(&self, now: u64, tolerance: u64) -> bool {
    let mut prev_consumer = self.prev_consumer.load(Ordering::Acquire);

    while prev_consumer < self.max_consumers {
      // this is the fast patb
      if unsafe { &*self.get_buffer_item(prev_consumer) }
        .get_heartbeat()
        .is_alive(now, tolerance)
      {
        return true;
      }
      prev_consumer = self.find_next_consumer(now, tolerance);
    }

    false
  }
}
