//! Dolphin process memory access for rollback save states.
//!
//! Reads/writes the GameCube emulated RAM from Dolphin's process memory.
//! This enables save state snapshots without modifying Dolphin itself.

use std::sync::{Arc, Mutex};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
#[cfg(windows)]
use windows_sys::Win32::System::Memory::{
    VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_READWRITE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
    PROCESS_VM_OPERATION,
};

/// GC RAM is 24MB active (of 32MB allocated).
/// For rollback, we save the full 32MB to be safe.
const GC_RAM_SIZE: usize = 32 * 1024 * 1024; // 32 MB

/// A snapshot of Dolphin's emulated memory for one frame.
#[derive(Clone)]
pub struct MemorySnapshot {
    pub frame: u32,
    pub data: Vec<u8>,
    pub ram_base: usize,
}

impl MemorySnapshot {
    pub fn new(frame: u32, size: usize) -> Self {
        Self {
            frame,
            data: vec![0u8; size],
            ram_base: 0,
        }
    }
}

/// Ring buffer of memory snapshots for rollback.
pub struct SaveStateRing {
    pub states: Vec<Option<MemorySnapshot>>,
    pub capacity: usize,
    pub write_idx: usize,
}

impl SaveStateRing {
    pub fn new(capacity: usize) -> Self {
        let mut states = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            states.push(None);
        }
        Self {
            states,
            capacity,
            write_idx: 0,
        }
    }

    /// Save a snapshot, overwriting the oldest if full.
    pub fn push(&mut self, snapshot: MemorySnapshot) {
        self.states[self.write_idx] = Some(snapshot);
        self.write_idx = (self.write_idx + 1) % self.capacity;
    }

    /// Find the snapshot for a specific frame.
    pub fn get(&self, frame: u32) -> Option<&MemorySnapshot> {
        self.states.iter().flatten().find(|s| s.frame == frame)
    }

    /// Find the latest snapshot at or before the given frame.
    pub fn get_latest_before(&self, frame: u32) -> Option<&MemorySnapshot> {
        self.states
            .iter()
            .flatten()
            .filter(|s| s.frame <= frame)
            .max_by_key(|s| s.frame)
    }
}

/// Handle to Dolphin's process for memory operations.
#[cfg(windows)]
pub struct DolphinMemory {
    process_handle: HANDLE,
    /// Base address of GC emulated RAM in Dolphin's address space.
    ram_base: usize,
    /// Size of the memory region we're tracking.
    ram_size: usize,
    /// PID of the attached Dolphin process.
    pub pid: u32,
}

#[cfg(windows)]
impl DolphinMemory {
    /// Attach to a running Dolphin process by PID.
    pub fn attach(pid: u32) -> Result<Self, String> {
        let handle = unsafe {
            OpenProcess(
                PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_QUERY_INFORMATION,
                0,
                pid,
            )
        };

        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(format!(
                "Failed to open Dolphin process (PID {}). Try running as administrator.",
                pid
            ));
        }

        let mut mem = Self {
            process_handle: handle,
            ram_base: 0,
            ram_size: GC_RAM_SIZE,
            pid,
        };

        // Find the GC RAM region
        mem.find_gc_ram()?;

        Ok(mem)
    }

    /// Scan Dolphin's memory to find the GameCube RAM region.
    /// Dolphin allocates a large block (usually 32MB) for GC RAM.
    /// We look for a committed, read/write region of exactly 32MB.
    fn find_gc_ram(&mut self) -> Result<(), String> {
        let mut addr: usize = 0;
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let info_size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();

        // Collect ALL candidate 32MB R/W regions
        let mut candidates: Vec<usize> = Vec::new();

        loop {
            let result = unsafe {
                VirtualQueryEx(
                    self.process_handle,
                    addr as *const _,
                    &mut info,
                    info_size,
                )
            };

            if result == 0 {
                break;
            }

            // Exact 32MB committed R/W regions
            if info.RegionSize == GC_RAM_SIZE
                && info.State == MEM_COMMIT
                && info.Protect == PAGE_READWRITE
            {
                candidates.push(info.BaseAddress as usize);
            }

            addr = info.BaseAddress as usize + info.RegionSize;
            if addr == 0 {
                break;
            }
        }

        // Also check larger regions (>= 32MB)
        addr = 0;
        loop {
            let result = unsafe {
                VirtualQueryEx(
                    self.process_handle,
                    addr as *const _,
                    &mut info,
                    info_size,
                )
            };

            if result == 0 {
                break;
            }

            if info.RegionSize >= GC_RAM_SIZE
                && info.RegionSize != GC_RAM_SIZE // Don't re-add exact matches
                && info.State == MEM_COMMIT
                && (info.Protect == PAGE_READWRITE || info.Protect == 0x40)
            {
                candidates.push(info.BaseAddress as usize);
            }

            addr = info.BaseAddress as usize + info.RegionSize;
            if addr == 0 {
                break;
            }
        }

        // Validate each candidate by reading known GC values.
        // GC RAM offset 0x0 usually contains the game ID (e.g., "G4NJ" for GNT4).
        // Also check OS timebase at 0x000000F8 which should be non-zero when a game is running.
        for base in &candidates {
            // Try reading bytes at offset 0 (game ID area)
            let mut id_buf = [0u8; 4];
            let mut bytes_read: usize = 0;
            let ok = unsafe {
                ReadProcessMemory(
                    self.process_handle,
                    *base as *const _,
                    id_buf.as_mut_ptr() as *mut _,
                    4,
                    &mut bytes_read,
                )
            };
            if ok == 0 || bytes_read != 4 {
                continue;
            }

            // Check for known GNT4 game ID "G4NJ" or any valid GC game ID pattern
            // Also check if the OS timebase (offset 0xF8) is non-zero
            let mut tb_buf = [0u8; 4];
            let mut tb_read: usize = 0;
            let _ = unsafe {
                ReadProcessMemory(
                    self.process_handle,
                    (*base + 0xF8) as *const _,
                    tb_buf.as_mut_ptr() as *mut _,
                    4,
                    &mut tb_read,
                )
            };
            let timebase = u32::from_be_bytes(tb_buf);

            // Check Gecko unlock address (offset 0x002232E8 from GC base)
            // If this reads 0x0001FF03, we definitely found GC RAM with our codes applied
            let mut gecko_buf = [0u8; 4];
            let mut gecko_read: usize = 0;
            let _ = unsafe {
                ReadProcessMemory(
                    self.process_handle,
                    (*base + 0x002232E8) as *const _,
                    gecko_buf.as_mut_ptr() as *mut _,
                    4,
                    &mut gecko_read,
                )
            };
            let gecko_val = u32::from_be_bytes(gecko_buf);

            // Check player pointer area (offset 0x00226358)
            let mut pp_buf = [0u8; 4];
            let mut pp_read: usize = 0;
            let _ = unsafe {
                ReadProcessMemory(
                    self.process_handle,
                    (*base + 0x00226358) as *const _,
                    pp_buf.as_mut_ptr() as *mut _,
                    4,
                    &mut pp_read,
                )
            };
            let p1_ptr = u32::from_be_bytes(pp_buf);

            // Score this candidate
            let has_gecko = gecko_val == 0x0001FF03;
            let has_timebase = timebase > 1000;
            let has_player = p1_ptr >= 0x80000000 && p1_ptr < 0x81800000;

            // Best: has gecko codes AND (timebase or player pointer)
            if has_gecko && (has_timebase || has_player) {
                self.ram_base = *base;
                return Ok(());
            }
            // Good: has timebase (game is running)
            if has_timebase && !has_gecko {
                // Keep looking, prefer one with gecko
                continue;
            }
        }

        // Second pass: accept any candidate with timebase
        for base in &candidates {
            let mut tb_buf = [0u8; 4];
            let mut tb_read: usize = 0;
            let _ = unsafe {
                ReadProcessMemory(
                    self.process_handle,
                    (*base + 0xF8) as *const _,
                    tb_buf.as_mut_ptr() as *mut _,
                    4,
                    &mut tb_read,
                )
            };
            let timebase = u32::from_be_bytes(tb_buf);
            if timebase > 1000 {
                self.ram_base = *base;
                return Ok(());
            }
        }

        // Last resort: just use first candidate
        if let Some(&base) = candidates.first() {
            self.ram_base = base;
            return Ok(());
        }

        Err("Could not find GameCube RAM in Dolphin's memory. Is a game running?".to_string())
    }

    /// Read a block of memory from Dolphin's emulated GC RAM.
    /// `offset` is relative to the GC RAM base (0x80000000 in GC address space).
    pub fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, String> {
        if self.ram_base == 0 {
            return Err("Not attached to Dolphin RAM".to_string());
        }

        let addr = self.ram_base + offset;
        let mut bytes_read: usize = 0;

        let ok = unsafe {
            ReadProcessMemory(
                self.process_handle,
                addr as *const _,
                buf.as_mut_ptr() as *mut _,
                buf.len(),
                &mut bytes_read,
            )
        };

        if ok == 0 {
            Err(format!(
                "ReadProcessMemory failed at offset 0x{:X}",
                offset
            ))
        } else {
            Ok(bytes_read)
        }
    }

    /// Write a block of memory to Dolphin's emulated GC RAM.
    pub fn write(&self, offset: usize, data: &[u8]) -> Result<usize, String> {
        if self.ram_base == 0 {
            return Err("Not attached to Dolphin RAM".to_string());
        }

        let addr = self.ram_base + offset;
        let mut bytes_written: usize = 0;

        let ok = unsafe {
            WriteProcessMemory(
                self.process_handle,
                addr as *mut _,
                data.as_ptr() as *const _,
                data.len(),
                &mut bytes_written,
            )
        };

        if ok == 0 {
            Err(format!(
                "WriteProcessMemory failed at offset 0x{:X}",
                offset
            ))
        } else {
            Ok(bytes_written)
        }
    }

    /// Read a u32 from GC RAM at the given GC address (e.g., 0x80226358).
    pub fn read_u32(&self, gc_addr: u32) -> Result<u32, String> {
        let offset = (gc_addr & 0x01FFFFFF) as usize; // Strip 0x80/0x81 prefix
        let mut buf = [0u8; 4];
        self.read(offset, &mut buf)?;
        // GC is big-endian
        Ok(u32::from_be_bytes(buf))
    }

    /// Read a u16 from GC RAM.
    pub fn read_u16(&self, gc_addr: u32) -> Result<u16, String> {
        let offset = (gc_addr & 0x01FFFFFF) as usize;
        let mut buf = [0u8; 2];
        self.read(offset, &mut buf)?;
        Ok(u16::from_be_bytes(buf))
    }

    /// Read a f32 from GC RAM.
    pub fn read_f32(&self, gc_addr: u32) -> Result<f32, String> {
        let offset = (gc_addr & 0x01FFFFFF) as usize;
        let mut buf = [0u8; 4];
        self.read(offset, &mut buf)?;
        Ok(f32::from_be_bytes(buf))
    }

    /// Write a u32 to GC RAM.
    pub fn write_u32(&self, gc_addr: u32, value: u32) -> Result<(), String> {
        let offset = (gc_addr & 0x01FFFFFF) as usize;
        self.write(offset, &value.to_be_bytes())?;
        Ok(())
    }

    /// Save a full snapshot of GC RAM.
    pub fn save_state(&self, frame: u32) -> Result<MemorySnapshot, String> {
        let mut snapshot = MemorySnapshot::new(frame, self.ram_size);
        snapshot.ram_base = self.ram_base;
        self.read(0, &mut snapshot.data)?;
        Ok(snapshot)
    }

    /// Restore a snapshot back to Dolphin's memory.
    pub fn load_state(&self, snapshot: &MemorySnapshot) -> Result<(), String> {
        self.write(0, &snapshot.data)?;
        Ok(())
    }

    /// Read GNT4 player state for sync verification.
    /// NOTE: 0x80226358 etc. are POINTER addresses. We must dereference them
    /// to get the actual player struct base, then apply offsets.
    pub fn read_player_state(&self, player: u8) -> Result<PlayerState, String> {
        let ptr_addr: u32 = match player {
            0 => 0x80226358, // P1 pointer
            1 => 0x80226614, // P2 pointer
            2 => 0x802268D0, // P3 pointer
            3 => 0x80226B8C, // P4 pointer
            _ => return Err("Invalid player index".to_string()),
        };

        // Dereference: read the pointer value, then read fields at ptr + offset
        let base_addr = self.read_u32(ptr_addr)?;

        // Validate pointer is in GC RAM range
        if base_addr < 0x80000000 || base_addr >= 0x81800000 {
            return Err(format!(
                "P{} pointer at {:#010X} = {:#010X} (not in GC RAM — not in battle?)",
                player + 1, ptr_addr, base_addr
            ));
        }

        Ok(PlayerState {
            health: self.read_u16(base_addr + 0x262)?,  // damage accumulator (0 = full HP)
            chakra: self.read_u16(base_addr + 0x28E)?,   // 0x3C00 = full
            vertical_speed: self.read_f32(base_addr + 0x1DC)?,
            gravity: self.read_f32(base_addr + 0x1E0)?,
            block_meter: self.read_u16(base_addr + 0x294)?,
        })
    }

    /// Dereference a player pointer and return the actual struct base address.
    /// Returns None if the pointer is invalid (not in battle).
    pub fn resolve_player_ptr(&self, player: u8) -> Result<Option<u32>, String> {
        let ptr_addr: u32 = match player {
            0 => 0x80226358,
            1 => 0x80226614,
            2 => 0x802268D0,
            3 => 0x80226B8C,
            _ => return Err("Invalid player index".to_string()),
        };
        let ptr = self.read_u32(ptr_addr)?;
        if ptr >= 0x80000000 && ptr < 0x81800000 {
            Ok(Some(ptr))
        } else {
            Ok(None)
        }
    }

    /// Read the GC OS timebase (raw bus clock ticks).
    /// 0x800000F8 is the lower 32 bits of the GC timebase.
    pub fn read_timebase_ticks(&self) -> Result<u32, String> {
        self.read_u32(0x800000F8)
    }

    /// Read frame counter — returns the raw timebase ticks.
    /// The rollback loop uses tick deltas to detect frame advancement.
    pub fn read_frame_counter(&self) -> Result<u32, String> {
        self.read_u32(0x800000F8)
    }
}

// SAFETY: DolphinMemory is only accessed behind a Mutex, and Windows HANDLE
// values are safe to send between threads (they're process-wide).
#[cfg(windows)]
unsafe impl Send for DolphinMemory {}
#[cfg(windows)]
unsafe impl Sync for DolphinMemory {}

#[cfg(windows)]
impl Drop for DolphinMemory {
    fn drop(&mut self) {
        if !self.process_handle.is_null() && self.process_handle != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.process_handle);
            }
        }
    }
}

/// Player state snapshot for sync verification.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlayerState {
    pub health: u16,
    pub chakra: u16,
    pub vertical_speed: f32,
    pub gravity: f32,
    pub block_meter: u16,
}

// ── GC Controller Input ──

/// GC controller button bitmask (matches PADStatus.button).
pub mod gc_buttons {
    pub const DPAD_LEFT: u16  = 0x0001;
    pub const DPAD_RIGHT: u16 = 0x0002;
    pub const DPAD_DOWN: u16  = 0x0004;
    pub const DPAD_UP: u16    = 0x0008;
    pub const Z: u16          = 0x0010;
    pub const R: u16          = 0x0020;
    pub const L: u16          = 0x0040;
    pub const A: u16          = 0x0100;
    pub const B: u16          = 0x0200;
    pub const X: u16          = 0x0400;
    pub const Y: u16          = 0x0800;
    pub const START: u16      = 0x1000;
}

/// GC PADStatus struct layout (12 bytes per controller).
/// This is what PADRead() fills in GC games.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct GCPadStatus {
    pub buttons: u16,     // +0x00 button bitmask
    pub stick_x: i8,      // +0x02 main stick X (-128..127)
    pub stick_y: i8,      // +0x03 main stick Y
    pub cstick_x: i8,     // +0x04 C-stick X
    pub cstick_y: i8,     // +0x05 C-stick Y
    pub trigger_l: u8,    // +0x06 L analog (0-255)
    pub trigger_r: u8,    // +0x07 R analog (0-255)
}

impl Default for GCPadStatus {
    fn default() -> Self {
        Self {
            buttons: 0,
            stick_x: 0,
            stick_y: 0,
            cstick_x: 0,
            cstick_y: 0,
            trigger_l: 0,
            trigger_r: 0,
        }
    }
}

/// Known GNT4 memory addresses for controller input.
/// These are where the game stores its per-frame input state.
///
/// NOTE: These offsets are relative to each player's struct base.
/// They need to be verified via Dolphin's debugger on first test.
/// NicholasMoser's GNT4 research + community RE gives us these starting points.
pub mod gnt4_input_addrs {
    /// Player struct bases (same as in read_player_state)
    pub const P1_BASE: u32 = 0x80226358;
    pub const P2_BASE: u32 = 0x80226614;

    /// Offset from player base to processed button input (u16, big-endian)
    /// Verified via fast input scan: +0x252 changes with button presses.
    /// Old offset +0x0C was always zero — wrong for GNT4.
    pub const BUTTONS_OFFSET: u32 = 0x252;
    /// Offset to action state / current move ID (u16)
    pub const ACTION_STATE_OFFSET: u32 = 0x12C;
    /// Offset to action flags / animation state (u16)
    pub const ACTION_FLAGS_OFFSET: u32 = 0x12A;
    /// Offset to grounded/airborne flag (u16, 0x8000 = airborne)
    pub const AIRBORNE_OFFSET: u32 = 0x128;
    /// Offset to animation frame counter (u16, increments each frame)
    pub const ANIM_FRAME_OFFSET: u32 = 0x25A;
    /// Offset to main stick X (i8) — still needs verification
    pub const STICK_X_OFFSET: u32 = 0x10;
    /// Offset to main stick Y (i8) — still needs verification
    pub const STICK_Y_OFFSET: u32 = 0x11;
    /// Offset to C-stick X (i8) — still needs verification
    pub const CSTICK_X_OFFSET: u32 = 0x12;
    /// Offset to C-stick Y (i8) — still needs verification
    pub const CSTICK_Y_OFFSET: u32 = 0x13;
    /// Offset to L trigger analog (u8) — still needs verification
    pub const TRIGGER_L_OFFSET: u32 = 0x14;
    /// Offset to R trigger analog (u8) — still needs verification
    pub const TRIGGER_R_OFFSET: u32 = 0x15;

    /// Alternative: The PAD polling buffer in GNT4's BSS section.
    /// PADRead() fills 4 PADStatus structs here (12 bytes each).
    /// This is a more universal injection point.
    pub const PAD_BUFFER_BASE: u32 = 0x802233A0; // Needs verification
    pub const PAD_STATUS_SIZE: u32 = 12;
}

#[cfg(windows)]
impl DolphinMemory {
    /// Read the current controller input for a player from GNT4 memory.
    /// Dereferences the player pointer first to find the actual struct.
    pub fn read_player_input(&self, player: u8) -> Result<GCPadStatus, String> {
        use gnt4_input_addrs::*;
        let base = self.resolve_player_ptr(player)?
            .ok_or_else(|| format!("P{} pointer not valid (not in battle?)", player + 1))?;

        Ok(GCPadStatus {
            buttons: self.read_u16(base + BUTTONS_OFFSET)?,
            stick_x: self.read_u8(base + STICK_X_OFFSET)? as i8,
            stick_y: self.read_u8(base + STICK_Y_OFFSET)? as i8,
            cstick_x: self.read_u8(base + CSTICK_X_OFFSET)? as i8,
            cstick_y: self.read_u8(base + CSTICK_Y_OFFSET)? as i8,
            trigger_l: self.read_u8(base + TRIGGER_L_OFFSET)?,
            trigger_r: self.read_u8(base + TRIGGER_R_OFFSET)?,
        })
        // Note: stick/trigger offsets (+0x10-0x15) still need verification.
        // Only +0x252 (buttons) is confirmed via fast input scan.
    }

    /// Write controller input for a player into GNT4 memory.
    /// This is the core of input injection — writes remote player's input.
    /// Dereferences the player pointer first.
    pub fn write_player_input(&self, player: u8, input: &GCPadStatus) -> Result<(), String> {
        use gnt4_input_addrs::*;
        let base = self.resolve_player_ptr(player)?
            .ok_or_else(|| format!("P{} pointer not valid (not in battle?)", player + 1))?;

        // Write button state
        self.write_u16(base + BUTTONS_OFFSET, input.buttons)?;

        // Write sticks and triggers
        self.write_u8(base + STICK_X_OFFSET, input.stick_x as u8)?;
        self.write_u8(base + STICK_Y_OFFSET, input.stick_y as u8)?;
        self.write_u8(base + CSTICK_X_OFFSET, input.cstick_x as u8)?;
        self.write_u8(base + CSTICK_Y_OFFSET, input.cstick_y as u8)?;
        self.write_u8(base + TRIGGER_L_OFFSET, input.trigger_l)?;
        self.write_u8(base + TRIGGER_R_OFFSET, input.trigger_r)?;

        Ok(())
    }

    /// Alternative injection: write directly to the PAD polling buffer.
    /// More universal — works regardless of game-specific struct layout.
    pub fn write_pad_buffer(&self, port: u8, input: &GCPadStatus) -> Result<(), String> {
        use gnt4_input_addrs::*;
        if port > 3 {
            return Err("Invalid controller port (0-3)".to_string());
        }

        let addr = PAD_BUFFER_BASE + (port as u32 * PAD_STATUS_SIZE);

        // PADStatus struct: buttons(u16) stickX(i8) stickY(i8) cstickX(i8) cstickY(i8) trigL(u8) trigR(u8)
        let mut buf = [0u8; 8];
        let buttons_be = input.buttons.to_be_bytes();
        buf[0] = buttons_be[0];
        buf[1] = buttons_be[1];
        buf[2] = input.stick_x as u8;
        buf[3] = input.stick_y as u8;
        buf[4] = input.cstick_x as u8;
        buf[5] = input.cstick_y as u8;
        buf[6] = input.trigger_l;
        buf[7] = input.trigger_r;

        let offset = (addr & 0x01FFFFFF) as usize;
        self.write(offset, &buf)?;
        Ok(())
    }

    /// Read a single byte from GC RAM.
    pub fn read_u8(&self, gc_addr: u32) -> Result<u8, String> {
        let offset = (gc_addr & 0x01FFFFFF) as usize;
        let mut buf = [0u8; 1];
        self.read(offset, &mut buf)?;
        Ok(buf[0])
    }

    /// Write a u16 to GC RAM (big-endian).
    pub fn write_u16(&self, gc_addr: u32, value: u16) -> Result<(), String> {
        let offset = (gc_addr & 0x01FFFFFF) as usize;
        self.write(offset, &value.to_be_bytes())?;
        Ok(())
    }

    /// Write a single byte to GC RAM.
    pub fn write_u8(&self, gc_addr: u32, value: u8) -> Result<(), String> {
        let offset = (gc_addr & 0x01FFFFFF) as usize;
        self.write(offset, &[value])?;
        Ok(())
    }
}

/// Shared state for Dolphin memory access.
pub struct DolphinMemState {
    #[cfg(windows)]
    pub memory: Option<DolphinMemory>,
    pub save_ring: SaveStateRing,
}

impl DolphinMemState {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            memory: None,
            save_ring: SaveStateRing::new(10), // 10 frames of rollback history
        }
    }
}

// ── Tauri Commands ──

/// Attach to Dolphin's process memory. Call after launching Dolphin.
#[tauri::command]
pub fn dolphin_mem_attach(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        // Find Dolphin PID
        let pid = find_dolphin_pid()?;

        let mut ms = state.lock().map_err(|e| e.to_string())?;
        let mem = DolphinMemory::attach(pid)?;
        let base = mem.ram_base;
        ms.memory = Some(mem);

        Ok(format!(
            "Attached to Dolphin (PID {}) — GC RAM at 0x{:X}",
            pid, base
        ))
    }

    #[cfg(not(windows))]
    Err("Dolphin memory access only supported on Windows".to_string())
}

/// Read player state for debugging/verification.
#[tauri::command]
pub fn dolphin_mem_read_player(
    player: u8,
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<PlayerState, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;
        mem.read_player_state(player)
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

/// Read the current frame counter from GNT4.
#[tauri::command]
pub fn dolphin_mem_read_frame(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<u32, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;
        mem.read_frame_counter()
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

/// Save current game state to the rollback ring buffer.
#[tauri::command]
pub fn dolphin_mem_save_state(
    frame: u32,
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        let mut ms = state.lock().map_err(|e| e.to_string())?;
        let snapshot = {
            let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;
            let start = std::time::Instant::now();
            let snap = mem.save_state(frame)?;
            let elapsed = start.elapsed();
            (snap, elapsed)
        };
        let elapsed = snapshot.1;
        ms.save_ring.push(snapshot.0);
        Ok(format!(
            "Saved state for frame {} in {:.2}ms ({:.1} MB)",
            frame,
            elapsed.as_secs_f64() * 1000.0,
            GC_RAM_SIZE as f64 / 1024.0 / 1024.0
        ))
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

/// Load a saved state from the ring buffer (for rollback).
#[tauri::command]
pub fn dolphin_mem_load_state(
    frame: u32,
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let snapshot = ms
            .save_ring
            .get(frame)
            .ok_or(format!("No saved state for frame {}", frame))?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;
        let start = std::time::Instant::now();
        mem.load_state(snapshot)?;
        let elapsed = start.elapsed();
        Ok(format!(
            "Loaded state for frame {} in {:.2}ms",
            frame,
            elapsed.as_secs_f64() * 1000.0
        ))
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

/// Check if a match has ended by reading player health.
/// Returns: "p1_win", "p2_win", "draw", or "playing".
/// NOTE: GNT4 health is a DAMAGE ACCUMULATOR — 0 = full HP, increases as damage taken.
/// A player is KO'd when health >= their character's max HP (varies per character, ~180-220).
/// We use a threshold of 150 as a conservative "likely KO" indicator.
#[tauri::command]
pub fn dolphin_mem_check_winner(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<MatchOutcome, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        let p1 = mem.read_player_state(0)?;
        let p2 = mem.read_player_state(1)?;
        let frame = mem.read_frame_counter().unwrap_or(0);

        // Health is damage accumulated. Higher = more damaged.
        // Character max HP varies (~180-220). Use 150 as KO threshold for now.
        // TODO: Read per-character max HP from game data
        let ko_threshold: u16 = 150;
        let p1_ko = p1.health >= ko_threshold;
        let p2_ko = p2.health >= ko_threshold;

        let outcome = if p1_ko && p2_ko {
            MatchOutcome {
                result: "draw".to_string(),
                p1_health: p1.health,
                p2_health: p2.health,
                frame,
            }
        } else if p1_ko {
            MatchOutcome {
                result: "p2_win".to_string(),
                p1_health: p1.health,
                p2_health: p2.health,
                frame,
            }
        } else if p2_ko {
            MatchOutcome {
                result: "p1_win".to_string(),
                p1_health: p1.health,
                p2_health: p2.health,
                frame,
            }
        } else {
            MatchOutcome {
                result: "playing".to_string(),
                p1_health: p1.health,
                p2_health: p2.health,
                frame,
            }
        };

        Ok(outcome)
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MatchOutcome {
    pub result: String,  // "p1_win", "p2_win", "draw", "playing"
    pub p1_health: u16,
    pub p2_health: u16,
    pub frame: u32,
}

/// Read controller input for a player.
#[tauri::command]
pub fn dolphin_mem_read_input(
    player: u8,
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<GCPadStatus, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;
        mem.read_player_input(player)
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

/// Write controller input for a player (input injection).
#[tauri::command]
pub fn dolphin_mem_write_input(
    player: u8,
    buttons: u16,
    stick_x: i8,
    stick_y: i8,
    cstick_x: i8,
    cstick_y: i8,
    trigger_l: u8,
    trigger_r: u8,
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;
        let input = GCPadStatus {
            buttons,
            stick_x,
            stick_y,
            cstick_x,
            cstick_y,
            trigger_l,
            trigger_r,
        };
        mem.write_player_input(player, &input)
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

/// Detach from Dolphin.
#[tauri::command]
pub fn dolphin_mem_detach(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let mut ms = state.lock().map_err(|e| e.to_string())?;
        ms.memory = None;
    }
    Ok(())
}

// ── Local Test Mode ──

/// Test rollback locally: reads P1 input, saves/loads states, measures timing.
/// This lets you test the full rollback pipeline with just one Dolphin instance.
#[tauri::command]
pub fn dolphin_test_rollback(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<LocalTestResult, String> {
    #[cfg(windows)]
    {
        let mut ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        // 1. Read current game state
        let frame = mem.read_frame_counter().unwrap_or(0);
        let p1_input = mem.read_player_input(0)?;
        let p1_state = mem.read_player_state(0)?;
        let p2_state = mem.read_player_state(1)?;

        // 2. Time a save state
        let save_start = std::time::Instant::now();
        let snapshot = mem.save_state(frame)?;
        let save_ms = save_start.elapsed().as_secs_f64() * 1000.0;

        // 3. Load state DISABLED — crashes Dolphin mid-emulation
        let load_ms = 0.0;

        // 4. Test input injection: write P1's input to P2 (mirror test)
        let inject_start = std::time::Instant::now();
        mem.write_player_input(1, &p1_input)?;
        let inject_ms = inject_start.elapsed().as_secs_f64() * 1000.0;

        // 5. Save to ring buffer for subsequent tests
        ms.save_ring.push(snapshot);

        Ok(LocalTestResult {
            frame,
            save_state_ms: save_ms,
            load_state_ms: load_ms,
            input_inject_ms: inject_ms,
            snapshot_size_mb: GC_RAM_SIZE as f64 / 1024.0 / 1024.0,
            p1_buttons: p1_input.buttons,
            p1_stick_x: p1_input.stick_x,
            p1_stick_y: p1_input.stick_y,
            p1_health: p1_state.health,
            p2_health: p2_state.health,
            ring_buffer_count: ms.save_ring.states.iter().filter(|s| s.is_some()).count() as u32,
        })
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LocalTestResult {
    pub frame: u32,
    pub save_state_ms: f64,
    pub load_state_ms: f64,
    pub input_inject_ms: f64,
    pub snapshot_size_mb: f64,
    pub p1_buttons: u16,
    pub p1_stick_x: i8,
    pub p1_stick_y: i8,
    pub p1_health: u16,
    pub p2_health: u16,
    pub ring_buffer_count: u32,
}

/// Continuous rollback stress test — saves state, waits N frames, loads it back.
/// Simulates what happens during a rollback: rewind → replay.
#[tauri::command]
pub fn dolphin_test_rewind(
    frames_back: u32,
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<RewindTestResult, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        let current_frame = mem.read_frame_counter().unwrap_or(0);

        // Try to find a saved state from `frames_back` ago
        let target_frame = current_frame.saturating_sub(frames_back);
        let snapshot = ms.save_ring.get_latest_before(target_frame + 1);

        match snapshot {
            Some(snap) => {
                let actual_depth = current_frame - snap.frame;
                let load_start = std::time::Instant::now();
                mem.load_state(snap)?;
                let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;

                // Read state after rewind to verify it worked
                let rewound_frame = mem.read_frame_counter().unwrap_or(0);
                let p1 = mem.read_player_state(0)?;
                let p2 = mem.read_player_state(1)?;

                Ok(RewindTestResult {
                    success: true,
                    requested_depth: frames_back,
                    actual_depth,
                    original_frame: current_frame,
                    rewound_to_frame: rewound_frame,
                    load_ms,
                    p1_health_after: p1.health,
                    p2_health_after: p2.health,
                    error: None,
                })
            }
            None => Ok(RewindTestResult {
                success: false,
                requested_depth: frames_back,
                actual_depth: 0,
                original_frame: current_frame,
                rewound_to_frame: 0,
                load_ms: 0.0,
                p1_health_after: 0,
                p2_health_after: 0,
                error: Some(format!(
                    "No save state found for frame {} (ring has {} states)",
                    target_frame,
                    ms.save_ring.states.iter().filter(|s| s.is_some()).count()
                )),
            }),
        }
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RewindTestResult {
    pub success: bool,
    pub requested_depth: u32,
    pub actual_depth: u32,
    pub original_frame: u32,
    pub rewound_to_frame: u32,
    pub load_ms: f64,
    pub p1_health_after: u16,
    pub p2_health_after: u16,
    pub error: Option<String>,
}

/// Simple save speed benchmark for the frontend.
/// NOTE: Load is disabled — writing 32MB back into Dolphin mid-emulation causes
/// "Invalid read from 0x00000014" crashes. Real rollback will need Dolphin's
/// native save state API or pausing emulation before load.
#[tauri::command]
pub fn test_save_load_speed(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<SaveLoadSpeedResult, String> {
    #[cfg(windows)]
    {
        let mut ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        let frame = mem.read_frame_counter().unwrap_or(0);

        // Save (safe — just reading memory)
        let save_start = std::time::Instant::now();
        let snapshot = mem.save_state(frame)?;
        let save_ms = save_start.elapsed().as_secs_f64() * 1000.0;

        // Store in ring buffer for potential future use
        let size = snapshot.data.len() as u64;
        ms.save_ring.push(snapshot);

        Ok(SaveLoadSpeedResult {
            save_ms,
            load_ms: 0.0, // Load disabled to prevent Dolphin crash
            size_bytes: size,
        })
    }
    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SaveLoadSpeedResult {
    pub save_ms: f64,
    pub load_ms: f64,
    pub size_bytes: u64,
}

// ── Debug Scan ──

/// Comprehensive debug dump of all known GNT4 addresses.
/// Run this while a fight is active in Dolphin to verify all memory addresses.
#[tauri::command]
pub fn dolphin_debug_scan(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<DebugScanResult, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        let frame = mem.read_frame_counter().unwrap_or(0);

        // Read player states
        let p1 = mem.read_player_state(0).ok();
        let p2 = mem.read_player_state(1).ok();
        let p1_input = mem.read_player_input(0).ok();
        let p2_input = mem.read_player_input(1).ok();

        // Read P1 pointer and dereference it
        let p1_ptr_addr: u32 = 0x80226358;
        let p1_ptr_val = mem.read_u32(p1_ptr_addr).unwrap_or(0);
        let mut p1_raw_offsets: Vec<(String, String)> = Vec::new();

        // Show the pointer value first
        p1_raw_offsets.push((
            format!("P1_PTR (0x{:08X})", p1_ptr_addr),
            format!("0x{:08X} {}", p1_ptr_val,
                if p1_ptr_val >= 0x80000000 && p1_ptr_val < 0x81800000 { "(VALID)" } else { "(INVALID - not in battle?)" }),
        ));

        // If pointer is valid, read offsets from the dereferenced address
        let p1_base = if p1_ptr_val >= 0x80000000 && p1_ptr_val < 0x81800000 {
            p1_ptr_val
        } else {
            0 // Will show DEADBEEF for all reads
        };

        // Known offsets to verify (applied to DEREFERENCED pointer)
        let check_offsets: &[(u32, &str)] = &[
            (0x0004, "chr_id_or_ptr"),
            (0x000C, "buttons_held"),
            (0x000E, "buttons_pressed"),
            (0x0010, "stick_x"),
            (0x0011, "stick_y"),
            (0x0012, "cstick_x"),
            (0x0013, "cstick_y"),
            (0x0014, "trigger_l"),
            (0x0015, "trigger_r"),
            (0x0024, "input_raw"),
            (0x0130, "position_ptr"),
            (0x01DC, "vertical_speed"),
            (0x01E0, "gravity"),
            (0x0262, "health_dmg"),
            (0x0264, "health_max_maybe"),
            (0x028E, "chakra"),
            (0x0290, "chakra_max_maybe"),
            (0x0294, "block_meter"),
            (0x0296, "block_meter_max_maybe"),
            (0x02A0, "combat_state"),
            (0x02BE, "damage_modifier"),
            (0x02F8, "throw_state"),
        ];

        for (offset, name) in check_offsets {
            if p1_base == 0 {
                p1_raw_offsets.push((
                    format!("{} (+0x{:04X})", name, offset),
                    "N/A (pointer invalid)".to_string(),
                ));
            } else {
                let addr = p1_base + offset;
                let val = mem.read_u32(addr).unwrap_or(0xDEADBEEF);
                p1_raw_offsets.push((
                    format!("{} (+0x{:04X} = 0x{:08X})", name, offset, addr),
                    format!("0x{:08X} ({})", val, val),
                ));
            }
        }

        // Read some global addresses
        let fight_mode = mem.read_u32(0x802233A8).unwrap_or(0);
        let scene_id = mem.read_u32(0x80222FB8).unwrap_or(0);

        Ok(DebugScanResult {
            frame_counter: frame,
            p1_state: p1.map(|s| format!("HP:{} CK:{} VS:{:.2} GR:{:.2} BLK:{}",
                s.health, s.chakra, s.vertical_speed, s.gravity, s.block_meter)),
            p2_state: p2.map(|s| format!("HP:{} CK:{} VS:{:.2} GR:{:.2} BLK:{}",
                s.health, s.chakra, s.vertical_speed, s.gravity, s.block_meter)),
            p1_input: p1_input.map(|i| format!("BTN:0x{:04X} SX:{} SY:{} CX:{} CY:{} L:{} R:{}",
                i.buttons, i.stick_x, i.stick_y, i.cstick_x, i.cstick_y, i.trigger_l, i.trigger_r)),
            p2_input: p2_input.map(|i| format!("BTN:0x{:04X} SX:{} SY:{} CX:{} CY:{} L:{} R:{}",
                i.buttons, i.stick_x, i.stick_y, i.cstick_x, i.cstick_y, i.trigger_l, i.trigger_r)),
            p1_raw: p1_raw_offsets,
            fight_mode: format!("0x{:08X}", fight_mode),
            scene_ptr: format!("0x{:08X}", scene_id),
        })
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DebugScanResult {
    pub frame_counter: u32,
    pub p1_state: Option<String>,
    pub p2_state: Option<String>,
    pub p1_input: Option<String>,
    pub p2_input: Option<String>,
    pub p1_raw: Vec<(String, String)>,
    pub fight_mode: String,
    pub scene_ptr: String,
}

// ── Memory Scanner ──

/// Scan GC RAM for a u16 value (big-endian) and return all offsets where it's found.
/// Used to discover correct player struct addresses by searching for known values like health.
#[tauri::command]
pub fn dolphin_mem_scan_u16(
    value: u16,
    start_gc: u32,
    end_gc: u32,
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<Vec<String>, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        let start_off = (start_gc & 0x01FFFFFF) as usize;
        let end_off = (end_gc & 0x01FFFFFF) as usize;
        let search_bytes = value.to_be_bytes();

        // Read the entire range at once for speed
        let len = end_off - start_off;
        let mut buf = vec![0u8; len];
        mem.read(start_off, &mut buf)?;

        let mut results = Vec::new();
        for i in 0..len.saturating_sub(1) {
            if buf[i] == search_bytes[0] && buf[i + 1] == search_bytes[1] {
                let gc_addr = 0x80000000 + start_off as u32 + i as u32;
                results.push(format!("0x{:08X}", gc_addr));
                if results.len() >= 200 {
                    break; // Cap results
                }
            }
        }

        Ok(results)
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

/// Dump a range of memory as hex for manual inspection.
#[tauri::command]
pub fn dolphin_mem_hex_dump(
    gc_addr: u32,
    length: u32,
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<Vec<String>, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        let offset = (gc_addr & 0x01FFFFFF) as usize;
        let len = length.min(4096) as usize; // Cap at 4KB
        let mut buf = vec![0u8; len];
        mem.read(offset, &mut buf)?;

        let mut lines = Vec::new();
        for chunk_start in (0..len).step_by(16) {
            let chunk_end = (chunk_start + 16).min(len);
            let addr = gc_addr + chunk_start as u32;
            let hex: Vec<String> = buf[chunk_start..chunk_end]
                .iter()
                .map(|b| format!("{:02X}", b))
                .collect();
            let ascii: String = buf[chunk_start..chunk_end]
                .iter()
                .map(|&b| if b >= 0x20 && b < 0x7F { b as char } else { '.' })
                .collect();
            lines.push(format!("0x{:08X}: {}  {}", addr, hex.join(" "), ascii));
        }

        Ok(lines)
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

/// List all large memory regions found in Dolphin's address space.
/// Helps diagnose if we attached to the wrong region.
#[tauri::command]
pub fn dolphin_mem_list_regions(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<Vec<String>, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        let mut regions = Vec::new();
        let mut addr: usize = 0;
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let info_size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();

        loop {
            let result = unsafe {
                VirtualQueryEx(
                    mem.process_handle,
                    addr as *const _,
                    &mut info,
                    info_size,
                )
            };
            if result == 0 {
                break;
            }

            // Show regions >= 1MB
            if info.RegionSize >= 1024 * 1024 && info.State == MEM_COMMIT {
                let selected = if info.BaseAddress as usize == mem.ram_base { " <<<SELECTED" } else { "" };
                regions.push(format!(
                    "0x{:X} size={:.1}MB protect=0x{:X}{}",
                    info.BaseAddress as usize,
                    info.RegionSize as f64 / 1024.0 / 1024.0,
                    info.Protect,
                    selected
                ));
            }

            addr = info.BaseAddress as usize + info.RegionSize;
            if addr == 0 {
                break;
            }
        }

        Ok(regions)
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

/// Auto-discover player struct addresses by scanning for health-like values.
/// Scans the entire GC RAM range 0x80000000-0x81800000 for common health values,
/// then does a second scan to find values that decreased (took damage).
/// Returns a structured report of candidate addresses.
#[tauri::command]
pub fn dolphin_auto_discover(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<AutoDiscoverResult, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        // Read a large chunk of GC RAM (first 2MB of game data area 0x80200000-0x80400000)
        let scan_start: usize = 0x00200000; // 0x80200000
        let scan_len: usize = 0x200000; // 2MB
        let mut buf = vec![0u8; scan_len];
        mem.read(scan_start, &mut buf)?;

        // Count non-zero u16 values to find "hot" memory regions
        let mut region_activity: Vec<(u32, u32)> = Vec::new(); // (gc_addr, non_zero_count)
        for block_start in (0..scan_len).step_by(0x400) { // Check every 1KB block
            let block_end = (block_start + 0x400).min(scan_len);
            let mut non_zero = 0u32;
            for i in (block_start..block_end).step_by(2) {
                if i + 1 < scan_len {
                    let val = u16::from_be_bytes([buf[i], buf[i + 1]]);
                    if val != 0 {
                        non_zero += 1;
                    }
                }
            }
            if non_zero > 50 { // Block has significant data
                let gc_addr = 0x80000000 + scan_start as u32 + block_start as u32;
                region_activity.push((gc_addr, non_zero));
            }
        }
        region_activity.sort_by(|a, b| b.1.cmp(&a.1));
        region_activity.truncate(20);

        // Search for common health values
        let health_candidates: &[u16] = &[176, 48000, 480, 240, 100, 1000, 200, 300, 500, 128, 256, 512];
        let mut health_matches: Vec<(u16, Vec<String>)> = Vec::new();

        for &health_val in health_candidates {
            let search = health_val.to_be_bytes();
            let mut addrs = Vec::new();
            for i in 0..scan_len.saturating_sub(1) {
                if buf[i] == search[0] && buf[i + 1] == search[1] {
                    let gc_addr = 0x80000000 + scan_start as u32 + i as u32;
                    addrs.push(format!("0x{:08X}", gc_addr));
                    if addrs.len() >= 30 { break; }
                }
            }
            if !addrs.is_empty() {
                health_matches.push((health_val, addrs));
            }
        }

        // Look for the frame counter by reading known locations
        let frame_candidates: &[u32] = &[
            0x8024A594, 0x80222FB0, 0x80222FB4, 0x80222FB8,
            0x80223000, 0x80224000, 0x8022F000, 0x80230000,
        ];
        let mut frame_values: Vec<String> = Vec::new();
        for &addr in frame_candidates {
            let val = mem.read_u32(addr).unwrap_or(0);
            if val != 0 {
                frame_values.push(format!("0x{:08X} = {} (0x{:08X})", addr, val, val));
            }
        }

        // Check our Gecko code unlock addresses to verify we're reading the right memory
        let gecko_check_addrs: &[(u32, &str)] = &[
            (0x80223258, "unlock_flags"),
            (0x802232E8, "unlock_chars"),
            (0x802232F0, "unlock_extra"),
            (0x802232FC, "unlock_stages"),
        ];
        let mut gecko_values: Vec<String> = Vec::new();
        for &(addr, name) in gecko_check_addrs {
            let val = mem.read_u32(addr).unwrap_or(0);
            gecko_values.push(format!("{}: 0x{:08X} = {}", name, val, val));
        }

        Ok(AutoDiscoverResult {
            active_regions: region_activity.iter()
                .map(|(addr, count)| format!("0x{:08X}: {} active values", addr, count))
                .collect(),
            health_matches: health_matches.iter()
                .map(|(val, addrs)| format!("Health={}: {} matches [{}]", val, addrs.len(),
                    addrs.iter().take(5).cloned().collect::<Vec<_>>().join(", ")))
                .collect(),
            frame_candidates: frame_values,
            gecko_verification: gecko_values,
        })
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutoDiscoverResult {
    pub active_regions: Vec<String>,
    pub health_matches: Vec<String>,
    pub frame_candidates: Vec<String>,
    pub gecko_verification: Vec<String>,
}

/// Full debug scan: one-button report of everything we need.
/// Combines: pointer dereference, player states, inputs, gecko verification,
/// frame counter, active regions, and health scan.
#[tauri::command]
pub fn dolphin_full_debug(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<FullDebugReport, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        let mut lines: Vec<String> = Vec::new();

        // ── Section 1: Frame counter candidates ──
        lines.push("═══ FRAME COUNTER ═══".to_string());
        // Scan many candidate addresses — we need one that increments ~60/sec
        let frame_candidates: &[u32] = &[
            0x80222FB0, 0x80222FB4, 0x80222FB8, 0x80222FBC,
            0x80223000, 0x80223004, 0x80223008,
            0x8024A594, 0x8024A598,
            // GNT4 known timer/counter areas
            0x80226D00, 0x80226D04, 0x80226D08, 0x80226D0C,
            0x802233D8, 0x802233DC, 0x802233E0,
            // VI counter (Dolphin internal frame counter)
            0x800030F8, 0x800030FC,
            // OS tick counters
            0x800000F8, 0x800000FC,
            // GNT4 global timer candidates
            0x80222FA0, 0x80222FA4, 0x80222FA8, 0x80222FAC,
        ];
        for &addr in frame_candidates {
            let val = mem.read_u32(addr).unwrap_or(0);
            // Plausible frame counter: non-zero, not a pointer, reasonable range
            if val != 0 && val < 0x10000000 && val >= 0x80000000u32.wrapping_sub(0x80000000) {
                lines.push(format!("0x{:08X} = {} (0x{:08X})", addr, val, val));
            }
        }
        // Also read the anim frame from P1 struct as a proxy frame counter
        let p1_ptr = mem.read_u32(0x80226358).unwrap_or(0);
        if p1_ptr >= 0x80000000 && p1_ptr < 0x81800000 {
            let anim_f = mem.read_u16(p1_ptr + 0x25A).unwrap_or(0);
            lines.push(format!("P1 anim_frame (+0x25A) = {} (proxy)", anim_f));
        }

        // ── Section 2: Global game state ──
        lines.push("".to_string());
        lines.push("═══ GAME STATE ═══".to_string());
        let fight_mode = mem.read_u32(0x802233A8).unwrap_or(0);
        let scene_id = mem.read_u32(0x80222FB8).unwrap_or(0);
        lines.push(format!("Fight mode: 0x{:08X}", fight_mode));
        lines.push(format!("Scene:      0x{:08X}", scene_id));
        // Scan nearby addresses for round/win state
        lines.push("  ── GAME STATE SCAN (0x802233A0-0x802233F0) ──".to_string());
        let mut gs_line = String::new();
        for addr in (0x802233A0u32..0x802233F0).step_by(4) {
            let val = mem.read_u32(addr).unwrap_or(0);
            if val != 0 {
                gs_line.push_str(&format!("{:08X}={:08X} ", addr, val));
                if gs_line.len() > 80 {
                    lines.push(format!("  {}", gs_line.trim()));
                    gs_line.clear();
                }
            }
        }
        if !gs_line.is_empty() {
            lines.push(format!("  {}", gs_line.trim()));
        }
        // Also scan timer/round info area
        let timer = mem.read_u32(0x80226CF0).unwrap_or(0);
        let round_state = mem.read_u32(0x80226CF4).unwrap_or(0);
        let p1_rounds = mem.read_u8(0x80226CF8).unwrap_or(0);
        let p2_rounds = mem.read_u8(0x80226CF9).unwrap_or(0);
        lines.push(format!("Timer: {} RoundState: 0x{:08X} P1wins:{} P2wins:{}", timer, round_state, p1_rounds, p2_rounds));

        // ── Section 3: Player pointers + state ──
        lines.push("".to_string());
        lines.push("═══ PLAYER POINTERS ═══".to_string());
        let ptr_addrs: &[(u8, u32)] = &[
            (1, 0x80226358), (2, 0x80226614),
            (3, 0x802268D0), (4, 0x80226B8C),
        ];

        let mut any_valid = false;
        for &(pnum, ptr_addr) in ptr_addrs {
            let ptr_val = mem.read_u32(ptr_addr).unwrap_or(0);
            let valid = ptr_val >= 0x80000000 && ptr_val < 0x81800000;
            let tag = if valid { "VALID" } else { "---" };
            lines.push(format!("P{} ptr @ 0x{:08X} → 0x{:08X} [{}]", pnum, ptr_addr, ptr_val, tag));

            if valid {
                any_valid = true;
                // Read key fields from dereferenced pointer
                let hp = mem.read_u16(ptr_val + 0x262).unwrap_or(0xFFFF);
                let ck = mem.read_u16(ptr_val + 0x28E).unwrap_or(0xFFFF);
                let blk = mem.read_u16(ptr_val + 0x294).unwrap_or(0xFFFF);
                let btn = mem.read_u16(ptr_val + 0x252).unwrap_or(0xFFFF);
                let action = mem.read_u16(ptr_val + 0x12C).unwrap_or(0xFFFF);
                let airborne = mem.read_u16(ptr_val + 0x128).unwrap_or(0);
                let anim_frame = mem.read_u16(ptr_val + 0x25A).unwrap_or(0);
                let chr = mem.read_u32(ptr_val + 0x04).unwrap_or(0);
                let grav = mem.read_f32(ptr_val + 0x1E0).unwrap_or(0.0);
                let air_tag = if airborne & 0x8000 != 0 { "AIR" } else { "GND" };
                lines.push(format!("  chr=0x{:08X} hp_dmg={} ck={} blk={}", chr, hp, ck, blk));
                lines.push(format!("  btn=0x{:04X} action=0x{:04X} [{}] anim_f={} grav={:.4}", btn, action, air_tag, anim_frame, grav));

                // ── Wide offset scan for P1 only (find button inputs) ──
                if pnum == 1 {
                    lines.push("  ── OFFSET SCAN (P1 struct, every 2 bytes 0x00-0x300) ──".to_string());
                    let mut scan_line = String::new();
                    for off in (0u32..0x300).step_by(2) {
                        let val = mem.read_u16(ptr_val + off).unwrap_or(0);
                        if val != 0 {
                            scan_line.push_str(&format!("+{:03X}={:04X} ", off, val));
                            if scan_line.len() > 80 {
                                lines.push(format!("  {}", scan_line.trim()));
                                scan_line.clear();
                            }
                        }
                    }
                    if !scan_line.is_empty() {
                        lines.push(format!("  {}", scan_line.trim()));
                    }
                }
            }
        }

        if !any_valid {
            lines.push("  ⚠ No valid player pointers — probably not in a fight".to_string());
        }

        // ── Section 3b: Raw controller input scan ──
        // GNT4 stores processed inputs in the player struct but they get consumed each frame.
        // Scan known GC PAD buffer candidates to find where raw held-button state lives.
        lines.push("".to_string());
        lines.push("═══ RAW INPUT SCAN ═══".to_string());
        // Common GNT4/GC PAD buffer locations to check
        let input_candidates: &[(u32, &str)] = &[
            (0x80222EB0, "80222EB0"), // GNT4 input area candidate
            (0x80222EB2, "80222EB2"),
            (0x80222EC0, "80222EC0"),
            (0x80222EC2, "80222EC2"),
            (0x80222ED0, "80222ED0"),
            (0x80223340, "80223340"), // Near fight mode
            (0x80223342, "80223342"),
            (0x80223344, "80223344"),
            (0x80223346, "80223346"),
            (0x80223348, "80223348"),
            (0x8022334A, "8022334A"),
        ];
        for &(addr, label) in input_candidates {
            let val = mem.read_u16(addr).unwrap_or(0);
            if val != 0 {
                lines.push(format!("  {} = 0x{:04X} ({}) [NON-ZERO]", label, val, val));
            }
        }
        // Broad scan: 0x80222E00-0x80222F80 for any non-zero u16 (input buffer area)
        lines.push("  ── INPUT AREA SCAN (0x80222E00-0x80222F80) ──".to_string());
        let mut inp_line = String::new();
        for addr in (0x80222E00u32..0x80222F80).step_by(2) {
            let val = mem.read_u16(addr).unwrap_or(0);
            if val != 0 {
                inp_line.push_str(&format!("{:08X}={:04X} ", addr, val));
                if inp_line.len() > 80 {
                    lines.push(format!("  {}", inp_line.trim()));
                    inp_line.clear();
                }
            }
        }
        if !inp_line.is_empty() {
            lines.push(format!("  {}", inp_line.trim()));
        }
        // Also scan PAD buffer area used by GameCube SDK (typically 0x800C3xxx or 0x80431xxx)
        // Try reading the first 48 bytes (4 pads * 12 bytes) from several candidate bases
        for &pad_base in &[0x80431000u32, 0x80431080, 0x800C3F78, 0x800C4000] {
            let btn = mem.read_u16(pad_base).unwrap_or(0);
            let sx = mem.read_u8(pad_base + 2).unwrap_or(0) as i8;
            let sy = mem.read_u8(pad_base + 3).unwrap_or(0) as i8;
            if btn != 0 || sx != 0 || sy != 0 {
                lines.push(format!("  PAD@{:08X}: btn=0x{:04X} stick=({},{})", pad_base, btn, sx, sy));
            }
        }

        // ── Section 4: Gecko code verification ──
        lines.push("".to_string());
        lines.push("═══ GECKO UNLOCK CODES ═══".to_string());
        let gecko_checks: &[(u32, &str, u32)] = &[
            (0x802232E8, "char_unlock", 0x0001FF03),
            (0x802232F0, "extras", 0x00FFFFFF),
            (0x802232FC, "stages", 0x0021),
            (0x80223258, "save_flags", 0x0017),
        ];
        let mut all_gecko_ok = true;
        for &(addr, name, expected) in gecko_checks {
            let val = mem.read_u32(addr).unwrap_or(0);
            let ok = if name == "stages" || name == "save_flags" {
                (val & 0xFFFF0000) >> 16 == expected as u32 || (val & 0xFFFF) == expected as u32
            } else {
                val == expected
            };
            let tag = if ok { "OK" } else { "MISMATCH" };
            if !ok { all_gecko_ok = false; }
            lines.push(format!("{}: 0x{:08X} (want 0x{:08X}) [{}]", name, val, expected, tag));
        }

        let gecko_status = if all_gecko_ok {
            "APPLIED".to_string()
        } else {
            "NOT APPLIED — will attempt live write".to_string()
        };
        lines.push(format!("Gecko status: {}", gecko_status));

        // If gecko codes aren't applied, try to apply them now
        if !all_gecko_ok {
            lines.push("".to_string());
            lines.push("═══ APPLYING GECKO CODES ═══".to_string());
            match mem.write_u32(0x802232E8, 0x0001FF03) {
                Ok(_) => lines.push("Wrote 0x0001FF03 → 0x802232E8 (characters)".to_string()),
                Err(e) => lines.push(format!("WRITE FAILED 0x802232E8: {}", e)),
            }
            match mem.write_u32(0x802232F0, 0x00FFFFFF) {
                Ok(_) => lines.push("Wrote 0x00FFFFFF → 0x802232F0 (extras)".to_string()),
                Err(e) => lines.push(format!("WRITE FAILED 0x802232F0: {}", e)),
            }
            match mem.write_u16(0x802232FC, 0x0021) {
                Ok(_) => lines.push("Wrote 0x0021 → 0x802232FC (stages)".to_string()),
                Err(e) => lines.push(format!("WRITE FAILED 0x802232FC: {}", e)),
            }
            match mem.write_u16(0x80223258, 0x0017) {
                Ok(_) => lines.push("Wrote 0x0017 → 0x80223258 (save flags)".to_string()),
                Err(e) => lines.push(format!("WRITE FAILED 0x80223258: {}", e)),
            }
            // Verify
            let v1 = mem.read_u32(0x802232E8).unwrap_or(0);
            let v2 = mem.read_u32(0x802232F0).unwrap_or(0);
            if v1 == 0x0001FF03 && v2 == 0x00FFFFFF {
                lines.push("✓ Gecko codes applied successfully!".to_string());
            } else {
                lines.push(format!("✗ Verify failed: chars=0x{:08X} extras=0x{:08X}", v1, v2));
            }
        }

        // ── Section 5: Memory region info ──
        lines.push("".to_string());
        lines.push("═══ GC RAM REGION ═══".to_string());
        lines.push(format!("Attached base: 0x{:X} ({}MB)", mem.ram_base, mem.ram_size / 1024 / 1024));

        let frame = mem.read_frame_counter().unwrap_or(0);
        let report = FullDebugReport {
            lines: lines.clone(),
            has_valid_players: any_valid,
            gecko_applied: all_gecko_ok || {
                let v1 = mem.read_u32(0x802232E8).unwrap_or(0);
                v1 == 0x0001FF03
            },
            frame_counter: frame,
        };

        // Write report to a file Claude can read
        let log_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("debug_report.txt")))
            .unwrap_or_else(|| std::path::PathBuf::from("debug_report.txt"));
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut content = format!("HowlingWind Debug Report — epoch:{}\n{}\n", secs, "=".repeat(50));
        for line in &lines {
            content.push_str(line);
            content.push('\n');
        }
        content.push_str(&format!("\nhas_valid_players: {}\n", any_valid));
        content.push_str(&format!("gecko_applied: {}\n", report.gecko_applied));
        content.push_str(&format!("frame_counter: {}\n", frame));
        let _ = std::fs::write(&log_path, &content);

        Ok(report)
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

/// Fast input scan — reads P1 struct offsets 0x00-0x20 in a tight burst (120 reads over ~2 seconds)
/// and logs every change to a file. This catches transient button presses that the 2s scan misses.
#[tauri::command]
pub fn dolphin_fast_input_scan(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<String, String> {
    #[cfg(windows)]
    {
        let ms = state.lock().map_err(|e| e.to_string())?;
        let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

        // Resolve P1 pointer
        let p1_ptr = mem.read_u32(0x80226358)?;
        if p1_ptr < 0x80000000 || p1_ptr >= 0x81800000 {
            return Err("P1 pointer not valid — not in battle".to_string());
        }

        let mut log_lines: Vec<String> = Vec::new();
        log_lines.push("Fast Input Scan — 3s delay then 300 samples over ~5 seconds".to_string());
        log_lines.push(format!("P1 struct base: 0x{:08X}", p1_ptr));
        log_lines.push("".to_string());

        // 3-second delay so user can switch to game window
        std::thread::sleep(std::time::Duration::from_secs(3));

        // Take initial snapshot of first 0x20 bytes
        let mut prev = [0u16; 16];
        for i in 0u32..16 {
            prev[i as usize] = mem.read_u16(p1_ptr + i * 2).unwrap_or(0);
        }
        log_lines.push(format!("Initial: {:04X} {:04X} {:04X} {:04X} {:04X} {:04X} {:04X} {:04X} {:04X} {:04X} {:04X} {:04X} {:04X} {:04X} {:04X} {:04X}",
            prev[0], prev[1], prev[2], prev[3], prev[4], prev[5], prev[6], prev[7],
            prev[8], prev[9], prev[10], prev[11], prev[12], prev[13], prev[14], prev[15]));

        // Also scan wider offsets that might be input-related
        // Read a broader set including known candidates
        let watch_offsets: Vec<u32> = vec![
            0x0C, 0x0E, 0x10, 0x12, 0x14, // original input offsets
            0x24, 0x26, 0x28, 0x2A, 0x2C, 0x2E, 0x30, // action state area
            0x68, 0x6A, 0x6C, 0x6E, // more candidates
            0x120, 0x122, 0x124, 0x126, 0x128, 0x12A, 0x12C, // mid struct
            0x24C, 0x24E, 0x250, 0x252, 0x254, 0x256, 0x258, 0x25A, // near health
        ];
        let mut prev_wide: Vec<u16> = watch_offsets.iter()
            .map(|&off| mem.read_u16(p1_ptr + off).unwrap_or(0))
            .collect();

        // Sample 300 times with ~16ms sleep (≈60Hz, ~5 seconds)
        for sample in 0..300 {
            std::thread::sleep(std::time::Duration::from_millis(16));

            let mut changed = false;
            let mut change_desc = format!("#{:03}: ", sample);

            // Check first 0x20 bytes
            for i in 0u32..16 {
                let val = mem.read_u16(p1_ptr + i * 2).unwrap_or(0);
                if val != prev[i as usize] {
                    changed = true;
                    change_desc.push_str(&format!("+{:02X}: {:04X}→{:04X} ", i * 2, prev[i as usize], val));
                    prev[i as usize] = val;
                }
            }

            // Check wide offsets
            for (idx, &off) in watch_offsets.iter().enumerate() {
                let val = mem.read_u16(p1_ptr + off).unwrap_or(0);
                if val != prev_wide[idx] {
                    changed = true;
                    change_desc.push_str(&format!("+{:03X}: {:04X}→{:04X} ", off, prev_wide[idx], val));
                    prev_wide[idx] = val;
                }
            }

            if changed {
                log_lines.push(change_desc);
            }
        }

        log_lines.push("".to_string());
        log_lines.push("Scan complete.".to_string());

        let content = log_lines.join("\n");
        let log_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("fast_input_scan.txt").to_string_lossy().to_string()))
            .unwrap_or_else(|| "fast_input_scan.txt".to_string());
        let _ = std::fs::write(log_path, &content);

        Ok(format!("{} changes detected, saved to fast_input_scan.txt", log_lines.len() - 4))
    }

    #[cfg(not(windows))]
    Err("Not supported on this platform".to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FullDebugReport {
    pub lines: Vec<String>,
    pub has_valid_players: bool,
    pub gecko_applied: bool,
    pub frame_counter: u32,
}

// ── Helpers ──

#[cfg(windows)]
fn find_dolphin_pid() -> Result<u32, String> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err("Failed to create process snapshot".to_string());
        }

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snapshot, &mut entry) == 0 {
            CloseHandle(snapshot);
            return Err("Failed to enumerate processes".to_string());
        }

        loop {
            let name = String::from_utf16_lossy(
                &entry.szExeFile[..entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0)],
            );
            let name_lower = name.to_lowercase();
            if name_lower.contains("dolphin") && name_lower.ends_with(".exe") {
                let pid = entry.th32ProcessID;
                CloseHandle(snapshot);
                return Ok(pid);
            }

            if Process32NextW(snapshot, &mut entry) == 0 {
                break;
            }
        }

        CloseHandle(snapshot);
        Err("Dolphin process not found. Make sure Dolphin is running.".to_string())
    }
}

/// Apply GNT4 unlock codes directly into running Dolphin memory.
/// This is the runtime equivalent of Gecko codes — writes the unlock
/// values into GC RAM so the user doesn't need to restart Dolphin.
#[tauri::command]
pub fn dolphin_apply_gecko_live(
    state: tauri::State<'_, Arc<Mutex<DolphinMemState>>>,
) -> Result<String, String> {
    let ms = state.lock().map_err(|e| e.to_string())?;
    let mem = ms.memory.as_ref().ok_or("Not attached to Dolphin")?;

    let mut log = String::new();

    // Gecko code: 02223258 0017FFFF
    // Type 02 = 16-bit write: addr 0x80223258, value 0x0017, count 0xFFFF
    // This means: write 0x0017 to 0x80223258 (repeatedly — but for unlock it's a single write)
    // Actually: Gecko 02 type = "16-Bit Write (Fill)" — writes value to addr
    // 02XXXXXX YYYYYYYY -> write u16 Y to 0x80XXXXXX, fill count is upper Y
    // For 02223258 0017FFFF: write 0xFFFF to 0x80223258... no.
    // Gecko 02: addr=0x80223258, value_and_count=0x0017FFFF
    // Actually the format is: 02XXXXXX YYYYZZZZ where YYYY=value ZZZZ=count+1
    // So: value=0x0017, count=0xFFFF+1=65536 half-words = write 0x0017 to 0x80223258..0x8022xxxx
    // That's a LOT of writes. For unlock, we just need to set the save data.

    // Simpler approach: write the direct unlock values that the codes produce
    // From NicholasMoser's docs, the unlock addresses are:
    // 0x802232E8 = u32 0x0001FF03 (unlocked characters bitfield)
    // 0x802232F0 = u32 0x00FFFFFF (more unlock flags)
    // 0x802232FC = u16 fill starting here

    // Write character unlock bitfield
    mem.write_u32(0x802232E8, 0x0001FF03)?;
    log.push_str("Wrote 0x0001FF03 to 0x802232E8 (character unlocks)\n");

    mem.write_u32(0x802232F0, 0x00FFFFFF)?;
    log.push_str("Wrote 0x00FFFFFF to 0x802232F0 (stage/mode unlocks)\n");

    // For the 02-type fill codes, we write the values they target
    // 02223258 0017FFFF: fill 0x0017 starting at 0x80223258, 0xFFFF+1 halfwords
    // This fills a large region — let's write a reasonable chunk
    // The save data region is around 0x80223258..0x80223258+0x20000
    // But filling 128KB of memory is excessive. Let's just write the key unlock bytes.
    // 022232FC 0021FFFF: fill 0x0021 starting at 0x802232FC
    // This is also a fill. Let's write key offsets.
    mem.write_u16(0x802232FC, 0x0021)?;
    log.push_str("Wrote 0x0021 to 0x802232FC (additional unlocks)\n");
    mem.write_u16(0x802232FE, 0x0021)?;
    log.push_str("Wrote 0x0021 to 0x802232FE\n");

    // Also write the full fill for 02223258: value 0x0017 to save data region
    // The save data appears to be at 0x80223258. Write a few key bytes.
    mem.write_u16(0x80223258, 0x0017)?;
    log.push_str("Wrote 0x0017 to 0x80223258 (save data flags)\n");

    // Verify by reading back
    let v1 = mem.read_u32(0x802232E8)?;
    let v2 = mem.read_u32(0x802232F0)?;
    let v3 = mem.read_u16(0x802232FC)?;
    log.push_str(&format!("\nVerify: 0x802232E8={:#010X}, 0x802232F0={:#010X}, 0x802232FC={:#06X}\n", v1, v2, v3));

    if v1 == 0x0001FF03 && v2 == 0x00FFFFFF {
        log.push_str("SUCCESS: Unlock values written and verified!\n");
        log.push_str("NOTE: You may need to return to the title screen for unlocks to take effect.\n");
    } else {
        log.push_str("WARNING: Readback doesn't match. Memory write may not have worked.\n");
    }

    Ok(log)
}
