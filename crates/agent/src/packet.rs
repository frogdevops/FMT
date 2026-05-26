use std::cell::Cell;
use std::collections::HashMap;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::mpsc::{channel, Sender};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress, LoadLibraryA};

use crate::hook::Hook;

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

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WSABUF {
    pub len: u32,
    pub buf: *mut u8,
}

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

static SEND_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static WSASEND_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static SENDTO_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static WSASENDTO_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static RECV_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static WSARECV_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static RECVFROM_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static WSARECVFROM_HOOK: Mutex<Option<Hook>> = Mutex::new(None);

#[derive(Debug, Clone, Copy)]
pub struct SafeWSABUF {
    pub len: u32,
    pub buf: usize,
}

static GQCS_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static GQCSEX_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static PACKET_LOG: Mutex<Vec<RawPacketFrame>> = Mutex::new(Vec::new());
static PACKET_SENDER: OnceLock<Sender<String>> = OnceLock::new();

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OVERLAPPED_ENTRY {
    pub lp_completion_key: usize,
    pub lp_overlapped: *mut std::ffi::c_void,
    pub internal: usize,
    pub dw_number_of_bytes_transferred: u32,
}

type GQCSFn = unsafe extern "system" fn(
    windows_sys::Win32::Foundation::HANDLE,
    *mut u32,
    *mut usize,
    *mut *mut std::ffi::c_void,
    u32,
) -> windows_sys::Win32::Foundation::BOOL;

type GQCSExFn = unsafe extern "system" fn(
    windows_sys::Win32::Foundation::HANDLE,
    *mut OVERLAPPED_ENTRY,
    u32,
    *mut u32,
    u32,
    windows_sys::Win32::Foundation::BOOL,
) -> windows_sys::Win32::Foundation::BOOL;

fn get_pending_recv() -> &'static Mutex<HashMap<usize, Vec<SafeWSABUF>>> {
    static MAP: OnceLock<Mutex<HashMap<usize, Vec<SafeWSABUF>>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

thread_local! {
    static IN_HOOK: Cell<bool> = Cell::new(false);
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum Direction {
    C2S,
    S2C,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RawPacketFrame {
    pub timestamp_ms: u64,
    pub direction: Direction,
    pub raw_len: usize,
    pub payload_hex: String,
}

fn hex_preview(buf: &[u8]) -> String {
    let mut s = String::with_capacity(buf.len() * 2);
    for &b in buf {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn get_timestamp() -> u64 {
    static START_TIME: OnceLock<std::time::Instant> = OnceLock::new();
    START_TIME.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

fn format_activity_line(entry: &RawPacketFrame) -> String {
    let direction_tag = match entry.direction {
        Direction::C2S => "[OUTGOING]",
        Direction::S2C => "[INCOMING]",
    };
    format!("{} {} bytes: {}\n\n", direction_tag, entry.raw_len, entry.payload_hex)
}

fn log_packet_entry(entry: RawPacketFrame) {
    let json_line = match serde_json::to_string(&entry) {
        Ok(line) => line,
        Err(_) => return,
    };

    // Append to packets.log
    let path = crate::paths::output_path("packets.log");
    let _ = agent_core::logfile::append_log(&path, &json_line);

    // Append to activity.log (human-readable, pretty-printed formatting)
    let activity_line = format_activity_line(&entry);
    if !activity_line.is_empty() {
        let activity_path = crate::paths::output_path("activity.log");
        let _ = agent_core::logfile::append_log(&activity_path, &activity_line);
    }

    // Stream to TCP broadcast channel
    if let Some(sender) = PACKET_SENDER.get() {
        let _ = sender.send(json_line);
    }

    // Append to memory log (capped at 10000 entries)
    if let Ok(mut log) = PACKET_LOG.lock() {
        log.push(entry);
        if log.len() > 10000 {
            let to_drain = log.len() - 10000;
            log.drain(0..to_drain);
        }
    }
}

unsafe extern "system" fn send_detour(s: usize, buf: *const u8, len: i32, flags: i32) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = {
            let guard = SEND_HOOK.lock().unwrap();
            guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
        };
        if tramp != 0 {
            let orig: SendFn = std::mem::transmute(tramp);
            return orig(s, buf, len, flags);
        }
        return -1;
    }

    IN_HOOK.with(|h| h.set(true));

    if len > 0 && !buf.is_null() {
        let data = std::slice::from_raw_parts(buf, len as usize);
        let preview = hex_preview(data);

        let entry = RawPacketFrame {
            timestamp_ms: get_timestamp(),
            direction: Direction::C2S,
            raw_len: len as usize,
            payload_hex: preview,
        };

        log_packet_entry(entry);
    }

    let tramp = {
        let guard = SEND_HOOK.lock().unwrap();
        guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
    };
    if tramp == 0 {
        IN_HOOK.with(|h| h.set(false));
        return -1;
    }
    let orig: SendFn = std::mem::transmute(tramp);
    let bytes_sent = orig(s, buf, len, flags);

    let err = if bytes_sent == -1 { unsafe { windows_sys::Win32::Networking::WinSock::WSAGetLastError() } } else { 0 };
    if bytes_sent > 0 || (bytes_sent == -1 && err != windows_sys::Win32::Networking::WinSock::WSAEWOULDBLOCK as i32) {
        crate::paths::log(&format!(
            "[send detour] s={}, len={}, bytes_sent={}, err={}",
            s, len, bytes_sent, err
        ));
    }

    IN_HOOK.with(|h| h.set(false));
    bytes_sent
}

unsafe extern "system" fn recv_detour(s: usize, buf: *mut u8, len: i32, flags: i32) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = {
            let guard = RECV_HOOK.lock().unwrap();
            guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
        };
        if tramp != 0 {
            let orig: RecvFn = std::mem::transmute(tramp);
            return orig(s, buf, len, flags);
        }
        return -1;
    }

    IN_HOOK.with(|h| h.set(true));

    let tramp = {
        let guard = RECV_HOOK.lock().unwrap();
        guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
    };
    if tramp == 0 {
        IN_HOOK.with(|h| h.set(false));
        return -1;
    }
    let orig: RecvFn = std::mem::transmute(tramp);
    let bytes_received = orig(s, buf, len, flags);

    let err = if bytes_received == -1 { unsafe { windows_sys::Win32::Networking::WinSock::WSAGetLastError() } } else { 0 };
    if bytes_received > 0 || (bytes_received == -1 && err != windows_sys::Win32::Networking::WinSock::WSAEWOULDBLOCK as i32) {
        crate::paths::log(&format!(
            "[recv detour] s={}, len={}, bytes_received={}, err={}",
            s, len, bytes_received, err
        ));
    }

    if bytes_received > 0 {
        let data = std::slice::from_raw_parts(buf, bytes_received as usize);
        let preview = hex_preview(data);

        let entry = RawPacketFrame {
            timestamp_ms: get_timestamp(),
            direction: Direction::S2C,
            raw_len: bytes_received as usize,
            payload_hex: preview,
        };

        log_packet_entry(entry);
    }

    IN_HOOK.with(|h| h.set(false));
    bytes_received
}

unsafe extern "system" fn wsarecv_detour(
    s: usize,
    lpBuffers: *mut WSABUF,
    dwBufferCount: u32,
    lpNumberOfBytesRecvd: *mut u32,
    lpFlags: *mut u32,
    lpOverlapped: *mut std::ffi::c_void,
    lpCompletionRoutine: *mut std::ffi::c_void,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = {
            let guard = WSARECV_HOOK.lock().unwrap();
            guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
        };
        if tramp != 0 {
            let orig: WSARecvFn = std::mem::transmute(tramp);
            return orig(s, lpBuffers, dwBufferCount, lpNumberOfBytesRecvd, lpFlags, lpOverlapped, lpCompletionRoutine);
        }
        return -1;
    }

    IN_HOOK.with(|h| h.set(true));

    let tramp = {
        let guard = WSARECV_HOOK.lock().unwrap();
        guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
    };
    if tramp == 0 {
        IN_HOOK.with(|h| h.set(false));
        return -1;
    }
    let orig: WSARecvFn = std::mem::transmute(tramp);
    let res = orig(s, lpBuffers, dwBufferCount, lpNumberOfBytesRecvd, lpFlags, lpOverlapped, lpCompletionRoutine);

    let err = if res == -1 { unsafe { windows_sys::Win32::Networking::WinSock::WSAGetLastError() } } else { 0 };
    let recvd_val = if !lpNumberOfBytesRecvd.is_null() { unsafe { *lpNumberOfBytesRecvd } } else { 999999 };
    crate::paths::log(&format!(
        "[WSARecv detour] s={}, dwBufferCount={}, lpNumberOfBytesRecvd_ptr={:?}, val={}, lpOverlapped={:?} -> res={}, err={}",
        s, dwBufferCount, lpNumberOfBytesRecvd, recvd_val, lpOverlapped, res, err
    ));

    if res == 0 {
        let bytes_received = if !lpNumberOfBytesRecvd.is_null() {
            *lpNumberOfBytesRecvd as usize
        } else {
            0
        };

        if bytes_received > 0 && !lpBuffers.is_null() && dwBufferCount > 0 {
            let mut bufs = Vec::with_capacity(dwBufferCount as usize);
            for i in 0..dwBufferCount {
                let raw_buf = *lpBuffers.add(i as usize);
                bufs.push(SafeWSABUF {
                    len: raw_buf.len,
                    buf: raw_buf.buf as usize,
                });
            }
            handle_async_completed_data(&bufs, bytes_received);
        }
    } else if res == -1 {
        if err == windows_sys::Win32::Networking::WinSock::WSA_IO_PENDING as i32 && !lpOverlapped.is_null() && !lpBuffers.is_null() && dwBufferCount > 0 {
            let mut bufs = Vec::with_capacity(dwBufferCount as usize);
            for i in 0..dwBufferCount {
                let raw_buf = *lpBuffers.add(i as usize);
                bufs.push(SafeWSABUF {
                    len: raw_buf.len,
                    buf: raw_buf.buf as usize,
                });
            }
            get_pending_recv().lock().unwrap().insert(lpOverlapped as usize, bufs);
        }
    }

    IN_HOOK.with(|h| h.set(false));
    res
}

unsafe extern "system" fn wsasend_detour(
    s: usize,
    lpBuffers: *const WSABUF,
    dwBufferCount: u32,
    lpNumberOfBytesSent: *mut u32,
    dwFlags: u32,
    lpOverlapped: *mut std::ffi::c_void,
    lpCompletionRoutine: *mut std::ffi::c_void,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = {
            let guard = WSASEND_HOOK.lock().unwrap();
            guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
        };
        if tramp != 0 {
            let orig: WSASendFn = std::mem::transmute(tramp);
            return orig(s, lpBuffers, dwBufferCount, lpNumberOfBytesSent, dwFlags, lpOverlapped, lpCompletionRoutine);
        }
        return -1;
    }

    IN_HOOK.with(|h| h.set(true));

    if !lpBuffers.is_null() && dwBufferCount > 0 {
        let mut gathered = Vec::new();
        for i in 0..dwBufferCount {
            let buf = *lpBuffers.add(i as usize);
            if !buf.buf.is_null() && buf.len > 0 {
                let slice = std::slice::from_raw_parts(buf.buf, buf.len as usize);
                gathered.extend_from_slice(slice);
            }
        }
        if !gathered.is_empty() {
            let preview = hex_preview(&gathered);
            let entry = RawPacketFrame {
                timestamp_ms: get_timestamp(),
                direction: Direction::C2S,
                raw_len: gathered.len(),
                payload_hex: preview,
            };
            log_packet_entry(entry);
        }
    }

    let tramp = {
        let guard = WSASEND_HOOK.lock().unwrap();
        guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
    };
    if tramp == 0 {
        IN_HOOK.with(|h| h.set(false));
        return -1;
    }
    let orig: WSASendFn = std::mem::transmute(tramp);
    let res = orig(s, lpBuffers, dwBufferCount, lpNumberOfBytesSent, dwFlags, lpOverlapped, lpCompletionRoutine);

    let err = if res == -1 { unsafe { windows_sys::Win32::Networking::WinSock::WSAGetLastError() } } else { 0 };
    let sent_val = if !lpNumberOfBytesSent.is_null() { unsafe { *lpNumberOfBytesSent } } else { 999999 };
    if res != 0 || err != windows_sys::Win32::Networking::WinSock::WSAEWOULDBLOCK as i32 {
        crate::paths::log(&format!(
            "[WSASend detour] s={}, dwBufferCount={}, lpNumberOfBytesSent_val={}, res={}, err={}",
            s, dwBufferCount, sent_val, res, err
        ));
    }

    IN_HOOK.with(|h| h.set(false));
    res
}

unsafe extern "system" fn sendto_detour(
    s: usize,
    buf: *const u8,
    len: i32,
    flags: i32,
    to: *const std::ffi::c_void,
    tolen: i32,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = {
            let guard = SENDTO_HOOK.lock().unwrap();
            guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
        };
        if tramp != 0 {
            let orig: SendToFn = std::mem::transmute(tramp);
            return orig(s, buf, len, flags, to, tolen);
        }
        return -1;
    }

    IN_HOOK.with(|h| h.set(true));

    if len > 0 && !buf.is_null() {
        let data = std::slice::from_raw_parts(buf, len as usize);
        let preview = hex_preview(data);
        let entry = RawPacketFrame {
            timestamp_ms: get_timestamp(),
            direction: Direction::C2S,
            raw_len: len as usize,
            payload_hex: preview,
        };
        log_packet_entry(entry);
    }

    let tramp = {
        let guard = SENDTO_HOOK.lock().unwrap();
        guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
    };
    if tramp == 0 {
        IN_HOOK.with(|h| h.set(false));
        return -1;
    }
    let orig: SendToFn = std::mem::transmute(tramp);
    let bytes_sent = orig(s, buf, len, flags, to, tolen);

    let err = if bytes_sent == -1 { unsafe { windows_sys::Win32::Networking::WinSock::WSAGetLastError() } } else { 0 };
    if bytes_sent > 0 || (bytes_sent == -1 && err != windows_sys::Win32::Networking::WinSock::WSAEWOULDBLOCK as i32) {
        crate::paths::log(&format!(
            "[sendto detour] s={}, len={}, bytes_sent={}, err={}",
            s, len, bytes_sent, err
        ));
    }

    IN_HOOK.with(|h| h.set(false));
    bytes_sent
}

unsafe extern "system" fn wsasendto_detour(
    s: usize,
    lpBuffers: *const WSABUF,
    dwBufferCount: u32,
    lpNumberOfBytesSent: *mut u32,
    dwFlags: u32,
    lpTo: *const std::ffi::c_void,
    iTolen: i32,
    lpOverlapped: *mut std::ffi::c_void,
    lpCompletionRoutine: *mut std::ffi::c_void,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = {
            let guard = WSASENDTO_HOOK.lock().unwrap();
            guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
        };
        if tramp != 0 {
            let orig: WSASendToFn = std::mem::transmute(tramp);
            return orig(s, lpBuffers, dwBufferCount, lpNumberOfBytesSent, dwFlags, lpTo, iTolen, lpOverlapped, lpCompletionRoutine);
        }
        return -1;
    }

    IN_HOOK.with(|h| h.set(true));

    if !lpBuffers.is_null() && dwBufferCount > 0 {
        let mut gathered = Vec::new();
        for i in 0..dwBufferCount {
            let buf = *lpBuffers.add(i as usize);
            if !buf.buf.is_null() && buf.len > 0 {
                let slice = std::slice::from_raw_parts(buf.buf, buf.len as usize);
                gathered.extend_from_slice(slice);
            }
        }
        if !gathered.is_empty() {
            let preview = hex_preview(&gathered);
            let entry = RawPacketFrame {
                timestamp_ms: get_timestamp(),
                direction: Direction::C2S,
                raw_len: gathered.len(),
                payload_hex: preview,
            };
            log_packet_entry(entry);
        }
    }

    let tramp = {
        let guard = WSASENDTO_HOOK.lock().unwrap();
        guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
    };
    if tramp == 0 {
        IN_HOOK.with(|h| h.set(false));
        return -1;
    }
    let orig: WSASendToFn = std::mem::transmute(tramp);
    let res = orig(s, lpBuffers, dwBufferCount, lpNumberOfBytesSent, dwFlags, lpTo, iTolen, lpOverlapped, lpCompletionRoutine);

    let err = if res == -1 { unsafe { windows_sys::Win32::Networking::WinSock::WSAGetLastError() } } else { 0 };
    let sent_val = if !lpNumberOfBytesSent.is_null() { unsafe { *lpNumberOfBytesSent } } else { 999999 };
    if res != 0 || err != windows_sys::Win32::Networking::WinSock::WSAEWOULDBLOCK as i32 {
        crate::paths::log(&format!(
            "[WSASendTo detour] s={}, dwBufferCount={}, lpNumberOfBytesSent_val={}, res={}, err={}",
            s, dwBufferCount, sent_val, res, err
        ));
    }

    IN_HOOK.with(|h| h.set(false));
    res
}

unsafe extern "system" fn recvfrom_detour(
    s: usize,
    buf: *mut u8,
    len: i32,
    flags: i32,
    from: *mut std::ffi::c_void,
    fromlen: *mut i32,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = {
            let guard = RECVFROM_HOOK.lock().unwrap();
            guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
        };
        if tramp != 0 {
            let orig: RecvFromFn = std::mem::transmute(tramp);
            return orig(s, buf, len, flags, from, fromlen);
        }
        return -1;
    }

    IN_HOOK.with(|h| h.set(true));

    let tramp = {
        let guard = RECVFROM_HOOK.lock().unwrap();
        guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
    };
    if tramp == 0 {
        IN_HOOK.with(|h| h.set(false));
        return -1;
    }
    let orig: RecvFromFn = std::mem::transmute(tramp);
    let bytes_received = orig(s, buf, len, flags, from, fromlen);

    let err = if bytes_received == -1 { unsafe { windows_sys::Win32::Networking::WinSock::WSAGetLastError() } } else { 0 };
    if bytes_received > 0 || (bytes_received == -1 && err != windows_sys::Win32::Networking::WinSock::WSAEWOULDBLOCK as i32) {
        crate::paths::log(&format!(
            "[recvfrom detour] s={}, len={}, bytes_received={}, err={}",
            s, len, bytes_received, err
        ));
    }

    if bytes_received > 0 && !buf.is_null() {
        let data = std::slice::from_raw_parts(buf, bytes_received as usize);
        let preview = hex_preview(data);
        let entry = RawPacketFrame {
            timestamp_ms: get_timestamp(),
            direction: Direction::S2C,
            raw_len: bytes_received as usize,
            payload_hex: preview,
        };
        log_packet_entry(entry);
    }

    IN_HOOK.with(|h| h.set(false));
    bytes_received
}

unsafe extern "system" fn wsarecvfrom_detour(
    s: usize,
    lpBuffers: *mut WSABUF,
    dwBufferCount: u32,
    lpNumberOfBytesRecvd: *mut u32,
    lpFlags: *mut u32,
    lpFrom: *mut std::ffi::c_void,
    lpFromlen: *mut i32,
    lpOverlapped: *mut std::ffi::c_void,
    lpCompletionRoutine: *mut std::ffi::c_void,
) -> i32 {
    if IN_HOOK.with(|h| h.get()) {
        let tramp = {
            let guard = WSARECVFROM_HOOK.lock().unwrap();
            guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
        };
        if tramp != 0 {
            let orig: WSARecvFromFn = std::mem::transmute(tramp);
            return orig(s, lpBuffers, dwBufferCount, lpNumberOfBytesRecvd, lpFlags, lpFrom, lpFromlen, lpOverlapped, lpCompletionRoutine);
        }
        return -1;
    }

    IN_HOOK.with(|h| h.set(true));

    let tramp = {
        let guard = WSARECVFROM_HOOK.lock().unwrap();
        guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
    };
    if tramp == 0 {
        IN_HOOK.with(|h| h.set(false));
        return -1;
    }
    let orig: WSARecvFromFn = std::mem::transmute(tramp);
    let res = orig(s, lpBuffers, dwBufferCount, lpNumberOfBytesRecvd, lpFlags, lpFrom, lpFromlen, lpOverlapped, lpCompletionRoutine);

    let err = if res == -1 { unsafe { windows_sys::Win32::Networking::WinSock::WSAGetLastError() } } else { 0 };
    let recvd_val = if !lpNumberOfBytesRecvd.is_null() { unsafe { *lpNumberOfBytesRecvd } } else { 999999 };
    
    if res == 0 || (res == -1 && err != windows_sys::Win32::Networking::WinSock::WSAEWOULDBLOCK as i32) {
        crate::paths::log(&format!(
            "[WSARecvFrom detour] s={}, dwBufferCount={}, lpNumberOfBytesRecvd_val={}, res={}, err={}",
            s, dwBufferCount, recvd_val, res, err
        ));
    }

    if res == 0 {
        let bytes_received = if !lpNumberOfBytesRecvd.is_null() {
            *lpNumberOfBytesRecvd as usize
        } else {
            0
        };

        if bytes_received > 0 && !lpBuffers.is_null() && dwBufferCount > 0 {
            let mut bufs = Vec::with_capacity(dwBufferCount as usize);
            for i in 0..dwBufferCount {
                let raw_buf = *lpBuffers.add(i as usize);
                bufs.push(SafeWSABUF {
                    len: raw_buf.len,
                    buf: raw_buf.buf as usize,
                });
            }
            handle_async_completed_data(&bufs, bytes_received);
        }
    } else if res == -1 {
        if err == windows_sys::Win32::Networking::WinSock::WSA_IO_PENDING as i32 && !lpOverlapped.is_null() && !lpBuffers.is_null() && dwBufferCount > 0 {
            let mut bufs = Vec::with_capacity(dwBufferCount as usize);
            for i in 0..dwBufferCount {
                let raw_buf = *lpBuffers.add(i as usize);
                bufs.push(SafeWSABUF {
                    len: raw_buf.len,
                    buf: raw_buf.buf as usize,
                });
            }
            get_pending_recv().lock().unwrap().insert(lpOverlapped as usize, bufs);
        }
    }

    IN_HOOK.with(|h| h.set(false));
    res
}

fn handle_async_completed_data(bufs: &[SafeWSABUF], bytes_received: usize) {
    let mut gathered = Vec::with_capacity(bytes_received);
    let mut remaining = bytes_received;
    for buf in bufs {
        if remaining == 0 {
            break;
        }
        let buf_len = buf.len as usize;
        let buf_data = buf.buf as *mut u8;
        if !buf_data.is_null() && buf_len > 0 {
            let to_copy = std::cmp::min(remaining, buf_len);
            let slice = unsafe { std::slice::from_raw_parts(buf_data, to_copy) };
            gathered.extend_from_slice(slice);
            remaining -= to_copy;
        }
    }

    if !gathered.is_empty() {
        let preview = hex_preview(&gathered);

        let entry = RawPacketFrame {
            timestamp_ms: get_timestamp(),
            direction: Direction::S2C,
            raw_len: gathered.len(),
            payload_hex: preview,
        };

        log_packet_entry(entry);
    }
}

unsafe extern "system" fn gqcs_detour(
    completion_port: windows_sys::Win32::Foundation::HANDLE,
    lp_number_of_bytes_transferred: *mut u32,
    lp_completion_key: *mut usize,
    lp_overlapped: *mut *mut std::ffi::c_void,
    dw_milliseconds: u32,
) -> windows_sys::Win32::Foundation::BOOL {
    let tramp = {
        let guard = GQCS_HOOK.lock().unwrap();
        guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
    };
    if tramp == 0 {
        return 0;
    }
    let orig: GQCSFn = std::mem::transmute(tramp);
    let res = orig(completion_port, lp_number_of_bytes_transferred, lp_completion_key, lp_overlapped, dw_milliseconds);

    if res != 0 && !lp_overlapped.is_null() && !(*lp_overlapped).is_null() {
        let overlapped_addr = *lp_overlapped as usize;
        let bytes_transferred = if !lp_number_of_bytes_transferred.is_null() {
            *lp_number_of_bytes_transferred as usize
        } else {
            0
        };

        let has_pending = get_pending_recv().lock().unwrap().contains_key(&overlapped_addr);
        if bytes_transferred > 0 || has_pending {
            crate::paths::log(&format!(
                "[gqcs detour] overlapped_addr={:#x}, bytes_transferred={}, has_pending={}",
                overlapped_addr, bytes_transferred, has_pending
            ));
        }

        if bytes_transferred > 0 {
            let pending = get_pending_recv().lock().unwrap().remove(&overlapped_addr);
            if let Some(bufs) = pending {
                handle_async_completed_data(&bufs, bytes_transferred);
            }
        } else {
            get_pending_recv().lock().unwrap().remove(&overlapped_addr);
        }
    }

    res
}

unsafe extern "system" fn gqcsex_detour(
    completion_port: windows_sys::Win32::Foundation::HANDLE,
    lp_completion_port_entries: *mut OVERLAPPED_ENTRY,
    ul_count: u32,
    ul_num_entries_removed: *mut u32,
    dw_milliseconds: u32,
    f_alertable: windows_sys::Win32::Foundation::BOOL,
) -> windows_sys::Win32::Foundation::BOOL {
    let tramp = {
        let guard = GQCSEX_HOOK.lock().unwrap();
        guard.as_ref().map(|h| h.trampoline).unwrap_or(0)
    };
    if tramp == 0 {
        return 0;
    }
    let orig: GQCSExFn = std::mem::transmute(tramp);
    let res = orig(completion_port, lp_completion_port_entries, ul_count, ul_num_entries_removed, dw_milliseconds, f_alertable);

    if res != 0 && !lp_completion_port_entries.is_null() && !ul_num_entries_removed.is_null() {
        let removed = *ul_num_entries_removed as usize;
        let mut pending_guard = get_pending_recv().lock().unwrap();
        for i in 0..removed {
            let entry = lp_completion_port_entries.add(i);
            let overlapped_addr = (*entry).lp_overlapped as usize;
            if overlapped_addr != 0 {
                let bytes_transferred = (*entry).dw_number_of_bytes_transferred as usize;
                let has_pending = pending_guard.contains_key(&overlapped_addr);
                if bytes_transferred > 0 || has_pending {
                    crate::paths::log(&format!(
                        "[gqcsex detour] entry={} overlapped_addr={:#x}, bytes_transferred={}, has_pending={}",
                        i, overlapped_addr, bytes_transferred, has_pending
                    ));
                }

                if bytes_transferred > 0 {
                    if let Some(bufs) = pending_guard.remove(&overlapped_addr) {
                        handle_async_completed_data(&bufs, bytes_transferred);
                    }
                } else {
                    pending_guard.remove(&overlapped_addr);
                }
            }
        }
    }

    res
}

#[allow(dead_code)]
pub fn start_tcp_server() {
    let (tx, rx) = channel::<String>();
    if PACKET_SENDER.set(tx).is_err() {
        return;
    }

    std::thread::spawn(move || {
        let listener = match TcpListener::bind("127.0.0.1:50051") {
            Ok(l) => {
                crate::paths::log("TCP server listening on 127.0.0.1:50051");
                l
            }
            Err(e) => {
                crate::paths::log(&format!("Failed to bind TCP server to port 50051: {}", e));
                return;
            }
        };

        let clients: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let clients_clone = clients.clone();

        // Accept client connections thread
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        crate::paths::log("TCP client connected to packet stream");
                        stream.set_write_timeout(Some(std::time::Duration::from_millis(500))).ok();
                        if let Ok(mut guard) = clients_clone.lock() {
                            guard.push(stream);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Broadcast loop thread (reads from channel and writes to clients)
        for msg in rx {
            let payload = format!("{}\n", msg);
            if let Ok(mut guard) = clients.lock() {
                let mut to_remove = Vec::new();
                for (i, client) in guard.iter_mut().enumerate() {
                    if client.write_all(payload.as_bytes()).is_err() {
                        to_remove.push(i);
                    }
                }
                for &idx in to_remove.iter().rev() {
                    guard.remove(idx);
                    crate::paths::log("TCP client disconnected from packet stream");
                }
            }
        }
    });
}

#[allow(dead_code)]
pub unsafe fn install_packet_hooks() {
    let mut ws2 = GetModuleHandleA(b"ws2_32.dll\0".as_ptr());
    if ws2.is_null() {
        ws2 = LoadLibraryA(b"ws2_32.dll\0".as_ptr());
    }
    if ws2.is_null() {
        crate::paths::log("Failed to load/get ws2_32.dll");
        return;
    }

    let send_addr = GetProcAddress(ws2, b"send\0".as_ptr());
    let wsasend_addr = GetProcAddress(ws2, b"WSASend\0".as_ptr());
    let sendto_addr = GetProcAddress(ws2, b"sendto\0".as_ptr());
    let wsasendto_addr = GetProcAddress(ws2, b"WSASendTo\0".as_ptr());

    let recv_addr = GetProcAddress(ws2, b"recv\0".as_ptr());
    let wsarecv_addr = GetProcAddress(ws2, b"WSARecv\0".as_ptr());
    let recvfrom_addr = GetProcAddress(ws2, b"recvfrom\0".as_ptr());
    let wsarecvfrom_addr = GetProcAddress(ws2, b"WSARecvFrom\0".as_ptr());

    if let Some(send_addr) = send_addr {
        let hook = crate::hook::install(send_addr as usize, send_detour as *const () as usize);
        if let Some(hook) = hook {
            *SEND_HOOK.lock().unwrap() = Some(hook);
            crate::paths::log("Winsock send hooked successfully");
        }
    } else {
        crate::paths::log("Failed to resolve send in ws2_32.dll");
    }

    if let Some(wsasend_addr) = wsasend_addr {
        let hook = crate::hook::install(wsasend_addr as usize, wsasend_detour as *const () as usize);
        if let Some(hook) = hook {
            *WSASEND_HOOK.lock().unwrap() = Some(hook);
            crate::paths::log("Winsock WSASend hooked successfully");
        }
    } else {
        crate::paths::log("Failed to resolve WSASend in ws2_32.dll");
    }

    if let Some(sendto_addr) = sendto_addr {
        let hook = crate::hook::install(sendto_addr as usize, sendto_detour as *const () as usize);
        if let Some(hook) = hook {
            *SENDTO_HOOK.lock().unwrap() = Some(hook);
            crate::paths::log("Winsock sendto hooked successfully");
        }
    } else {
        crate::paths::log("Failed to resolve sendto in ws2_32.dll");
    }

    if let Some(wsasendto_addr) = wsasendto_addr {
        let hook = crate::hook::install(wsasendto_addr as usize, wsasendto_detour as *const () as usize);
        if let Some(hook) = hook {
            *WSASENDTO_HOOK.lock().unwrap() = Some(hook);
            crate::paths::log("Winsock WSASendTo hooked successfully");
        }
    } else {
        crate::paths::log("Failed to resolve WSASendTo in ws2_32.dll");
    }

    if let Some(recv_addr) = recv_addr {
        let hook = crate::hook::install(recv_addr as usize, recv_detour as *const () as usize);
        if let Some(hook) = hook {
            *RECV_HOOK.lock().unwrap() = Some(hook);
            crate::paths::log("Winsock recv hooked successfully");
        }
    } else {
        crate::paths::log("Failed to resolve recv in ws2_32.dll");
    }

    if let Some(wsarecv_addr) = wsarecv_addr {
        let hook = crate::hook::install(wsarecv_addr as usize, wsarecv_detour as *const () as usize);
        if let Some(hook) = hook {
            *WSARECV_HOOK.lock().unwrap() = Some(hook);
            crate::paths::log("Winsock WSARecv hooked successfully");
        }
    } else {
        crate::paths::log("Failed to resolve WSARecv in ws2_32.dll");
    }

    if let Some(recvfrom_addr) = recvfrom_addr {
        let hook = crate::hook::install(recvfrom_addr as usize, recvfrom_detour as *const () as usize);
        if let Some(hook) = hook {
            *RECVFROM_HOOK.lock().unwrap() = Some(hook);
            crate::paths::log("Winsock recvfrom hooked successfully");
        }
    } else {
        crate::paths::log("Failed to resolve recvfrom in ws2_32.dll");
    }

    if let Some(wsarecvfrom_addr) = wsarecvfrom_addr {
        let hook = crate::hook::install(wsarecvfrom_addr as usize, wsarecvfrom_detour as *const () as usize);
        if let Some(hook) = hook {
            *WSARECVFROM_HOOK.lock().unwrap() = Some(hook);
            crate::paths::log("Winsock WSARecvFrom hooked successfully");
        }
    } else {
        crate::paths::log("Failed to resolve WSARecvFrom in ws2_32.dll");
    }

    let kernel32 = GetModuleHandleA(b"kernel32.dll\0".as_ptr());
    if !kernel32.is_null() {
        let gqcs_addr = GetProcAddress(kernel32, b"GetQueuedCompletionStatus\0".as_ptr());
        let gqcsex_addr = GetProcAddress(kernel32, b"GetQueuedCompletionStatusEx\0".as_ptr());

        if let Some(gqcs_addr) = gqcs_addr {
            let hook = crate::hook::install(gqcs_addr as usize, gqcs_detour as *const () as usize);
            if let Some(hook) = hook {
                *GQCS_HOOK.lock().unwrap() = Some(hook);
                crate::paths::log("GetQueuedCompletionStatus hooked successfully");
            }
        }

        if let Some(gqcsex_addr) = gqcsex_addr {
            let hook = crate::hook::install(gqcsex_addr as usize, gqcsex_detour as *const () as usize);
            if let Some(hook) = hook {
                *GQCSEX_HOOK.lock().unwrap() = Some(hook);
                crate::paths::log("GetQueuedCompletionStatusEx hooked successfully");
            }
        }
    }
}

#[allow(dead_code)]
pub unsafe fn remove_packet_hooks() {
    if let Some(mut hook) = SEND_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock send unhooked");
    }
    if let Some(mut hook) = WSASEND_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock WSASend unhooked");
    }
    if let Some(mut hook) = SENDTO_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock sendto unhooked");
    }
    if let Some(mut hook) = WSASENDTO_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock WSASendTo unhooked");
    }
    if let Some(mut hook) = RECV_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock recv unhooked");
    }
    if let Some(mut hook) = WSARECV_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock WSARecv unhooked");
    }
    if let Some(mut hook) = RECVFROM_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock recvfrom unhooked");
    }
    if let Some(mut hook) = WSARECVFROM_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock WSARecvFrom unhooked");
    }
    if let Some(mut hook) = GQCS_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("GetQueuedCompletionStatus unhooked");
    }
    if let Some(mut hook) = GQCSEX_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("GetQueuedCompletionStatusEx unhooked");
    }
}

#[allow(dead_code)]
pub fn take_packets() -> Vec<RawPacketFrame> {
    if let Ok(mut log) = PACKET_LOG.lock() {
        std::mem::take(&mut *log)
    } else {
        Vec::new()
    }
}
