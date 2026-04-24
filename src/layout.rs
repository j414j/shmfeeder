use std::sync::atomic::AtomicU8;

use crate::heartbeats::Heartbeats;
use crate::queue::BroadCastQueue;

pub struct ShmHeader {
  pub magic: u64,
  pub version: u64,
  pub n_slots: usize,
  pub state: AtomicU8,
  pub queue_offset: usize,
}

#[repr(u8)]
pub enum ShmState {
  Starting = 0,
  Ready = 1,
  ShuttingDown = 2,
  Uninit = 3
}

pub struct ShmQueue<T, const MAX_CONSUMERS: usize> {
  pub header: ShmHeader,
  pub heartbeats: Heartbeats<MAX_CONSUMERS>,
  pub queue: BroadCastQueue<T>,
}
