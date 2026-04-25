use std::sync::atomic::{AtomicU8, AtomicUsize};

use crate::heartbeats::Heartbeats;

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
pub struct ShmQueue<const MAX_CONSUMERS: usize> {
  pub header: ShmHeader,
  pub heartbeats: Heartbeats<MAX_CONSUMERS>,
}

/// Debug utility to print the contents of the Shared Memory Queue
#[cfg(debug_assertions)]
pub fn debug_print_shm<T, const MAX_CONSUMERS: usize>(queue: &ShmQueue<MAX_CONSUMERS>)
where
  T: Copy,
{
  let slot_layout = crate::queue::BroadCastQueue::<T>::slot_layout();
  let header_size = size_of::<crate::layout::ShmHeader>();
  let shm_queue_size = size_of::<ShmQueue<MAX_CONSUMERS>>();
  let heartbeats_offset = std::mem::offset_of!(ShmQueue<MAX_CONSUMERS>, heartbeats);
  let heartbeats_size = size_of::<Heartbeats<MAX_CONSUMERS>>();
  let producer_hb_offset =
    heartbeats_offset + std::mem::offset_of!(Heartbeats<MAX_CONSUMERS>, producer);
  #[cfg(not(feature="no-consumer-heartbeat"))]
  let consumers_hb_offset =
    heartbeats_offset + std::mem::offset_of!(Heartbeats<MAX_CONSUMERS>, consumers);
  let queue_offset = queue.header.queue_offset;
  let queue_base_offset = shm_queue_size;
  let data_area_begin = queue_base_offset + queue_offset;
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
    (
      heartbeats_offset,
      format!("HEARTBEATS BEGIN (size = {heartbeats_size} bytes)"),
    ),
    (producer_hb_offset, "PRODUCER HEARTBEAT".to_string()),
    #[cfg(not(feature="no-consumer-heartbeat"))]
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
  let full_length = shm_queue_size + queue_offset + n_slots * slot_layout.size;

  println!("========== SHARED MEMORY DUMP ==========");
  println!("base_ptr            = {:p}", queue);
  println!("total_size          = {} bytes", full_length);
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

  let bytes = unsafe {
    std::slice::from_raw_parts(
      queue as *const ShmQueue<MAX_CONSUMERS> as *const u8,
      full_length,
    )
  };
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
