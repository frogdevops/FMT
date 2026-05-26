use std::cell::Cell;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::mpsc::{channel, Sender};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress, LoadLibraryA};

use crate::hook::Hook;

type SendFn = unsafe extern "system" fn(usize, *const u8, i32, i32) -> i32;
type RecvFn = unsafe extern "system" fn(usize, *mut u8, i32, i32) -> i32;

static SEND_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static RECV_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static PACKET_LOG: Mutex<Vec<PacketEntry>> = Mutex::new(Vec::new());
static PACKET_SENDER: OnceLock<Sender<String>> = OnceLock::new();

thread_local! {
    static IN_HOOK: Cell<bool> = Cell::new(false);
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub enum Direction {
    C2S,
    S2C,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PacketEntry {
    pub timestamp_ms: u64,
    pub direction: Direction,
    pub raw_len: usize,
    pub bson_json: Option<String>,
    pub raw_preview: String,
}

fn hex_preview(buf: &[u8]) -> String {
    let limit = std::cmp::min(buf.len(), 64);
    let mut s = String::with_capacity(limit * 2);
    for &b in &buf[..limit] {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn get_timestamp() -> u64 {
    static START_TIME: OnceLock<std::time::Instant> = OnceLock::new();
    START_TIME.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

fn log_packet_entry(entry: PacketEntry) {
    let json_line = match serde_json::to_string(&entry) {
        Ok(line) => line,
        Err(_) => return,
    };

    // Append to packets.log
    let path = crate::paths::output_path("packets.log");
    let _ = agent_core::logfile::append_log(&path, &json_line);

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
        let bson_json = crate::bson::try_parse_bson(data);

        let entry = PacketEntry {
            timestamp_ms: get_timestamp(),
            direction: Direction::C2S,
            raw_len: len as usize,
            bson_json,
            raw_preview: preview,
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

    if bytes_received > 0 {
        let data = std::slice::from_raw_parts(buf, bytes_received as usize);
        let preview = hex_preview(data);
        let bson_json = crate::bson::try_parse_bson(data);

        let entry = PacketEntry {
            timestamp_ms: get_timestamp(),
            direction: Direction::S2C,
            raw_len: bytes_received as usize,
            bson_json,
            raw_preview: preview,
        };

        log_packet_entry(entry);
    }

    IN_HOOK.with(|h| h.set(false));
    bytes_received
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
    let recv_addr = GetProcAddress(ws2, b"recv\0".as_ptr());

    if let Some(send_addr) = send_addr {
        let hook = crate::hook::install(send_addr as usize, send_detour as *const () as usize);
        if let Some(hook) = hook {
            *SEND_HOOK.lock().unwrap() = Some(hook);
            crate::paths::log("Winsock send hooked successfully");
        }
    } else {
        crate::paths::log("Failed to resolve send in ws2_32.dll");
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
}

#[allow(dead_code)]
pub unsafe fn remove_packet_hooks() {
    if let Some(mut hook) = SEND_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock send unhooked");
    }
    if let Some(mut hook) = RECV_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock recv unhooked");
    }
}

#[allow(dead_code)]
pub fn take_packets() -> Vec<PacketEntry> {
    if let Ok(mut log) = PACKET_LOG.lock() {
        std::mem::take(&mut *log)
    } else {
        Vec::new()
    }
}
