//! Raw capture: hooks copy bytes into RING; a background thread drains RING to any
//! connected TCP client on 127.0.0.1:50051. All detours are dumb-fast + validated;
//! none may panic the game thread.

use std::io::Write;
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{BOOL, HANDLE};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress, LoadLibraryA};
use windows_sys::Win32::Networking::WinSock::{WSAGetLastError, WSA_IO_PENDING};

use agent_core::protocol::{Direction, FrameRing, RawFrame};

// ─── Ring ────────────────────────────────────────────────────────────────────

/// Capacity caps — the firehose killer (≈64k frames or 64 MB, oldest evicted).
const MAX_FRAMES: usize = 65_536;
const MAX_BYTES: usize = 64 * 1024 * 1024;

static RING: OnceLock<Mutex<FrameRing>> = OnceLock::new();

fn ring() -> &'static Mutex<FrameRing> {
    RING.get_or_init(|| Mutex::new(FrameRing::new(MAX_FRAMES, MAX_BYTES)))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Capture a raw frame. Called from detours: validate length, copy, push. Uses
/// try_lock so a contended/poisoned ring never blocks or panics a hooked thread.
pub(crate) fn capture(direction: Direction, socket_id: u64, ptr: *const u8, len: usize) {
    if ptr.is_null() || len == 0 || len > MAX_BYTES {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    if let Ok(mut r) = ring().try_lock() {
        r.push(RawFrame { timestamp_ms: now_ms(), direction, socket_id, bytes });
    }
}

/// Background thread: every 50 ms, drain the ring and write each frame to the
/// connected client as a length-prefixed record: [u8 dir][u64 socket][u32 len][bytes].
pub fn start_tcp_server() {
    std::thread::spawn(|| {
        let listener = match TcpListener::bind("127.0.0.1:50051") {
            Ok(l) => { crate::paths::log("protocol: TCP raw stream on 127.0.0.1:50051"); l }
            Err(e) => { crate::paths::log(&format!("protocol: TCP bind failed: {}", e)); return; }
        };
        for stream in listener.incoming() {
            let mut stream = match stream { Ok(s) => s, Err(_) => continue };
            crate::paths::log("protocol: client connected");
            loop {
                std::thread::sleep(Duration::from_millis(50));
                let frames = match ring().try_lock() { Ok(mut r) => r.drain(), Err(_) => continue };
                let mut buf = Vec::new();
                for f in &frames {
                    buf.push(if f.direction == Direction::C2S { 0u8 } else { 1u8 });
                    buf.extend_from_slice(&f.socket_id.to_le_bytes());
                    buf.extend_from_slice(&(f.bytes.len() as u32).to_le_bytes());
                    buf.extend_from_slice(&f.bytes);
                }
                if !buf.is_empty() && stream.write_all(&buf).is_err() {
                    crate::paths::log("protocol: client disconnected");
                    break;
                }
            }
        }
    });
}

// ─── FFI type aliases (verbatim from proven packet.rs) ───────────────────────

type SendFn = unsafe extern "system" fn(usize, *const u8, i32, i32) -> i32;
type WSASendFn = unsafe extern "system" fn(
    usize,
    *const WSABUF,
    u32,
    *mut u32,
    u32,
    *mut std::ffi::c_void,
    *mut std::ffi::c_void,
) -> i32;
type SendToFn = unsafe extern "system" fn(
    usize,
    *const u8,
    i32,
    i32,
    *const std::ffi::c_void,
    i32,
) -> i32;
type WSASendToFn = unsafe extern "system" fn(
    usize,
    *const WSABUF,
    u32,
    *mut u32,
    u32,
    *const std::ffi::c_void,
    i32,
    *mut std::ffi::c_void,
    *mut std::ffi::c_void,
) -> i32;
type RecvFn = unsafe extern "system" fn(usize, *mut u8, i32, i32) -> i32;
type WSARecvFn = unsafe extern "system" fn(
    usize,
    *mut WSABUF,
    u32,
    *mut u32,
    *mut u32,
    *mut std::ffi::c_void,
    *mut std::ffi::c_void,
) -> i32;
type RecvFromFn = unsafe extern "system" fn(
    usize,
    *mut u8,
    i32,
    i32,
    *mut std::ffi::c_void,
    *mut i32,
) -> i32;
type WSARecvFromFn = unsafe extern "system" fn(
    usize,
    *mut WSABUF,
    u32,
    *mut u32,
    *mut u32,
    *mut std::ffi::c_void,
    *mut i32,
    *mut std::ffi::c_void,
    *mut std::ffi::c_void,
) -> i32;
type GQCSFn = unsafe extern "system" fn(
    HANDLE,
    *mut u32,
    *mut usize,
    *mut *mut std::ffi::c_void,
    u32,
) -> BOOL;
type GQCSExFn = unsafe extern "system" fn(
    HANDLE,
    *mut OVERLAPPED_ENTRY,
    u32,
    *mut u32,
    u32,
    BOOL,
) -> BOOL;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WSABUF {
    pub len: u32,
    pub buf: *mut u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OVERLAPPED_ENTRY {
    pub lp_completion_key: usize,
    pub lp_overlapped: *mut std::ffi::c_void,
    pub internal: usize,
    pub dw_number_of_bytes_transferred: u32,
}

// ─── Reentrancy guard + state ────────────────────────────────────────────────

thread_local! { static IN_HOOK: std::cell::Cell<bool> = std::cell::Cell::new(false); }

/// Vec of installed Hooks; drained on remove.
static HOOKS: Mutex<Vec<crate::inline_detour::Hook>> = Mutex::new(Vec::new());

/// Count of APC-completion recvs we could not observe (no IOCP path).
static APC_GAP: AtomicU64 = AtomicU64::new(0);

/// Pending async recvs: overlapped ptr → (socket_id, lpBuffers as usize, dwBufferCount).
static PENDING: OnceLock<Mutex<std::collections::HashMap<usize, (u64, usize, u32)>>> =
    OnceLock::new();
const MAX_PENDING: usize = 4096;

fn pending() -> &'static Mutex<std::collections::HashMap<usize, (u64, usize, u32)>> {
    PENDING.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

// ─── One OnceLock<usize> per hooked function (trampoline/original address) ───

static ORIG_SEND:        OnceLock<usize> = OnceLock::new();
static ORIG_WSASEND:     OnceLock<usize> = OnceLock::new();
static ORIG_SENDTO:      OnceLock<usize> = OnceLock::new();
static ORIG_WSASENDTO:   OnceLock<usize> = OnceLock::new();
static ORIG_RECV:        OnceLock<usize> = OnceLock::new();
static ORIG_WSARECV:     OnceLock<usize> = OnceLock::new();
static ORIG_RECVFROM:    OnceLock<usize> = OnceLock::new();
static ORIG_WSARECVFROM: OnceLock<usize> = OnceLock::new();
static ORIG_GQCS:        OnceLock<usize> = OnceLock::new();
static ORIG_GQCSEX:      OnceLock<usize> = OnceLock::new();

// ─── WSABUF helper ───────────────────────────────────────────────────────────

/// Walk a WSABUF array, capturing up to `max_bytes` total into the ring.
unsafe fn capture_wsabufs(
    dir: Direction,
    socket: u64,
    lp_buffers: *const WSABUF,
    count: u32,
    max_bytes: usize,
) {
    if lp_buffers.is_null() || count == 0 || max_bytes == 0 {
        return;
    }
    let mut remaining = max_bytes;
    for i in 0..count as usize {
        if remaining == 0 {
            break;
        }
        let buf = *lp_buffers.add(i);
        if buf.buf.is_null() || buf.len == 0 {
            continue;
        }
        let take = remaining.min(buf.len as usize);
        capture(dir, socket, buf.buf as *const u8, take);
        remaining -= take;
    }
}

// ─── Outgoing detours ────────────────────────────────────────────────────────

unsafe extern "system" fn send_detour(s: usize, buf: *const u8, len: i32, flags: i32) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = ORIG_SEND.get().copied().unwrap_or(0);
        if tramp != 0 { let f: SendFn = std::mem::transmute(tramp); return f(s, buf, len, flags); }
        return -1;
    }
    IN_HOOK.with(|h| h.set(true));
    let tramp = ORIG_SEND.get().copied().unwrap_or(0);
    if tramp == 0 { IN_HOOK.with(|h| h.set(false)); return -1; }
    let orig: SendFn = std::mem::transmute(tramp);
    let ret = orig(s, buf, len, flags);
    if ret > 0 { capture(Direction::C2S, s as u64, buf, ret as usize); }
    IN_HOOK.with(|h| h.set(false));
    ret
}

unsafe extern "system" fn sendto_detour(
    s: usize, buf: *const u8, len: i32, flags: i32,
    to: *const std::ffi::c_void, tolen: i32,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = ORIG_SENDTO.get().copied().unwrap_or(0);
        if tramp != 0 { let f: SendToFn = std::mem::transmute(tramp); return f(s, buf, len, flags, to, tolen); }
        return -1;
    }
    IN_HOOK.with(|h| h.set(true));
    let tramp = ORIG_SENDTO.get().copied().unwrap_or(0);
    if tramp == 0 { IN_HOOK.with(|h| h.set(false)); return -1; }
    let orig: SendToFn = std::mem::transmute(tramp);
    let ret = orig(s, buf, len, flags, to, tolen);
    if ret > 0 { capture(Direction::C2S, s as u64, buf, ret as usize); }
    IN_HOOK.with(|h| h.set(false));
    ret
}

unsafe extern "system" fn wsasend_detour(
    s: usize,
    lp_buffers: *const WSABUF,
    dw_buffer_count: u32,
    lp_number_of_bytes_sent: *mut u32,
    dw_flags: u32,
    lp_overlapped: *mut std::ffi::c_void,
    lp_completion_routine: *mut std::ffi::c_void,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = ORIG_WSASEND.get().copied().unwrap_or(0);
        if tramp != 0 {
            let f: WSASendFn = std::mem::transmute(tramp);
            return f(s, lp_buffers, dw_buffer_count, lp_number_of_bytes_sent, dw_flags, lp_overlapped, lp_completion_routine);
        }
        return -1;
    }
    IN_HOOK.with(|h| h.set(true));
    let tramp = ORIG_WSASEND.get().copied().unwrap_or(0);
    if tramp == 0 { IN_HOOK.with(|h| h.set(false)); return -1; }
    let orig: WSASendFn = std::mem::transmute(tramp);
    let ret = orig(s, lp_buffers, dw_buffer_count, lp_number_of_bytes_sent, dw_flags, lp_overlapped, lp_completion_routine);
    if ret == 0 && !lp_number_of_bytes_sent.is_null() {
        let bytes = *lp_number_of_bytes_sent as usize;
        if bytes > 0 { capture_wsabufs(Direction::C2S, s as u64, lp_buffers, dw_buffer_count, bytes); }
    }
    IN_HOOK.with(|h| h.set(false));
    ret
}

unsafe extern "system" fn wsasendto_detour(
    s: usize,
    lp_buffers: *const WSABUF,
    dw_buffer_count: u32,
    lp_number_of_bytes_sent: *mut u32,
    dw_flags: u32,
    lp_to: *const std::ffi::c_void,
    i_tolen: i32,
    lp_overlapped: *mut std::ffi::c_void,
    lp_completion_routine: *mut std::ffi::c_void,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = ORIG_WSASENDTO.get().copied().unwrap_or(0);
        if tramp != 0 {
            let f: WSASendToFn = std::mem::transmute(tramp);
            return f(s, lp_buffers, dw_buffer_count, lp_number_of_bytes_sent, dw_flags, lp_to, i_tolen, lp_overlapped, lp_completion_routine);
        }
        return -1;
    }
    IN_HOOK.with(|h| h.set(true));
    let tramp = ORIG_WSASENDTO.get().copied().unwrap_or(0);
    if tramp == 0 { IN_HOOK.with(|h| h.set(false)); return -1; }
    let orig: WSASendToFn = std::mem::transmute(tramp);
    let ret = orig(s, lp_buffers, dw_buffer_count, lp_number_of_bytes_sent, dw_flags, lp_to, i_tolen, lp_overlapped, lp_completion_routine);
    if ret == 0 && !lp_number_of_bytes_sent.is_null() {
        let bytes = *lp_number_of_bytes_sent as usize;
        if bytes > 0 { capture_wsabufs(Direction::C2S, s as u64, lp_buffers, dw_buffer_count, bytes); }
    }
    IN_HOOK.with(|h| h.set(false));
    ret
}

// ─── Synchronous recv detours ────────────────────────────────────────────────

unsafe extern "system" fn recv_detour(s: usize, buf: *mut u8, len: i32, flags: i32) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = ORIG_RECV.get().copied().unwrap_or(0);
        if tramp != 0 { let f: RecvFn = std::mem::transmute(tramp); return f(s, buf, len, flags); }
        return -1;
    }
    IN_HOOK.with(|h| h.set(true));
    let tramp = ORIG_RECV.get().copied().unwrap_or(0);
    if tramp == 0 { IN_HOOK.with(|h| h.set(false)); return -1; }
    let orig: RecvFn = std::mem::transmute(tramp);
    let ret = orig(s, buf, len, flags);
    if ret > 0 { capture(Direction::S2C, s as u64, buf as *const u8, ret as usize); }
    IN_HOOK.with(|h| h.set(false));
    ret
}

unsafe extern "system" fn recvfrom_detour(
    s: usize, buf: *mut u8, len: i32, flags: i32,
    from: *mut std::ffi::c_void, fromlen: *mut i32,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = ORIG_RECVFROM.get().copied().unwrap_or(0);
        if tramp != 0 { let f: RecvFromFn = std::mem::transmute(tramp); return f(s, buf, len, flags, from, fromlen); }
        return -1;
    }
    IN_HOOK.with(|h| h.set(true));
    let tramp = ORIG_RECVFROM.get().copied().unwrap_or(0);
    if tramp == 0 { IN_HOOK.with(|h| h.set(false)); return -1; }
    let orig: RecvFromFn = std::mem::transmute(tramp);
    let ret = orig(s, buf, len, flags, from, fromlen);
    if ret > 0 { capture(Direction::S2C, s as u64, buf as *const u8, ret as usize); }
    IN_HOOK.with(|h| h.set(false));
    ret
}

// ─── Async recv detours ──────────────────────────────────────────────────────

unsafe extern "system" fn wsarecv_detour(
    s: usize,
    lp_buffers: *mut WSABUF,
    dw_buffer_count: u32,
    lp_number_of_bytes_recvd: *mut u32,
    lp_flags: *mut u32,
    lp_overlapped: *mut std::ffi::c_void,
    lp_completion_routine: *mut std::ffi::c_void,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = ORIG_WSARECV.get().copied().unwrap_or(0);
        if tramp != 0 {
            let f: WSARecvFn = std::mem::transmute(tramp);
            return f(s, lp_buffers, dw_buffer_count, lp_number_of_bytes_recvd, lp_flags, lp_overlapped, lp_completion_routine);
        }
        return -1;
    }
    IN_HOOK.with(|h| h.set(true));
    let tramp = ORIG_WSARECV.get().copied().unwrap_or(0);
    if tramp == 0 { IN_HOOK.with(|h| h.set(false)); return -1; }
    let orig: WSARecvFn = std::mem::transmute(tramp);
    let ret = orig(s, lp_buffers, dw_buffer_count, lp_number_of_bytes_recvd, lp_flags, lp_overlapped, lp_completion_routine);

    if ret == 0 {
        // Immediate completion
        if !lp_number_of_bytes_recvd.is_null() {
            let bytes = *lp_number_of_bytes_recvd as usize;
            if bytes > 0 {
                capture_wsabufs(Direction::S2C, s as u64, lp_buffers as *const WSABUF, dw_buffer_count, bytes);
            }
        }
    } else {
        // ret == -1
        let err = WSAGetLastError();
        if err == WSA_IO_PENDING && !lp_overlapped.is_null() && !lp_buffers.is_null() && dw_buffer_count > 0 {
            if !lp_completion_routine.is_null() {
                // APC path: we cannot intercept the completion callback
                APC_GAP.fetch_add(1, Ordering::Relaxed);
            } else {
                // IOCP path: register for completion in GQCS/GQCSEx
                if let Ok(mut map) = pending().try_lock() {
                    if map.len() < MAX_PENDING {
                        map.insert(lp_overlapped as usize, (s as u64, lp_buffers as usize, dw_buffer_count));
                    }
                }
            }
        }
    }

    IN_HOOK.with(|h| h.set(false));
    ret
}

unsafe extern "system" fn wsarecvfrom_detour(
    s: usize,
    lp_buffers: *mut WSABUF,
    dw_buffer_count: u32,
    lp_number_of_bytes_recvd: *mut u32,
    lp_flags: *mut u32,
    lp_from: *mut std::ffi::c_void,
    lp_fromlen: *mut i32,
    lp_overlapped: *mut std::ffi::c_void,
    lp_completion_routine: *mut std::ffi::c_void,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = ORIG_WSARECVFROM.get().copied().unwrap_or(0);
        if tramp != 0 {
            let f: WSARecvFromFn = std::mem::transmute(tramp);
            return f(s, lp_buffers, dw_buffer_count, lp_number_of_bytes_recvd, lp_flags, lp_from, lp_fromlen, lp_overlapped, lp_completion_routine);
        }
        return -1;
    }
    IN_HOOK.with(|h| h.set(true));
    let tramp = ORIG_WSARECVFROM.get().copied().unwrap_or(0);
    if tramp == 0 { IN_HOOK.with(|h| h.set(false)); return -1; }
    let orig: WSARecvFromFn = std::mem::transmute(tramp);
    let ret = orig(s, lp_buffers, dw_buffer_count, lp_number_of_bytes_recvd, lp_flags, lp_from, lp_fromlen, lp_overlapped, lp_completion_routine);

    if ret == 0 {
        // Immediate completion
        if !lp_number_of_bytes_recvd.is_null() {
            let bytes = *lp_number_of_bytes_recvd as usize;
            if bytes > 0 {
                capture_wsabufs(Direction::S2C, s as u64, lp_buffers as *const WSABUF, dw_buffer_count, bytes);
            }
        }
    } else {
        let err = WSAGetLastError();
        if err == WSA_IO_PENDING && !lp_overlapped.is_null() && !lp_buffers.is_null() && dw_buffer_count > 0 {
            if !lp_completion_routine.is_null() {
                APC_GAP.fetch_add(1, Ordering::Relaxed);
            } else {
                if let Ok(mut map) = pending().try_lock() {
                    if map.len() < MAX_PENDING {
                        map.insert(lp_overlapped as usize, (s as u64, lp_buffers as usize, dw_buffer_count));
                    }
                }
            }
        }
    }

    IN_HOOK.with(|h| h.set(false));
    ret
}

// ─── IOCP completion detours ─────────────────────────────────────────────────

unsafe extern "system" fn gqcs_detour(
    completion_port: HANDLE,
    lp_number_of_bytes_transferred: *mut u32,
    lp_completion_key: *mut usize,
    lp_overlapped: *mut *mut std::ffi::c_void,
    dw_milliseconds: u32,
) -> BOOL {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = ORIG_GQCS.get().copied().unwrap_or(0);
        if tramp != 0 {
            let f: GQCSFn = std::mem::transmute(tramp);
            return f(completion_port, lp_number_of_bytes_transferred, lp_completion_key, lp_overlapped, dw_milliseconds);
        }
        return 0;
    }
    IN_HOOK.with(|h| h.set(true));
    let tramp = ORIG_GQCS.get().copied().unwrap_or(0);
    if tramp == 0 { IN_HOOK.with(|h| h.set(false)); return 0; }
    let orig: GQCSFn = std::mem::transmute(tramp);
    let res = orig(completion_port, lp_number_of_bytes_transferred, lp_completion_key, lp_overlapped, dw_milliseconds);

    if res != 0 && !lp_overlapped.is_null() && !(*lp_overlapped).is_null() {
        let overlapped_addr = *lp_overlapped as usize;
        let bytes_transferred = if !lp_number_of_bytes_transferred.is_null() {
            *lp_number_of_bytes_transferred as usize
        } else {
            0
        };
        // Always remove to de-leak; only capture if data present
        if let Ok(mut map) = pending().try_lock() {
            if let Some((socket_id, lp_bufs_addr, count)) = map.remove(&overlapped_addr) {
                if bytes_transferred > 0 {
                    capture_wsabufs(
                        Direction::S2C,
                        socket_id,
                        lp_bufs_addr as *const WSABUF,
                        count,
                        bytes_transferred,
                    );
                }
            }
        }
    }

    IN_HOOK.with(|h| h.set(false));
    res
}

unsafe extern "system" fn gqcsex_detour(
    completion_port: HANDLE,
    lp_completion_port_entries: *mut OVERLAPPED_ENTRY,
    ul_count: u32,
    ul_num_entries_removed: *mut u32,
    dw_milliseconds: u32,
    f_alertable: BOOL,
) -> BOOL {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = ORIG_GQCSEX.get().copied().unwrap_or(0);
        if tramp != 0 {
            let f: GQCSExFn = std::mem::transmute(tramp);
            return f(completion_port, lp_completion_port_entries, ul_count, ul_num_entries_removed, dw_milliseconds, f_alertable);
        }
        return 0;
    }
    IN_HOOK.with(|h| h.set(true));
    let tramp = ORIG_GQCSEX.get().copied().unwrap_or(0);
    if tramp == 0 { IN_HOOK.with(|h| h.set(false)); return 0; }
    let orig: GQCSExFn = std::mem::transmute(tramp);
    let res = orig(completion_port, lp_completion_port_entries, ul_count, ul_num_entries_removed, dw_milliseconds, f_alertable);

    if res != 0 && !lp_completion_port_entries.is_null() && !ul_num_entries_removed.is_null() {
        let removed = *ul_num_entries_removed as usize;
        if let Ok(mut map) = pending().try_lock() {
            for i in 0..removed {
                let entry = &*lp_completion_port_entries.add(i);
                let overlapped_addr = entry.lp_overlapped as usize;
                if overlapped_addr == 0 { continue; }
                let bytes_transferred = entry.dw_number_of_bytes_transferred as usize;
                // Always remove to de-leak
                if let Some((socket_id, lp_bufs_addr, count)) = map.remove(&overlapped_addr) {
                    if bytes_transferred > 0 {
                        capture_wsabufs(
                            Direction::S2C,
                            socket_id,
                            lp_bufs_addr as *const WSABUF,
                            count,
                            bytes_transferred,
                        );
                    }
                }
            }
        }
    }

    IN_HOOK.with(|h| h.set(false));
    res
}

// ─── Install / remove ────────────────────────────────────────────────────────

/// Install WinSock + IOCP detours.
pub unsafe fn install_packet_hooks() {
    // Helper: resolve symbol, install hook, store trampoline in ORIG, push Hook.
    // ORIG is set to the trampoline address so detours call the real function through
    // the trampoline — not the now-patched entry point.
    macro_rules! hook_sym {
        ($module:expr, $sym:expr, $orig:expr, $detour:expr) => {{
            let addr_opt = GetProcAddress($module, $sym.as_ptr());
            if let Some(addr) = addr_opt {
                let addr_usize = addr as usize;
                if let Some(h) = crate::inline_detour::install(addr_usize, $detour as *const () as usize) {
                    let _ = $orig.set(h.trampoline);
                    if let Ok(mut hooks) = HOOKS.lock() { hooks.push(h); }
                }
            }
        }};
    }

    let ws2 = {
        let h = GetModuleHandleA(b"ws2_32.dll\0".as_ptr());
        if !h.is_null() { h } else { LoadLibraryA(b"ws2_32.dll\0".as_ptr()) }
    };
    if ws2.is_null() {
        crate::paths::log("protocol: failed to load ws2_32.dll — hooks not installed");
        return;
    }
    let kernel32 = GetModuleHandleA(b"kernel32.dll\0".as_ptr());

    hook_sym!(ws2, b"send\0",         ORIG_SEND,        send_detour);
    hook_sym!(ws2, b"WSASend\0",      ORIG_WSASEND,     wsasend_detour);
    hook_sym!(ws2, b"sendto\0",       ORIG_SENDTO,      sendto_detour);
    hook_sym!(ws2, b"WSASendTo\0",    ORIG_WSASENDTO,   wsasendto_detour);
    hook_sym!(ws2, b"recv\0",         ORIG_RECV,        recv_detour);
    hook_sym!(ws2, b"WSARecv\0",      ORIG_WSARECV,     wsarecv_detour);
    hook_sym!(ws2, b"recvfrom\0",     ORIG_RECVFROM,    recvfrom_detour);
    hook_sym!(ws2, b"WSARecvFrom\0",  ORIG_WSARECVFROM, wsarecvfrom_detour);
    if !kernel32.is_null() {
        hook_sym!(kernel32, b"GetQueuedCompletionStatus\0",   ORIG_GQCS,   gqcs_detour);
        hook_sym!(kernel32, b"GetQueuedCompletionStatusEx\0", ORIG_GQCSEX, gqcsex_detour);
    }

    crate::paths::log("protocol: hooks installed (send/recv family + IOCP)");
}

/// Remove all installed hooks and report APC gap.
pub unsafe fn remove_packet_hooks() {
    if let Ok(mut hooks) = HOOKS.lock() {
        for h in hooks.drain(..) {
            // Hook::remove is called by Drop, but we call it explicitly for clarity
            let mut h = h;
            h.remove();
        }
    }
    let gap = APC_GAP.load(Ordering::Relaxed);
    crate::paths::log(&format!("protocol: hooks removed (APC-completion recvs skipped: {})", gap));
}
