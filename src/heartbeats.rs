use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};

use libc::pid_t;

const INVALID_PID: pid_t = -1;
#[repr(C)]
pub struct ItemHeartbeat {
  pid: AtomicI32,
  last_update_us: AtomicU64,
}
#[repr(C)]
pub struct ConsumerHeartbeat {
  heartbeat: ItemHeartbeat,
  pub reserved: AtomicBool,
}

#[repr(C)]
pub struct ConsumerHeartbeats<const MAX_CONSUMERS: usize> {
  pub prev_consumer: AtomicUsize,
  pub consumers: [ConsumerHeartbeat; MAX_CONSUMERS],
}

#[repr(C)]
pub struct ProducerHeartbeat {
  pub heartbeat: ItemHeartbeat,
}

#[repr(C)]
pub struct Heartbeats<const MAX_CONSUMERS: usize> {
  pub producer: ProducerHeartbeat,
  pub consumers: ConsumerHeartbeats<MAX_CONSUMERS>,
}

pub enum TryNewConsumerFailReason {
  CASFailed,
  SlotsFull,
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
  fn set_pid(&self, pid: pid_t) {
    self.pid.store(pid, Ordering::Release);
  }
}

impl ConsumerHeartbeat {
  pub fn new(pid: pid_t) -> Self {
    Self {
      heartbeat: ItemHeartbeat::new(pid),
      reserved: AtomicBool::new(false),
    }
  }

  #[inline]
  pub fn get_heartbeat(&self) -> &ItemHeartbeat {
    &self.heartbeat
  }

  #[inline]
  pub fn is_reserved(&self) -> bool {
    self.reserved.load(Ordering::Acquire)
  }

  #[inline]
  pub fn try_reserve(&self, now: u64, tolerance: u64) -> bool {
    let cas_res = self
      .reserved
      .compare_exchange_weak(false, true, Ordering::AcqRel, Ordering::Relaxed)
      .is_ok();
    if !cas_res && !self.heartbeat.is_alive(now, tolerance) {
      // the consumer at this slot is dead
      return true;
    }

    return cas_res;
  }

  #[inline]
  pub fn free(&self) {
    self.reserved.store(false, Ordering::Release);
  }
}

impl<const MAX_CONSUMERS: usize> ConsumerHeartbeats<MAX_CONSUMERS> {
  pub fn new() -> Self {
    Self {
      prev_consumer: AtomicUsize::new(MAX_CONSUMERS),
      consumers: std::array::from_fn(|_| ConsumerHeartbeat::new(INVALID_PID)),
    }
  }

  pub fn new_consumer(&self, pid: pid_t, now: u64, tolerance: u64) -> Option<usize> {
    for i in 0..MAX_CONSUMERS {
      if self.consumers[i].try_reserve(now, tolerance) {
        let heartbeat = self.consumers[i].get_heartbeat();
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
    if id >= MAX_CONSUMERS {
      return false;
    }

    self.consumers[id].free();
    true
  }

  #[inline]
  pub fn update_heartbeat(&self, id: usize, now: u64) {
    if id >= MAX_CONSUMERS {
      return;
    }

    self.consumers[id].get_heartbeat().update(now);
  }

  fn find_next_consumer(&self, now: u64, tolerance: u64) -> usize {
    let mut ret = MAX_CONSUMERS;
    for i in 0..MAX_CONSUMERS {
      if self.consumers[i].is_reserved()
        && self.consumers[i].get_heartbeat().is_alive(now, tolerance)
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

    while prev_consumer < MAX_CONSUMERS {
      // this is the fast patb
      if self.consumers[prev_consumer]
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
