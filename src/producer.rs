use std::{ffi::CString, io, str::FromStr, sync::atomic::Ordering};

use libc::ftruncate;

#[cfg(not(feature = "no-heartbeats"))]
use crate::heartbeats::ItemHeartbeat;
#[cfg(debug_assertions)]
use crate::layout::debug_print_shm;
use crate::{
  error::{ShmError, ShmResult},
  layout::{ShmQueue, ShmState, calculate_data_buffer_offset_queue_begin},
  queue::{BroadCastQueue, BroadcastWriteHandle},
};
#[cfg(not(feature = "no-consumer-heartbeat"))]
use crate::{
  heartbeats::{ConsumerHeartbeat, ConsumerHeartbeatsMeta},
  layout::calculate_consumer_heartbeat_offset_meta_begin,
};
enum ConnectedMemoryType {
  New,
  Old,
}

fn try_init_shared_memory(name: &CString, size: usize) -> ShmResult<(ConnectedMemoryType, i32)> {
  let memory_fd = unsafe {
    libc::shm_open(
      name.as_ptr(),
      libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
      0o600,
    )
  };
  if memory_fd < 0 {
    let err = io::Error::last_os_error();
    match err.kind() {
      io::ErrorKind::AlreadyExists => {
        let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDWR, 0) };
        if fd < 0 {
          Err(io::Error::last_os_error().into())
        } else {
          Ok((ConnectedMemoryType::Old, fd))
        }
      }
      _ => Err(err.into()),
    }
  } else {
    let trunc_res = unsafe { ftruncate(memory_fd, size as libc::off_t) };
    if trunc_res != 0 {
      unsafe {
        libc::close(memory_fd);
        libc::shm_unlink(name.as_ptr());
      }

      Err(io::Error::last_os_error().into())
    } else {
      Ok((ConnectedMemoryType::New, memory_fd))
    }
  }
}

fn init_queue_at_ptr<T, const IS_NEW: bool>(
  ptr: *mut libc::c_void,
  magic: u64,
  version: u64,
  num_slots: usize,
  #[cfg(not(feature = "no-consumer-heartbeat"))] max_consumers: usize,
  #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
) where
  T: Copy,
{
  let queue = unsafe { &mut *(ptr as *mut ShmQueue) };

  queue
    .header
    .state
    .store(ShmState::Starting as u8, Ordering::Release);
  queue.header.magic = magic;
  queue.header.version = version;
  queue.header.n_slots = num_slots;
  if IS_NEW {
    queue
      .header
      .last_committed_slot
      .store(num_slots - 1, Ordering::Release);
  }

  #[cfg(not(feature = "no-consumer-heartbeat"))]
  {
    let consumer_buffer_offset =
      calculate_consumer_heartbeat_offset_meta_begin(ptr as *mut ShmQueue);
    queue.heartbeats.consumers = ConsumerHeartbeatsMeta::new(max_consumers, consumer_buffer_offset);
  }

  queue.header.queue_offset = calculate_data_buffer_offset_queue_begin::<T>(
    ptr as *mut ShmQueue,
    #[cfg(not(feature = "no-consumer-heartbeat"))]
    max_consumers,
  );

  #[cfg(not(feature = "no-heartbeats"))]
  {
    queue.heartbeats.producer.heartbeat = ItemHeartbeat::new(unsafe { libc::getpid() });
    queue.heartbeats.producer.heartbeat.update(now_timestamp);
  }

  queue
    .header
    .state
    .store(ShmState::Ready as u8, Ordering::Release);
}

fn setup_new_memory<T>(
  name: &CString,
  fd: i32,
  size: usize,
  magic: u64,
  version: u64,
  num_slots: usize,
  #[cfg(not(feature = "no-consumer-heartbeat"))] max_consumers: usize,
  #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
) -> ShmResult<*mut ShmQueue>
where
  T: Copy,
{
  let ptr = unsafe {
    libc::mmap(
      std::ptr::null_mut(),
      size,
      libc::PROT_READ | libc::PROT_WRITE,
      libc::MAP_SHARED,
      fd,
      0,
    )
  };
  if ptr == libc::MAP_FAILED {
    unsafe {
      libc::close(fd);
      libc::shm_unlink(name.as_ptr());
    }
    return Err(io::Error::last_os_error().into());
  }

  unsafe {
    // initialise memory to zero
    libc::memset(ptr, 0, size);
  }

  init_queue_at_ptr::<T, true>(
    ptr,
    magic,
    version,
    num_slots,
    #[cfg(not(feature = "no-consumer-heartbeat"))]
    max_consumers,
    #[cfg(not(feature = "no-heartbeats"))]
    now_timestamp,
  );

  Ok(ptr as *mut ShmQueue)
}

fn setup_old_memory<T>(
  fd: i32,
  size: usize,
  magic: u64,
  version: u64,
  num_slots: usize,
  #[cfg(not(feature = "no-consumer-heartbeat"))] max_consumers: usize,
  #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
  #[cfg(not(feature = "no-heartbeats"))] liveness_tolerance: u64,
) -> ShmResult<*mut ShmQueue>
where
  T: Copy,
{
  let ptr = unsafe {
    libc::mmap(
      std::ptr::null_mut(),
      size,
      libc::PROT_READ | libc::PROT_WRITE,
      libc::MAP_SHARED,
      fd,
      0,
    )
  };
  if ptr == libc::MAP_FAILED {
    unsafe {
      libc::close(fd);
    }
    return Err(io::Error::last_os_error().into());
  }

  let queue = unsafe { &mut *(ptr as *mut ShmQueue) };

  let queue_state = queue.header.state.load(Ordering::Acquire);

  match queue_state.into() {
    ShmState::Starting | ShmState::Ready | ShmState::ShuttingDown => {
      #[cfg(not(feature = "no-heartbeats"))]
      if queue
        .heartbeats
        .producer
        .heartbeat
        .is_alive(now_timestamp, liveness_tolerance)
      {
        let pid = queue.heartbeats.producer.heartbeat.get_pid();
        return Err(ShmError::QueueAlreadyAcquired(pid));
      }

      if queue.header.magic != magic {
        return Err(ShmError::BadMagicNum);
      }

      if queue.header.version != version {
        return Err(ShmError::VersionMismatch);
      }

      if queue.header.n_slots != num_slots {
        return Err(ShmError::CorruptedQueue);
      }

      #[cfg(not(feature = "no-consumer-heartbeat"))]
      if queue.heartbeats.consumers.max_consumers != max_consumers {
        return Err(ShmError::CorruptedQueue);
      }
    }
    ShmState::Invalid => return Err(ShmError::CorruptedQueue),
    ShmState::Uninit => {}
  }

  // ready to take ownership of queue and initialise it as new
  init_queue_at_ptr::<T, false>(
    ptr,
    magic,
    version,
    num_slots,
    #[cfg(not(feature = "no-consumer-heartbeat"))]
    max_consumers,
    #[cfg(not(feature = "no-heartbeats"))]
    now_timestamp,
  );

  Ok(ptr as *mut ShmQueue)
}

/// Builder for creating or taking over a named shared-memory producer queue.
///
/// The `num_slots` value passed to [`ProducerBuilder::new`] must be a non-zero
/// power of two. Producer and consumer processes must agree on the queue name,
/// payload type, magic number, and version.
pub struct ProducerBuilder {
  name: CString,
  num_slots: usize,
  magic: u64,
  version: u64,
  #[cfg(not(feature = "no-heartbeats"))]
  liveness_tolerance: u64,
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  max_consumers: usize,
}

impl ProducerBuilder {
  /// Creates a builder for a POSIX shared-memory object.
  ///
  /// `path` is passed to `shm_open` and should normally start with `/`, for
  /// example `"/ticks"`. `num_slots` is the ring-buffer length and must be a
  /// power of two.
  pub fn new(path: &str, num_slots: usize) -> ShmResult<Self> {
    let name = CString::from_str(path).map_err(|e| io::Error::other(e))?;

    Ok(Self {
      name,
      num_slots,
      magic: 0,
      version: 0,
      #[cfg(not(feature = "no-heartbeats"))]
      liveness_tolerance: 1000,
      #[cfg(not(feature = "no-consumer-heartbeat"))]
      max_consumers: 32,
    })
  }

  /// Sets an application-defined magic number used to reject incompatible queues.
  pub fn with_magic(mut self, magic: u64) -> Self {
    self.magic = magic;
    self
  }
  /// Sets an application-defined schema version used to reject incompatible queues.
  pub fn with_version(mut self, version: u64) -> Self {
    self.version = version;
    self
  }

  #[cfg(not(feature = "no-heartbeats"))]
  /// Sets the liveness tolerance, in the same timestamp units passed to
  /// [`ProducerBuilder::build`] and [`Producer::commit_next_slot`].
  ///
  /// The default is `1000`. If you pass microsecond timestamps, this means one
  /// millisecond.
  pub fn with_liveness_tolerance(mut self, liveness_tolerance: u64) -> Self {
    self.liveness_tolerance = liveness_tolerance;
    self
  }

  #[cfg(not(feature = "no-consumer-heartbeat"))]
  /// Sets the maximum number of concurrently attached consumers.
  ///
  /// The default is `32`.
  pub fn with_max_consumers(mut self, max_consumers: usize) -> Self {
    self.max_consumers = max_consumers;
    self
  }

  /// Creates a producer and initializes or takes over the shared-memory queue.
  ///
  /// When heartbeats are enabled, `now_timestamp` is recorded as the initial
  /// producer heartbeat. Use a monotonic or wall-clock timestamp consistently
  /// across all producers and consumers.
  pub fn build<T>(
    self,
    #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
  ) -> ShmResult<Producer<T>>
  where
    T: Copy,
  {
    Producer::new(
      self.name,
      self.num_slots,
      self.magic,
      self.version,
      #[cfg(not(feature = "no-consumer-heartbeat"))]
      self.max_consumers,
      #[cfg(not(feature = "no-heartbeats"))]
      now_timestamp,
      #[cfg(not(feature = "no-heartbeats"))]
      self.liveness_tolerance,
    )
  }
}

/// A single writer for a shared-memory broadcast queue.
///
/// A producer reserves the next slot with [`Producer::get_next_buffer`], writes a
/// fully initialized `T` into that pointer, and then publishes it with
/// [`Producer::commit_next_slot`].
pub struct Producer<T>
where
  T: Copy,
{
  mmap_ptr: *mut ShmQueue,
  mmap_size: usize,
  fd: i32,
  #[cfg(not(feature = "no-heartbeats"))]
  liveness_check_periods: u64,
  #[cfg(not(feature = "no-heartbeats"))]
  last_liveness_check: u64,
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  liveness_tolerance: u64,
  write_handle: BroadcastWriteHandle<T>,
}

impl<T> Producer<T>
where
  T: Copy,
{
  fn new(
    name: CString,
    num_slots: usize,
    magic: u64,
    version: u64,
    #[cfg(not(feature = "no-consumer-heartbeat"))] max_consumers: usize,
    #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
    #[cfg(not(feature = "no-heartbeats"))] liveness_tolerance: u64,
  ) -> ShmResult<Self> {
    let queue_layout = BroadCastQueue::<T>::slot_layout();

    #[cfg(not(feature = "no-consumer-heartbeat"))]
    let consumer_heartbeat_buffer_size =
      size_of::<ConsumerHeartbeat>() * max_consumers + align_of::<ConsumerHeartbeat>();
    #[cfg(feature = "no-consumer-heartbeat")]
    let consumer_heartbeat_buffer_size = 0;

    let size = std::mem::size_of::<ShmQueue>()
      + queue_layout.size * num_slots
      + queue_layout.align
      + consumer_heartbeat_buffer_size
      - 1;

    let (memory_type, fd) = try_init_shared_memory(&name, size)?;
    let ptr = match memory_type {
      ConnectedMemoryType::New => setup_new_memory::<T>(
        &name,
        fd,
        size,
        magic,
        version,
        num_slots,
        #[cfg(not(feature = "no-consumer-heartbeat"))]
        max_consumers,
        #[cfg(not(feature = "no-heartbeats"))]
        now_timestamp,
      )?,

      ConnectedMemoryType::Old => setup_old_memory::<T>(
        fd,
        size,
        magic,
        version,
        num_slots,
        #[cfg(not(feature = "no-consumer-heartbeat"))]
        max_consumers,
        #[cfg(not(feature = "no-heartbeats"))]
        now_timestamp,
        #[cfg(not(feature = "no-heartbeats"))]
        liveness_tolerance,
      )?,
    };

    #[cfg(debug_assertions)]
    debug_print_shm::<T>(unsafe { &*ptr });

    let queue = unsafe {
      BroadCastQueue::from_raw_parts(
        ptr.byte_add((*ptr).header.queue_offset) as *mut u8,
        queue_layout.size * num_slots,
        &mut (*ptr).header.last_committed_slot,
      )?
    };
    let write_handle = BroadcastWriteHandle::new(queue);

    Ok(Self {
      mmap_ptr: ptr,
      mmap_size: size,
      fd,
      write_handle,
      #[cfg(not(feature = "no-consumer-heartbeat"))]
      liveness_tolerance,
      #[cfg(not(feature = "no-heartbeats"))]
      last_liveness_check: now_timestamp,
      #[cfg(not(feature = "no-heartbeats"))]
      liveness_check_periods: liveness_tolerance / 2,
    })
  }

  #[inline]
  /// Returns a pointer to the next writable slot.
  ///
  /// Write exactly one initialized `T` into this pointer, then call
  /// [`Producer::commit_next_slot`] to publish it. Calling this again before
  /// committing overwrites the same pending slot.
  pub fn get_next_buffer(&mut self) -> *mut T {
    self.write_handle.get_next_buffer()
  }

  #[inline]
  #[cfg(not(feature = "no-heartbeats"))]
  /// Publishes the slot returned by [`Producer::get_next_buffer`].
  ///
  /// `now_timestamp` is also used to refresh the producer heartbeat. Once enough
  /// time has passed, the producer checks whether at least one consumer is still
  /// alive and returns [`ShmError::NoActiveConsumer`] if none are.
  pub fn commit_next_slot(&mut self, now_timestamp: u64) -> ShmResult<()> {
    self.write_handle.commit_next_slot();

    if now_timestamp.saturating_sub(self.last_liveness_check) > self.liveness_check_periods {
      let queue = unsafe { &mut *self.mmap_ptr };
      queue.heartbeats.producer.heartbeat.update(now_timestamp);
      self.last_liveness_check = now_timestamp;
      #[cfg(not(feature = "no-consumer-heartbeat"))]
      if !queue
        .heartbeats
        .consumers
        .is_any_consumer_alive(now_timestamp, self.liveness_tolerance)
      {
        return Err(ShmError::NoActiveConsumer);
      }
    }

    Ok(())
  }

  #[inline]
  #[cfg(feature = "no-heartbeats")]
  /// Publishes the slot returned by [`Producer::get_next_buffer`].
  pub fn commit_next_slot(&mut self) -> ShmResult<()> {
    self.write_handle.commit_next_slot();
    Ok(())
  }

  #[inline]
  #[cfg(debug_assertions)]
  /// Prints the shared-memory queue layout and contents for debugging.
  pub fn debug_print_queue(&self) {
    crate::layout::debug_print_shm::<T>(unsafe { &*self.mmap_ptr });
  }
}

impl<T> Drop for Producer<T>
where
  T: Copy,
{
  fn drop(&mut self) {
    unsafe {
      libc::munmap(self.mmap_ptr as *mut libc::c_void, self.mmap_size);
      libc::close(self.fd);
    }
  }
}
