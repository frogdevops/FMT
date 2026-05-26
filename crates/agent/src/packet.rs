use std::cell::Cell;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::mpsc::{channel, Sender};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress, LoadLibraryA};

use crate::hook::Hook;

type SendFn = unsafe extern "system" fn(usize, *const u8, i32, i32) -> i32;
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

static SEND_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static RECV_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static WSARECV_HOOK: Mutex<Option<Hook>> = Mutex::new(None);
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

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::time::{Duration, Instant};

#[derive(serde::Deserialize, serde::Serialize, Clone)]
struct PacketConfig {
    filters: Vec<String>,
    mappings: HashMap<String, String>,
}

static CONFIG: Mutex<Option<(PacketConfig, Instant)>> = Mutex::new(None);

fn get_config() -> PacketConfig {
    let mut guard = CONFIG.lock().unwrap();
    let now = Instant::now();
    
    if let Some((ref cfg, last_loaded)) = *guard {
        if now.duration_since(last_loaded) < Duration::from_secs(2) {
            return cfg.clone();
        }
    }

    let path = crate::paths::output_path("packet_config.json");
    let mut loaded_cfg = None;

    if path.exists() {
        if let Ok(mut file) = File::open(&path) {
            let mut data = String::new();
            if file.read_to_string(&mut data).is_ok() {
                if let Ok(cfg) = serde_json::from_str::<PacketConfig>(&data) {
                    loaded_cfg = Some(cfg);
                }
            }
        }
    }

    let cfg = match loaded_cfg {
        Some(c) => c,
        None => {
            let default_cfg = PacketConfig {
                filters: vec![
                    "ldEM".to_string(),
                    "Islh".to_string(),
                    "vHPe".to_string(),
                ],
                mappings: [
                    ("ldEM", "Heartbeat"),
                    ("GnkD", "Move"),
                    ("VChk", "VersionCheck"),
                    ("RuXN", "Login"),
                    ("ppIX", "JoinWorld"),
                    ("rICq", "WorldLoad"),
                    ("Islh", "Idle"),
                    ("UtgH", "Navigate"),
                    ("sMMF", "PlaceBlock"),
                    ("dIIB", "SelectItem"),
                    ("mSjb", "CollectItem"),
                    ("zCXI", "PlayAudio"),
                    ("TiLT", "TapTile"),
                    ("RlLO", "MenuAction"),
                    ("PZlO", "UIAction"),
                    ("yGLu", "WorldState"),
                    ("eIgm", "StopMoving"),
                    ("tmqV", "Settings"),
                    ("vHPe", "Ping"),
                    ("DZJs", "DataRequest"),
                    ("uygc", "Refresh"),
                    ("YWtw", "Sync"),
                    ("xxMa", "Config"),
                    ("Rlsm", "SetValue"),
                    ("yOLm", "Query"),
                    ("sDPK", "Status"),
                    ("uqjs", "Ready"),
                    ("empB", "MenuOpen"),
                    ("Fwpn", "FetchData"),
                    ("JlfF", "Disconnect"),
                    ("fgkp", "Cleanup"),
                ]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            };
            if let Ok(json_str) = serde_json::to_string_pretty(&default_cfg) {
                let _ = std::fs::write(&path, json_str);
            }
            default_cfg
        }
    };

    *guard = Some((cfg.clone(), now));
    cfg
}

fn format_activity_line(entry: &PacketEntry) -> String {
    let direction_tag = match entry.direction {
        Direction::C2S => "[OUTGOING]",
        Direction::S2C => "[INCOMING]",
    };

    let mut label = "???".to_string();
    let mut pretty_json = String::new();
    let config = get_config();

    if let Some(ref bson_str) = entry.bson_json {
        if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(bson_str) {
            // Build pretty json
            if let Ok(pretty) = serde_json::to_string_pretty(&json_val) {
                // Indent pretty json
                pretty_json = pretty.lines().map(|line| format!("  {}", line)).collect::<Vec<_>>().join("\n");
            }

            // Extract labels
            if let Some(obj) = json_val.as_object() {
                let mut keys: Vec<&String> = obj.keys().collect();
                keys.sort();
                let mut labels = Vec::new();
                for key in keys {
                    if let Some(val_obj) = obj.get(key).and_then(|v| v.as_object()) {
                        if let Some(id_val) = val_obj.get("ID").and_then(|id| id.as_str()) {
                            let name = match config.mappings.get(id_val) {
                                Some(name_str) => name_str.as_str(),
                                None => id_val,
                            };
                            labels.push(name);
                        }
                    }
                }
                if !labels.is_empty() {
                    label = labels.join(" + ");
                }
            }
        }
    }

    if entry.bson_json.is_some() {
        format!("{} {} ({} bytes)\n{}\n\n", direction_tag, label, entry.raw_len, pretty_json)
    } else {
        if entry.raw_len <= 1 {
            String::new()
        } else {
            format!("{} [RAW {} bytes] {}\n\n", direction_tag, entry.raw_len, entry.raw_preview)
        }
    }
}

fn should_filter_packet(_len: usize, bson_json: &Option<String>) -> bool {
    // 1. If it's not a parsed BSON document, it's just raw TCP/TLS overhead, filter it out.
    let bson_str = match bson_json {
        Some(s) => s,
        None => return true,
    };

    let config = get_config();

    // 2. Parse the BSON JSON value
    if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(bson_str) {
        if let Some(obj) = json_val.as_object() {
            let mut has_gameplay_events = false;
            let mut has_other_keys = false;

            for (key, val) in obj {
                let is_msg_key = key.starts_with('m') && key[1..].chars().all(|c| c.is_ascii_digit());
                if is_msg_key {
                    if let Some(val_obj) = val.as_object() {
                        if let Some(id_str) = val_obj.get("ID").and_then(|id| id.as_str()) {
                            if config.filters.iter().any(|f| f == id_str) {
                                // It is one of the filtered IDs
                            } else {
                                has_gameplay_events = true;
                            }
                        } else {
                            has_other_keys = true;
                        }
                    } else {
                        has_other_keys = true;
                    }
                } else if key != "sGot" {
                    has_other_keys = true;
                }
            }

            // Filter out if it contains only background noise IDs or ACK fields
            if !has_gameplay_events && !has_other_keys {
                return true;
            }
        }
    }

    false
}

fn log_packet_entry(entry: PacketEntry) {
    if should_filter_packet(entry.raw_len, &entry.bson_json) {
        return;
    }

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

    if res == 0 {
        let bytes_received = if !lpNumberOfBytesRecvd.is_null() {
            *lpNumberOfBytesRecvd as usize
        } else {
            0
        };

        if bytes_received > 0 && !lpBuffers.is_null() && dwBufferCount > 0 {
            let mut gathered = Vec::with_capacity(bytes_received);
            let mut remaining = bytes_received;
            for i in 0..dwBufferCount {
                if remaining == 0 {
                    break;
                }
                let buf_ptr = lpBuffers.add(i as usize);
                let buf_len = (*buf_ptr).len as usize;
                let buf_data = (*buf_ptr).buf;
                if !buf_data.is_null() && buf_len > 0 {
                    let to_copy = std::cmp::min(remaining, buf_len);
                    let slice = std::slice::from_raw_parts(buf_data, to_copy);
                    gathered.extend_from_slice(slice);
                    remaining -= to_copy;
                }
            }

            if !gathered.is_empty() {
                let preview = hex_preview(&gathered);
                let bson_json = crate::bson::try_parse_bson(&gathered);

                let entry = PacketEntry {
                    timestamp_ms: get_timestamp(),
                    direction: Direction::S2C,
                    raw_len: gathered.len(),
                    bson_json,
                    raw_preview: preview,
                };

                log_packet_entry(entry);
            }
        }
    }

    IN_HOOK.with(|h| h.set(false));
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
    let recv_addr = GetProcAddress(ws2, b"recv\0".as_ptr());
    let wsarecv_addr = GetProcAddress(ws2, b"WSARecv\0".as_ptr());

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

    if let Some(wsarecv_addr) = wsarecv_addr {
        let hook = crate::hook::install(wsarecv_addr as usize, wsarecv_detour as *const () as usize);
        if let Some(hook) = hook {
            *WSARECV_HOOK.lock().unwrap() = Some(hook);
            crate::paths::log("Winsock WSARecv hooked successfully");
        }
    } else {
        crate::paths::log("Failed to resolve WSARecv in ws2_32.dll");
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
    if let Some(mut hook) = WSARECV_HOOK.lock().unwrap().take() {
        hook.remove();
        crate::paths::log("Winsock WSARecv unhooked");
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
