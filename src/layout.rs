use std::sync::atomic::{AtomicU8, AtomicUsize};

use crate::heartbeats::Heartbeats;

pub struct ShmHeader {
  pub magic: u64,
  pub version: u64,
  pub n_slots: usize,
  pub queue_offset: usize,
  pub last_committed_slot: AtomicUsize,
  pub state: AtomicU8,
}

#[repr(u8)]
pub enum ShmState {
  Starting = 0,
  Ready = 1,
  ShuttingDown = 2,
  Uninit = 3,

  Invalid = 255,
}

impl From<u8> for ShmState {
  fn from(value: u8) -> Self {
    match value {
      0 => Self::Starting,
      1 => Self::Ready,
      2 => Self::ShuttingDown,
      3 => Self::Uninit,
      _ => Self::Invalid,
    }
  }
}

pub struct ShmQueue<const MAX_CONSUMERS: usize> {
  pub header: ShmHeader,
  pub heartbeats: Heartbeats<MAX_CONSUMERS>,
}
