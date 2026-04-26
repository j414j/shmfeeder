use std::io;

use crate::layout::ShmState;

/// Error type returned by producer and consumer operations.
#[derive(Debug)]
pub enum ShmError {
  /// The shared-memory queue exists but is not ready for consumers.
  QueueNotReady(ShmState),
  /// Another live producer already owns the queue. The value is its process ID.
  QueueAlreadyAcquired(i32),
  /// The consumer could not find a live producer heartbeat.
  NoActiveProducer,
  /// The producer could not find a live consumer heartbeat.
  NoActiveConsumer,
  /// No unread item is currently available for this consumer.
  NoData,
  /// The shared-memory contents do not look like a valid queue.
  CorruptedQueue,
  /// The queue version does not match the requested version.
  VersionMismatch,
  /// The queue magic number does not match the requested magic number.
  BadMagicNum,
  /// The configured ring-buffer length is not a non-zero power of two.
  LengthNotPowerOfTwo,
  /// The mapped data buffer was not aligned for the payload type.
  UnalignedPtr,
  /// All configured consumer heartbeat slots are currently in use.
  MaxConsumerLimitReached,
  /// An operating-system error from a POSIX shared-memory or mapping call.
  Io(io::Error),
}

impl std::fmt::Display for ShmError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::QueueNotReady(state) => write!(f, "shared-memory queue is not ready: {state:?}"),
      Self::QueueAlreadyAcquired(pid) => {
        write!(
          f,
          "shared-memory queue is already acquired by producer pid {pid}"
        )
      }
      Self::NoActiveProducer => f.write_str("no active producer heartbeat found"),
      Self::NoActiveConsumer => f.write_str("no active consumer heartbeat found"),
      Self::NoData => f.write_str("no unread data is currently available"),
      Self::CorruptedQueue => f.write_str("shared-memory queue is corrupted or incompatible"),
      Self::VersionMismatch => f.write_str("shared-memory queue version does not match"),
      Self::BadMagicNum => f.write_str("shared-memory queue magic number does not match"),
      Self::LengthNotPowerOfTwo => {
        f.write_str("shared-memory queue length must be a non-zero power of two")
      }
      Self::UnalignedPtr => f.write_str("shared-memory data buffer is not properly aligned"),
      Self::MaxConsumerLimitReached => f.write_str("maximum consumer limit reached"),
      Self::Io(err) => write!(f, "shared-memory OS error: {err}"),
    }
  }
}

impl std::error::Error for ShmError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::Io(err) => Some(err),
      _ => None,
    }
  }
}

impl From<io::Error> for ShmError {
  fn from(value: io::Error) -> Self {
    Self::Io(value)
  }
}

/// Convenient result alias for `shmfeeder` operations.
pub type ShmResult<T> = Result<T, ShmError>;
