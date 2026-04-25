use std::{ffi::CString, io, str::FromStr, sync::atomic::Ordering};

use libc::ftruncate;

#[cfg(not(feature = "no-heartbeats"))]
use crate::heartbeats::ItemHeartbeat;
use crate::{
  error::{ShmError, ShmResult},
  layout::{ShmQueue, ShmState},
  queue::{BroadCastQueue, BroadcastWriteHandle},
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

fn init_queue_at_ptr<T, const MAX_CONSUMERS: usize, const IS_NEW: bool>(
  ptr: *mut libc::c_void,
  magic: u64,
  version: u64,
  num_slots: usize,
  #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
) where
  T: Copy,
{
  let queue = unsafe { &mut *(ptr as *mut ShmQueue<MAX_CONSUMERS>) };

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
  let slot_layout = BroadCastQueue::<T>::slot_layout();
  let slot_begin = unsafe { ptr.byte_add(std::mem::size_of::<ShmQueue<MAX_CONSUMERS>>()) };
  // get an aligned pointer to slots array
  let slot_ptr =
    ((slot_begin as usize + slot_layout.align - 1) & !(slot_layout.align - 1)) as *mut libc::c_void;

  queue.header.queue_offset = unsafe { slot_ptr.byte_offset_from_unsigned(slot_begin) };

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

fn setup_new_memory<T, const MAX_CONSUMERS: usize>(
  name: &CString,
  fd: i32,
  size: usize,
  magic: u64,
  version: u64,
  num_slots: usize,
  #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
) -> ShmResult<*mut ShmQueue<MAX_CONSUMERS>>
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

  init_queue_at_ptr::<T, MAX_CONSUMERS, true>(
    ptr,
    magic,
    version,
    num_slots,
    #[cfg(not(feature = "no-heartbeats"))]
    now_timestamp,
  );

  Ok(ptr as *mut ShmQueue<MAX_CONSUMERS>)
}

fn setup_old_memory<T, const MAX_CONSUMERS: usize>(
  fd: i32,
  size: usize,
  magic: u64,
  version: u64,
  num_slots: usize,
  #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
  #[cfg(not(feature = "no-heartbeats"))] liveness_tolerance: u64,
) -> ShmResult<*mut ShmQueue<MAX_CONSUMERS>>
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

  let queue = unsafe { &mut *(ptr as *mut ShmQueue<MAX_CONSUMERS>) };

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
        return Err(ShmError::QueueAlreadyAcquired);
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
    }
    ShmState::Invalid => return Err(ShmError::CorruptedQueue),
    ShmState::Uninit => {}
  }

  // ready to take ownership of queue and initialise it as new
  init_queue_at_ptr::<T, MAX_CONSUMERS, false>(
    ptr,
    magic,
    version,
    num_slots,
    #[cfg(not(feature = "no-heartbeats"))]
    now_timestamp,
  );

  Ok(ptr as *mut ShmQueue<MAX_CONSUMERS>)
}

pub struct ProducerBuilder {
  name: CString,
  num_slots: usize,
  magic: u64,
  version: u64,
  #[cfg(not(feature = "no-heartbeats"))]
  liveness_tolerance: u64,
}

impl ProducerBuilder {
  pub fn new(path: &str, num_slots: usize) -> ShmResult<Self> {
    let name = CString::from_str(path).map_err(|e| io::Error::other(e))?;

    Ok(Self {
      name,
      num_slots,
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
  ) -> ShmResult<Producer<T, MAX_CONSUMERS>>
  where
    T: Copy,
  {
    Producer::new(
      self.name,
      self.num_slots,
      self.magic,
      self.version,
      #[cfg(not(feature = "no-heartbeats"))]
      now_timestamp,
      #[cfg(not(feature = "no-heartbeats"))]
      self.liveness_tolerance,
    )
  }
}

pub struct Producer<T, const MAX_CONSUMERS: usize>
where
  T: Copy,
{
  mmap_ptr: *mut ShmQueue<MAX_CONSUMERS>,
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

impl<T, const MAX_CONSUMERS: usize> Producer<T, MAX_CONSUMERS>
where
  T: Copy,
{
  fn new(
    name: CString,
    num_slots: usize,
    magic: u64,
    version: u64,
    #[cfg(not(feature = "no-heartbeats"))] now_timestamp: u64,
    #[cfg(not(feature = "no-heartbeats"))] liveness_tolerance: u64,
  ) -> ShmResult<Self> {
    let queue_layout = BroadCastQueue::<T>::slot_layout();

    let size = std::mem::size_of::<ShmQueue<MAX_CONSUMERS>>()
      + queue_layout.size * num_slots
      + queue_layout.align
      - 1;

    let (memory_type, fd) = try_init_shared_memory(&name, size)?;
    let ptr = match memory_type {
      ConnectedMemoryType::New => setup_new_memory::<T, _>(
        &name,
        fd,
        size,
        magic,
        version,
        num_slots,
        #[cfg(not(feature = "no-heartbeats"))]
        now_timestamp,
      )?,

      ConnectedMemoryType::Old => setup_old_memory::<T, _>(
        fd,
        size,
        magic,
        version,
        num_slots,
        #[cfg(not(feature = "no-heartbeats"))]
        now_timestamp,
        #[cfg(not(feature = "no-heartbeats"))]
        liveness_tolerance,
      )?,
    };

    let queue = unsafe {
      BroadCastQueue::from_raw_parts(
        ptr
          .byte_add(std::mem::size_of::<ShmQueue<MAX_CONSUMERS>>())
          .byte_add((*ptr).header.queue_offset) as *mut u8,
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
  pub fn get_next_buffer(&mut self) -> *mut T {
    self.write_handle.get_next_buffer()
  }

  #[inline]
  #[cfg(not(feature = "no-heartbeats"))]
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
  pub fn commit_next_slot(&mut self) -> ShmResult<()> {
    self.write_handle.commit_next_slot();
    Ok(())
  }

  #[inline]
  #[cfg(debug_assertions)]
  pub fn debug_print_queue(&self) {
    crate::layout::debug_print_shm::<T, _>(unsafe { &*self.mmap_ptr });
  }
}

impl<T, const MAX_CONSUMERS: usize> Drop for Producer<T, MAX_CONSUMERS>
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
