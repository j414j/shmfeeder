use std::io;

#[derive(Debug)]
pub enum ShmError {
  QueueNotReady,
  QueueAlreadyAcquired,
  CorruptedQueue,
  VersionMismatch,
  BadMagicNum,
  LengthNotPowerOfTwo,
  UnalignedPtr,
  Io(io::Error),
}

impl From<io::Error> for ShmError {
  fn from(value: io::Error) -> Self {
    Self::Io(value)
  }
}

pub type ShmResult<T> = Result<T, ShmError>;
