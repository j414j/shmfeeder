use std::{
  ffi::CString,
  io,
  mem::{offset_of, size_of},
  slice,
  str::FromStr,
  sync::atomic::Ordering,
};

use libc::ftruncate;

use crate::{
  error::{ShmError, ShmResult},
  heartbeats::{Heartbeats, ItemHeartbeat},
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

fn init_queue_at_ptr<T, const MAX_CONSUMERS: usize>(
  ptr: *mut libc::c_void,
  magic: u64,
  version: u64,
  num_slots: usize,
  now_timestamp: u64,
) {
  let queue = unsafe { &mut *(ptr as *mut ShmQueue<MAX_CONSUMERS>) };

  queue
    .header
    .state
    .store(ShmState::Starting as u8, Ordering::Release);
  queue.header.magic = magic;
  queue.header.version = version;
  queue.header.n_slots = num_slots;
  queue
    .header
    .last_committed_slot
    .store(MAX_CONSUMERS - 1, Ordering::Release);

  let slot_layout = BroadCastQueue::<T>::slot_layout();
  let slot_begin = unsafe { ptr.byte_add(std::mem::size_of::<ShmQueue<MAX_CONSUMERS>>()) };
  // get an aligned pointer to slots array
  let slot_ptr =
    ((slot_begin as usize + slot_layout.align - 1) & !(slot_layout.align - 1)) as *mut libc::c_void;

  queue.header.queue_offset = unsafe { slot_ptr.byte_offset_from_unsigned(slot_begin) };

  // TODO: init the slots here

  queue.heartbeats.producer.heartbeat = ItemHeartbeat::new(unsafe { libc::getpid() });
  queue.heartbeats.producer.heartbeat.update(now_timestamp);

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
  now_timestamp: u64,
) -> ShmResult<*mut ShmQueue<MAX_CONSUMERS>> {
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

  init_queue_at_ptr::<T, MAX_CONSUMERS>(ptr, magic, version, num_slots, now_timestamp);

  Ok(ptr as *mut ShmQueue<MAX_CONSUMERS>)
}

fn setup_old_memory<T, const MAX_CONSUMERS: usize>(
  name: &CString,
  fd: i32,
  size: usize,
  magic: u64,
  version: u64,
  num_slots: usize,
  now_timestamp: u64,
  liveness_tolerance: u64,
) -> ShmResult<*mut ShmQueue<MAX_CONSUMERS>> {
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

  let queue = unsafe { &mut *(ptr as *mut ShmQueue<MAX_CONSUMERS>) };

  let queue_state = queue.header.state.load(Ordering::Acquire);

  match queue_state.into() {
    ShmState::Starting | ShmState::Ready | ShmState::ShuttingDown => {
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
  init_queue_at_ptr::<T, MAX_CONSUMERS>(ptr, magic, version, num_slots, now_timestamp);

  Ok(ptr as *mut ShmQueue<MAX_CONSUMERS>)
}

pub struct ProducerBuilder {
  name: CString,
  num_slots: usize,
  magic: u64,
  version: u64,
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
  pub fn with_liveness_tolerance(mut self, liveness_tolerance: u64) -> Self {
    self.liveness_tolerance = liveness_tolerance;
    self
  }
  pub fn build<T, const MAX_CONSUMERS: usize>(
    self,
    now_timestamp: u64,
  ) -> ShmResult<Producer<T, MAX_CONSUMERS>> {
    Producer::new(
      self.name,
      self.num_slots,
      self.magic,
      self.version,
      now_timestamp,
      self.liveness_tolerance,
    )
  }
}

pub struct Producer<T, const MAX_CONSUMERS: usize> {
  mmap_ptr: *mut ShmQueue<MAX_CONSUMERS>,
  mmap_size: usize,
  fd: i32,
  name: CString,
  write_handle: BroadcastWriteHandle<T>,
}

impl<T, const MAX_CONSUMERS: usize> Producer<T, MAX_CONSUMERS> {
  fn new(
    name: CString,
    num_slots: usize,
    magic: u64,
    version: u64,
    now_timestamp: u64,
    liveness_tolerance: u64,
  ) -> ShmResult<Self> {
    let queue_layout = BroadCastQueue::<T>::slot_layout();

    let size = std::mem::size_of::<ShmQueue<MAX_CONSUMERS>>()
      + queue_layout.size * num_slots
      + queue_layout.align
      - 1;

    let (memory_type, fd) = try_init_shared_memory(&name, size)?;
    let ptr = match memory_type {
      ConnectedMemoryType::New => {
        setup_new_memory::<T, _>(&name, fd, size, magic, version, num_slots, now_timestamp)?
      }

      ConnectedMemoryType::Old => setup_old_memory::<T, _>(
        &name,
        fd,
        size,
        magic,
        version,
        num_slots,
        now_timestamp,
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
      name,
      write_handle,
    })
  }

  #[inline]
  pub fn get_next_buffer(&mut self) -> *mut T {
    self.write_handle.get_next_buffer()
  }

  #[inline]
  pub fn commit_next_slot(&mut self) {
    self.write_handle.commit_next_slot();
  }

  pub fn print_shm(&self) {
    let queue = unsafe { &*self.mmap_ptr };
    let slot_layout = BroadCastQueue::<T>::slot_layout();
    let header_size = size_of::<crate::layout::ShmHeader>();
    let shm_queue_size = size_of::<ShmQueue<MAX_CONSUMERS>>();
    let heartbeats_offset = offset_of!(ShmQueue<MAX_CONSUMERS>, heartbeats);
    let heartbeats_size = size_of::<Heartbeats<MAX_CONSUMERS>>();
    let producer_hb_offset = heartbeats_offset + offset_of!(Heartbeats<MAX_CONSUMERS>, producer);
    let consumers_hb_offset = heartbeats_offset + offset_of!(Heartbeats<MAX_CONSUMERS>, consumers);
    let queue_offset = queue.header.queue_offset;
    let queue_base_offset = shm_queue_size;
    let data_area_begin = queue_base_offset + queue_offset;
    let n_slots = queue.header.n_slots;
    let last_committed = queue.header.last_committed_slot.load(Ordering::Acquire);
    let state = queue.header.state.load(Ordering::Acquire);
    let state_name = match ShmState::from(state) {
      ShmState::Starting => "Starting",
      ShmState::Ready => "Ready",
      ShmState::ShuttingDown => "ShuttingDown",
      ShmState::Uninit => "Uninit",
      ShmState::Invalid => "Invalid",
    };

    let mut boundaries = vec![
      (0usize, format!("HEADER BEGIN (size = {header_size} bytes)")),
      (
        heartbeats_offset,
        format!("HEARTBEATS BEGIN (size = {heartbeats_size} bytes)"),
      ),
      (producer_hb_offset, "PRODUCER HEARTBEAT".to_string()),
      (consumers_hb_offset, "CONSUMER HEARTBEATS".to_string()),
      (
        queue_base_offset,
        format!("SHMQUEUE END / DATA REGION BASE (size = {shm_queue_size} bytes)"),
      ),
      (
        data_area_begin,
        format!(
          "DATA AREA BEGIN (aligned slot area, slot size = {} bytes, slots = {})",
          slot_layout.size, n_slots
        ),
      ),
    ];

    for slot_idx in 0..n_slots {
      boundaries.push((
        data_area_begin + slot_idx * slot_layout.size,
        format!("SLOT {slot_idx} BEGIN"),
      ));
    }
    boundaries.sort_by_key(|(offset, _)| *offset);

    println!("========== SHARED MEMORY DUMP ==========");
    println!("base_ptr            = {:p}", self.mmap_ptr);
    println!("total_size          = {} bytes", self.mmap_size);
    println!("header_size         = {} bytes", header_size);
    println!("shm_queue_size      = {} bytes", shm_queue_size);
    println!("heartbeats_offset   = 0x{heartbeats_offset:08x}");
    println!("heartbeats_size     = {} bytes", heartbeats_size);
    println!("queue_base_offset   = 0x{queue_base_offset:08x}");
    println!("queue_offset        = 0x{queue_offset:08x} (relative to queue_base_offset)");
    println!("data_area_begin     = 0x{data_area_begin:08x}");
    println!("slot_size           = {} bytes", slot_layout.size);
    println!("slot_align          = {} bytes", slot_layout.align);
    println!("num_slots           = {}", n_slots);
    println!("last_committed_slot = {}", last_committed);
    println!("state               = {} ({state_name})", state);
    println!("----------------------------------------");
    println!("Section map:");
    for (offset, label) in &boundaries {
      println!("  0x{offset:08x}  {label}");
    }
    println!("----------------------------------------");

    let bytes = unsafe { slice::from_raw_parts(self.mmap_ptr as *const u8, self.mmap_size) };
    let mut next_boundary_idx = 0usize;

    for line_start in (0..bytes.len()).step_by(16) {
      while next_boundary_idx < boundaries.len() && boundaries[next_boundary_idx].0 <= line_start {
        let (offset, label) = &boundaries[next_boundary_idx];
        println!("---- 0x{offset:08x} {label} ----");
        next_boundary_idx += 1;
      }

      let line_end = usize::min(line_start + 16, bytes.len());
      let line = &bytes[line_start..line_end];

      print!("{line_start:08x}: ");
      for idx in 0..16 {
        if let Some(byte) = line.get(idx) {
          print!("{byte:02x} ");
        } else {
          print!("   ");
        }
      }

      print!("|");
      for byte in line {
        let ch = if byte.is_ascii_graphic() || *byte == b' ' {
          *byte as char
        } else {
          '.'
        };
        print!("{ch}");
      }
      print!("|");

      let mut inline_boundary_idx = next_boundary_idx;
      while inline_boundary_idx < boundaries.len() && boundaries[inline_boundary_idx].0 < line_end {
        let (offset, label) = &boundaries[inline_boundary_idx];
        print!("  <-- 0x{offset:08x} {label}");
        inline_boundary_idx += 1;
      }

      println!();
    }

    println!("========== END SHARED MEMORY DUMP ==========");
  }
}

impl<T, const MAX_CONSUMERS: usize> Drop for Producer<T, MAX_CONSUMERS> {
  fn drop(&mut self) {
    unsafe {
      libc::munmap(self.mmap_ptr as *mut libc::c_void, self.mmap_size);
      libc::close(self.fd);
      libc::shm_unlink(self.name.as_ptr());
    }
  }
}
