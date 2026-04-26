use std::sync::atomic::{AtomicU8, AtomicUsize};

#[cfg(not(feature = "no-consumer-heartbeat"))]
use crate::heartbeats::ConsumerHeartbeat;
#[cfg(not(feature = "no-heartbeats"))]
use crate::heartbeats::Heartbeats;
use crate::queue::BroadCastQueue;
#[cfg(not(feature = "no-consumer-heartbeat"))]
use std::mem::offset_of;

// The queue layout is such
// |------------------ HEADER --------------------|                                 --
// |------------- Producer Heartbeat -------------|                                   |
// |----------- Consumer heartbeat Meta-----------| -                                 |
// |---------- Consumer Heartbeat padding --------|  |-> Consumer buffer offset      |-> data buffer offset
// |----------- Consumer Heartbeat Buffer --------| -                                 |
// |---------------- Data Padding ----------------|                                   |
// |---------------- Data buffer -----------------|                                 --

#[repr(C)]
pub struct ShmHeader {
  pub magic: u64,
  pub version: u64,
  pub n_slots: usize,
  pub queue_offset: usize,
  pub last_committed_slot: AtomicUsize,
  pub state: AtomicU8,
}

#[repr(u8)]
#[derive(Debug)]
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

#[repr(C)]
pub struct ShmQueue {
  pub header: ShmHeader,
  #[cfg(not(feature = "no-heartbeats"))]
  pub heartbeats: Heartbeats,
}

#[inline]
pub(crate) fn align_ptr_up<T>(base: *mut T, align: usize) -> *mut T {
  ((base as usize + align - 1) & !(align - 1)) as *mut T
}

#[inline]
#[cfg(not(feature = "no-consumer-heartbeat"))]
pub(crate) fn calculate_consumer_heartbeat_offset_queue_end(base: *mut ShmQueue) -> usize {
  let header_end = unsafe { base.byte_add(size_of::<ShmQueue>()) };
  (align_ptr_up(header_end, align_of::<ConsumerHeartbeat>()) as usize)
    .saturating_sub(header_end as usize)
}

#[inline]
#[cfg(not(feature = "no-consumer-heartbeat"))]
pub(crate) fn calculate_consumer_heartbeat_offset_meta_begin(base: *mut ShmQueue) -> usize {
  let queue_end_offset = calculate_consumer_heartbeat_offset_queue_end(base);
  let consumer_meta_offset = offset_of!(ShmQueue, heartbeats) + offset_of!(Heartbeats, consumers);
  size_of::<ShmQueue>() - consumer_meta_offset + queue_end_offset
}

#[inline]
#[cfg(not(feature = "no-consumer-heartbeat"))]
pub(crate) fn calculate_consumer_buffer_gross_size(
  base: *mut ShmQueue,
  max_consumers: usize,
) -> usize {
  calculate_consumer_heartbeat_offset_queue_end(base)
    + max_consumers * size_of::<ConsumerHeartbeat>()
}

#[inline]
pub(crate) fn calculate_data_buffer_offset_consumer_end<T>(consumer_end: *mut u8) -> usize
where
  T: Copy,
{
  let align = BroadCastQueue::<T>::slot_layout().align;
  let data_ptr = align_ptr_up(consumer_end, align);

  (data_ptr as usize).saturating_sub(consumer_end as usize)
}

#[inline]
pub(crate) fn calculate_data_buffer_offset_queue_begin<T>(
  base: *mut ShmQueue,
  #[cfg(not(feature = "no-consumer-heartbeat"))] max_consumers: usize,
) -> usize
where
  T: Copy,
{
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  let consumer_buffer_size = calculate_consumer_buffer_gross_size(base, max_consumers);

  #[cfg(feature = "no-consumer-heartbeat")]
  let consumer_buffer_size = 0;

  let consumer_end = size_of::<ShmQueue>() + consumer_buffer_size;

  calculate_data_buffer_offset_consumer_end::<T>(unsafe { base.byte_add(consumer_end) as *mut u8 })
    + consumer_end
}

/// Debug utility to print the contents of the Shared Memory Queue
#[cfg(debug_assertions)]
pub fn debug_print_shm<T>(queue: &ShmQueue)
where
  T: Copy,
{
  let slot_layout = crate::queue::BroadCastQueue::<T>::slot_layout();
  let base_addr = queue as *const ShmQueue as usize;
  let header_size = size_of::<crate::layout::ShmHeader>();
  let shm_queue_size = size_of::<ShmQueue>();
  #[cfg(not(feature = "no-heartbeats"))]
  let heartbeats_offset = std::mem::offset_of!(ShmQueue, heartbeats);
  #[cfg(not(feature = "no-heartbeats"))]
  let heartbeats_size = size_of::<Heartbeats>();
  #[cfg(not(feature = "no-heartbeats"))]
  let producer_hb_offset = heartbeats_offset + std::mem::offset_of!(Heartbeats, producer);
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  let consumers_hb_offset = heartbeats_offset + std::mem::offset_of!(Heartbeats, consumers);
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  let consumer_buffer_offset = queue.heartbeats.consumers.buffer_offset;
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  let max_consumers = queue.heartbeats.consumers.max_consumers;
  #[cfg(feature = "no-consumer-heartbeat")]
  let max_consumers = 0usize;
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  let consumer_buffer_begin = consumers_hb_offset + consumer_buffer_offset;
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  let consumer_buffer_size = max_consumers * size_of::<ConsumerHeartbeat>();
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  let consumer_buffer_end = consumer_buffer_begin + consumer_buffer_size;
  #[cfg(feature = "no-consumer-heartbeat")]
  let consumer_buffer_end = shm_queue_size;
  let data_area_begin = queue.header.queue_offset;
  let data_padding_size = data_area_begin.saturating_sub(consumer_buffer_end);
  let n_slots = queue.header.n_slots;
  let last_committed = queue
    .header
    .last_committed_slot
    .load(std::sync::atomic::Ordering::Acquire);
  let state = queue
    .header
    .state
    .load(std::sync::atomic::Ordering::Acquire);
  let state_name = match ShmState::from(state) {
    ShmState::Starting => "Starting",
    ShmState::Ready => "Ready",
    ShmState::ShuttingDown => "ShuttingDown",
    ShmState::Uninit => "Uninit",
    ShmState::Invalid => "Invalid",
  };

  let mut boundaries = vec![
    (0usize, format!("HEADER BEGIN (size = {header_size} bytes)")),
    #[cfg(not(feature = "no-heartbeats"))]
    (
      heartbeats_offset,
      format!("HEARTBEATS BEGIN (size = {heartbeats_size} bytes)"),
    ),
    #[cfg(not(feature = "no-heartbeats"))]
    (producer_hb_offset, "PRODUCER HEARTBEAT".to_string()),
    #[cfg(not(feature = "no-consumer-heartbeat"))]
    (consumers_hb_offset, "CONSUMER HEARTBEAT META".to_string()),
    (
      shm_queue_size,
      format!("SHMQUEUE END (size = {shm_queue_size} bytes)"),
    ),
    #[cfg(not(feature = "no-consumer-heartbeat"))]
    (
      consumer_buffer_begin,
      format!(
        "CONSUMER HEARTBEAT BUFFER BEGIN (item size = {} bytes, consumers = {})",
        size_of::<ConsumerHeartbeat>(),
        max_consumers
      ),
    ),
    #[cfg(not(feature = "no-consumer-heartbeat"))]
    (
      consumer_buffer_end,
      format!("CONSUMER HEARTBEAT BUFFER END (size = {consumer_buffer_size} bytes)"),
    ),
    (
      data_area_begin,
      format!(
        "DATA BUFFER BEGIN (aligned slot area, slot size = {} bytes, slots = {})",
        slot_layout.size, n_slots
      ),
    ),
  ];

  #[cfg(not(feature = "no-consumer-heartbeat"))]
  for consumer_idx in 0..max_consumers {
    boundaries.push((
      consumer_buffer_begin + consumer_idx * size_of::<ConsumerHeartbeat>(),
      format!("CONSUMER HEARTBEAT {consumer_idx} BEGIN"),
    ));
  }

  for slot_idx in 0..n_slots {
    boundaries.push((
      data_area_begin + slot_idx * slot_layout.size,
      format!("SLOT {slot_idx} BEGIN"),
    ));
  }
  boundaries.sort_by_key(|(offset, _)| *offset);
  let full_length = data_area_begin + n_slots * slot_layout.size;

  println!("========== SHARED MEMORY DUMP ==========");
  println!("base_ptr            = {:p}", queue);
  println!("total_size          = {} bytes", full_length);
  println!("header_size         = {} bytes", header_size);
  println!("shm_queue_size      = {} bytes", shm_queue_size);
  #[cfg(not(feature = "no-heartbeats"))]
  println!("heartbeats_offset   = 0x{heartbeats_offset:08x}");
  #[cfg(not(feature = "no-heartbeats"))]
  println!("heartbeats_size     = {} bytes", heartbeats_size);
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  println!("consumer_meta_offset= 0x{consumers_hb_offset:08x}");
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  println!("consumer_buf_offset = 0x{consumer_buffer_offset:08x} (relative to consumer meta)");
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  println!("consumer_buf_begin  = 0x{consumer_buffer_begin:08x}");
  #[cfg(not(feature = "no-consumer-heartbeat"))]
  println!("consumer_buf_size   = {} bytes", consumer_buffer_size);
  println!("max_consumers       = {}", max_consumers);
  println!("data_padding_size   = {} bytes", data_padding_size);
  println!("queue_offset        = 0x{data_area_begin:08x} (data buffer offset from base)");
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

  let bytes =
    unsafe { std::slice::from_raw_parts(queue as *const ShmQueue as *const u8, full_length) };
  let mut next_boundary_idx = 0usize;

  for line_start in (0..bytes.len()).step_by(16) {
    while next_boundary_idx < boundaries.len() && boundaries[next_boundary_idx].0 <= line_start {
      let (offset, label) = &boundaries[next_boundary_idx];
      println!("---- 0x{offset:08x} {label} ----");
      next_boundary_idx += 1;
    }

    let line_end = usize::min(line_start + 16, bytes.len());
    let line = &bytes[line_start..line_end];

    print!("{:08x}: ", base_addr + line_start);
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
