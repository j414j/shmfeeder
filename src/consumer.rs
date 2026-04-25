use std::{ffi::CString, io, str::FromStr, sync::atomic::Ordering};

use crate::{
  error::{ShmError, ShmResult},
  layout::{ShmQueue, ShmState},
  queue::{BroadCastQueue, BroadcastReadHandle},
};

fn try_init_shared_memory(name: &CString) -> ShmResult<i32> {
  let memory_fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDWR, 0) };
  if memory_fd < 0 {
    Err(io::Error::last_os_error().into())
  } else {
    Ok(memory_fd)
  }
}

fn try_attach_shared_memory<T, const MAX_CONSUMERS: usize>(
  name: &CString,
  fd: i32,
  magic: u64,
  version: u64,
  #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
  #[cfg(not(feature = "no-heartbeats"))] liveness_tolerance: u64,
) -> ShmResult<(*mut ShmQueue<MAX_CONSUMERS>, usize)>
where
  T: Copy,
{
  let initial_size = std::mem::size_of::<ShmQueue<MAX_CONSUMERS>>();
  let ptr = unsafe {
    libc::mmap(
      std::ptr::null_mut(),
      initial_size,
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

  let queue = unsafe { &mut *(ptr as *mut ShmQueue<MAX_CONSUMERS>) };

  let queue_state = queue.header.state.load(Ordering::Acquire);

  match queue_state.into() {
    ShmState::Starting | ShmState::Uninit | ShmState::ShuttingDown => {
      Err(ShmError::QueueNotReady(queue_state.into()))
    }
    ShmState::Ready => {
      #[cfg(not(feature = "no-heartbeats"))]
      if !queue
        .heartbeats
        .producer
        .heartbeat
        .is_alive(now_timestamp, liveness_tolerance)
      {
        return Err(ShmError::NoActiveProducer);
      }

      if queue.header.magic != magic {
        return Err(ShmError::BadMagicNum);
      }

      if queue.header.version != version {
        return Err(ShmError::VersionMismatch);
      }

      #[cfg(not(feature = "no-consumer-heartbeat"))]
      let consumer_id = queue.heartbeats.consumers.new_consumer(
        unsafe { libc::getpid() },
        now_timestamp,
        liveness_tolerance,
      );

      #[cfg(not(feature = "no-consumer-heartbeat"))]
      if consumer_id.is_none() {
        return Err(ShmError::MaxConsumerLimitReached);
      }

      let n_slots = queue.header.n_slots;
      let per_slot_size = BroadCastQueue::<T>::slot_layout().size;

      let final_queue_size = initial_size + queue.header.queue_offset + n_slots * per_slot_size;
      let munmap = unsafe { libc::munmap(ptr, initial_size) };

      if munmap != 0 {
        let err = io::Error::last_os_error();
        unsafe {
          libc::close(fd);
          libc::shm_unlink(name.as_ptr());
        }
        return Err(err.into());
      }

      let final_mmap = unsafe {
        libc::mmap(
          std::ptr::null_mut(),
          final_queue_size,
          libc::PROT_READ | libc::PROT_WRITE,
          libc::MAP_SHARED,
          fd,
          0,
        )
      };
      if final_mmap == libc::MAP_FAILED {
        let err = io::Error::last_os_error();
        unsafe {
          libc::close(fd);
          libc::shm_unlink(name.as_ptr());
        }
        return Err(err.into());
      }
      #[cfg(not(feature = "no-consumer-heartbeat"))]
      let consumer_heartbeat = consumer_id.unwrap();
      #[cfg(feature = "no-consumer-heartbeat")]
      let consumer_heartbeat = 0;

      Ok((
        final_mmap as *mut ShmQueue<MAX_CONSUMERS>,
        consumer_heartbeat,
      ))
    }
    ShmState::Invalid => Err(ShmError::CorruptedQueue),
  }
}

pub struct ConsumerBuilder {
  name: CString,
  magic: u64,
  version: u64,
  #[cfg(not(feature = "no-heartbeats"))]
  liveness_tolerance: u64,
}

impl ConsumerBuilder {
  pub fn new(path: &str) -> ShmResult<Self> {
    let name = CString::from_str(path).map_err(|e| io::Error::other(e))?;

    Ok(Self {
      name,
      magic: 0,
      version: 0,
      #[cfg(not(feature = "no-heartbeats"))]
      liveness_tolerance: 1000,
    })
  }

  pub fn with_magic(mut self, magic: u64) -> Self {
    self.magic = magic;
    self
  }
  pub fn with_version(mut self, version: u64) -> Self {
    self.version = version;
    self
  }
  #[cfg(not(feature = "no-heartbeats"))]
  pub fn with_liveness_tolerance(mut self, liveness_tolerance: u64) -> Self {
    self.liveness_tolerance = liveness_tolerance;
    self
  }
  pub fn build<T, const MAX_CONSUMERS: usize>(
    self,
    #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
  ) -> ShmResult<Consumer<T, MAX_CONSUMERS>>
  where
    T: Copy,
  {
    Consumer::new(
      self.name,
      self.magic,
      self.version,
      #[cfg(not(feature = "no-heartbeats"))]
      now_timestamp,
      #[cfg(not(feature = "no-heartbeats"))]
      self.liveness_tolerance,
    )
  }
}

pub struct Consumer<T, const MAX_CONSUMERS: usize>
where
  T: Copy,
{
  mmap_ptr: *mut ShmQueue<MAX_CONSUMERS>,
  mmap_size: usize,
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  id: usize,
  fd: i32,
  #[cfg(not(feature = "no-heartbeats"))]
  liveness_tolerance: u64,
  #[cfg(not(feature = "no-heartbeats"))]
  liveness_check_periods: u64,
  #[cfg(not(feature = "no-heartbeats"))]
  last_liveness_check: u64,
  read_handle: BroadcastReadHandle<T>,
}

impl<T, const MAX_CONSUMERS: usize> Consumer<T, MAX_CONSUMERS>
where
  T: Copy,
{
  fn new(
    name: CString,
    magic: u64,
    version: u64,
    #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
    #[cfg(not(feature = "no-heartbeats"))] liveness_tolerance: u64,
  ) -> ShmResult<Self> {
    let queue_layout = BroadCastQueue::<T>::slot_layout();

    let fd = try_init_shared_memory(&name)?;
    let (ptr, id) = try_attach_shared_memory::<T, _>(
      &name,
      fd,
      magic,
      version,
      #[cfg(not(feature = "no-heartbeats"))]
      now_timestamp,
      #[cfg(not(feature = "no-heartbeats"))]
      liveness_tolerance,
    )?;

    // id is only used to update our heartbeat
    #[cfg(feature = "no-consumer-heartbeat")]
    let _id = id;

    let num_slots = unsafe { (*ptr).header.n_slots };

    let queue = unsafe {
      BroadCastQueue::from_raw_parts(
        ptr
          .byte_add(std::mem::size_of::<ShmQueue<MAX_CONSUMERS>>())
          .byte_add((*ptr).header.queue_offset) as *mut u8,
        queue_layout.size * num_slots,
        &mut (*ptr).header.last_committed_slot,
      )?
    };
    let read_handle = BroadcastReadHandle::new(queue);

    let mmap_size = std::mem::size_of::<ShmQueue<MAX_CONSUMERS>>()
      + unsafe { (*ptr).header.queue_offset }
      + queue_layout.size * num_slots;

    Ok(Self {
      mmap_ptr: ptr,
      mmap_size,
      fd,
      #[cfg(not(feature = "no-consumer-heartbeat"))]
      id,
      read_handle,
      #[cfg(not(feature = "no-heartbeats"))]
      liveness_tolerance,
      #[cfg(not(feature = "no-heartbeats"))]
      liveness_check_periods: liveness_tolerance / 2,
      #[cfg(not(feature = "no-heartbeats"))]
      last_liveness_check: now_timestamp,
    })
  }

  #[inline]
  #[cfg(not(feature = "no-heartbeats"))]
  pub fn try_read(&mut self, now_timestamp: u64) -> ShmResult<&T> {
    if now_timestamp - self.last_liveness_check > self.liveness_check_periods {
      let queue = unsafe { &mut *self.mmap_ptr };
      if !queue
        .heartbeats
        .producer
        .heartbeat
        .is_alive(now_timestamp, self.liveness_tolerance)
      {
        return Err(ShmError::NoActiveProducer);
      }
      #[cfg(not(feature = "no-consumer-heartbeat"))]
      queue
        .heartbeats
        .consumers
        .update_heartbeat(self.id, now_timestamp);
      self.last_liveness_check = now_timestamp;
    }
    unsafe { self.read_handle.try_read() }.ok_or(ShmError::NoData)
  }
  #[inline]
  #[cfg(feature = "no-heartbeats")]
  pub fn try_read(&mut self) -> ShmResult<&T> {
    unsafe { self.read_handle.try_read() }.ok_or(ShmError::NoData)
  }
}

impl<T, const MAX_CONSUMERS: usize> Drop for Consumer<T, MAX_CONSUMERS>
where
  T: Copy,
{
  fn drop(&mut self) {
    unsafe {
      #[cfg(not(feature = "no-consumer-heartbeat"))]
      (*self.mmap_ptr).heartbeats.consumers.drop_consumer(self.id);

      libc::munmap(self.mmap_ptr as *mut libc::c_void, self.mmap_size);
      libc::close(self.fd);
    }
  }
}
