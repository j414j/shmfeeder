use std::io;

use crate::layout::ShmState;

#[derive(Debug)]
pub enum ShmError {
  QueueNotReady(ShmState),
  QueueAlreadyAcquired,
  NoActiveProducer,
  NoActiveConsumer,
  NoData,
  CorruptedQueue,
  VersionMismatch,
  BadMagicNum,
  LengthNotPowerOfTwo,
  UnalignedPtr,
  MaxConsumerLimitReached,
  Io(io::Error),
}

impl From<io::Error> for ShmError {
  fn from(value: io::Error) -> Self {
    Self::Io(value)
  }
}

pub type ShmResult<T> = Result<T, ShmError>;
