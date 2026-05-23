use std::os::raw::c_void;
use std::path::PathBuf;
use std::ptr;
use std::time::Duration;
use std::net::{TcpListener, TcpStream};
use std::io::{BufRead, BufReader, Write};

use agent_core::dump::build_dump;
use agent_core::format::format_dump;
use agent_core::logfile::{append_log, write_text};
use agent_core::respect::should_decline;

use windows_sys::Win32::Foundation::{BOOL, HANDLE, HMODULE, TRUE};

use crate::il2cpp_ffi::Il2CppApi;
use crate::mem_scan::{scan_gameassembly_for_strings, scan_metadata_candidates, scan_process_for_metadata};
use crate::real_runtime::RealRuntime;
use crate::win::loaded_module_names;

const DLL_PROCESS_ATTACH: u32 = 1;

type LpthreadStartRoutine = unsafe extern "system" fn(*mut c_void) -> u32;

extern "system" {
    fn CreateThread(
        lp_thread_attributes: *const c_void,
        dw_stack_size: usize,
        lp_start_address: Option<LpthreadStartRoutine>,
        lp_parameter: *const c_void,
        dw_creation_flags: u32,
        lp_thread_id: *mut u32,
    ) -> HANDLE;
}

fn log_path() -> PathBuf {
    PathBuf::from("agent.log")
}

fn dump_path() -> PathBuf {
    PathBuf::from("internals.txt")
}

fn log(line: &str) {
    let _ = append_log(&log_path(), line);
}

#[derive(serde::Deserialize)]
#[serde(tag = "cmd")]
enum Request {
    #[serde(rename = "get_assemblies")]
    GetAssemblies,
    #[serde(rename = "get_classes")]
    GetClasses { image: String },
    #[serde(rename = "get_fields")]
    GetFields { class: String },
}

#[derive(serde::Serialize)]
struct AssemblyEntry {
    assembly_address: String,
    image_address: String,
    image_name: String,
}

#[derive(serde::Serialize)]
struct GetAssembliesResponse {
    status: &'static str,
    assemblies: Vec<AssemblyEntry>,
}

#[derive(serde::Serialize)]
struct ClassEntry {
    class_address: String,
    name: String,
    namespace: String,
}

#[derive(serde::Serialize)]
struct GetClassesResponse {
    status: &'static str,
    classes: Vec<ClassEntry>,
}

#[derive(serde::Serialize)]
struct FieldEntry {
    name: String,
    type_name: String,
}

#[derive(serde::Serialize)]
struct GetFieldsResponse {
    status: &'static str,
    namespace: String,
    name: String,
    fields: Vec<FieldEntry>,
}

#[derive(serde::Serialize)]
struct ErrorResponse {
    status: &'static str,
    error: String,
}

fn parse_hex_ptr(s: &str) -> Result<*mut c_void, String> {
    let clean = s.strip_prefix("0x").unwrap_or(s);
    let val = usize::from_str_radix(clean, 16)
        .map_err(|e| format!("Invalid hex address: {}", e))?;
    Ok(val as *mut c_void)
}

fn handle_client(mut stream: TcpStream, runtime: &RealRuntime) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            log(&format!("failed to clone stream: {}", e));
            return;
        }
    });
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let req_str = line.trim();
                if req_str.is_empty() {
                    continue;
                }
                let response_json = match serde_json::from_str::<Request>(req_str) {
                    Ok(req) => match req {
                        Request::GetAssemblies => {
                            let assemblies = unsafe { runtime.get_assemblies() };
                            let list = assemblies.into_iter().map(|(asm, img, name)| AssemblyEntry {
                                assembly_address: format!("0x{:x}", asm as usize),
                                image_address: format!("0x{:x}", img as usize),
                                image_name: name,
                            }).collect();
                            serde_json::to_string(&GetAssembliesResponse {
                                status: "success",
                                assemblies: list,
                            })
                        }
                        Request::GetClasses { image } => {
                            match parse_hex_ptr(&image) {
                                Ok(img_ptr) => {
                                    let classes = unsafe { runtime.get_classes(img_ptr) };
                                    let list = classes.into_iter().map(|(cls, name, ns)| ClassEntry {
                                        class_address: format!("0x{:x}", cls as usize),
                                        name,
                                        namespace: ns,
                                    }).collect();
                                    serde_json::to_string(&GetClassesResponse {
                                        status: "success",
                                        classes: list,
                                    })
                                }
                                Err(err) => serde_json::to_string(&ErrorResponse {
                                    status: "error",
                                    error: err,
                                })
                            }
                        }
                        Request::GetFields { class } => {
                            match parse_hex_ptr(&class) {
                                Ok(cls_ptr) => {
                                    match unsafe { runtime.get_class_info(cls_ptr) } {
                                        Some((ns, name, fields)) => {
                                            let list = fields.into_iter().map(|(fname, ftype)| FieldEntry {
                                                name: fname,
                                                type_name: ftype,
                                            }).collect();
                                            serde_json::to_string(&GetFieldsResponse {
                                                status: "success",
                                                namespace: ns,
                                                name,
                                                fields: list,
                                            })
                                        }
                                        None => serde_json::to_string(&ErrorResponse {
                                            status: "error",
                                            error: "Class not found or null pointer".to_string(),
                                        })
                                    }
                                }
                                Err(err) => serde_json::to_string(&ErrorResponse {
                                    status: "error",
                                    error: err,
                                })
                            }
                        }
                    },
                    Err(err) => serde_json::to_string(&ErrorResponse {
                        status: "error",
                        error: format!("Failed to parse request JSON: {}", err),
                    })
                };

                match response_json {
                    Ok(mut resp) => {
                        resp.push('\n');
                        if let Err(e) = stream.write_all(resp.as_bytes()) {
                            log(&format!("failed to write response: {}", e));
                            break;
                        }
                        let _ = stream.flush();
                    }
                    Err(e) => {
                        log(&format!("failed to serialize response: {}", e));
                        break;
                    }
                }
            }
            Err(e) => {
                log(&format!("read error: {}", e));
                break;
            }
        }
    }
}

extern "system" fn worker(_param: *mut c_void) -> u32 {
    let _ = write_text(&log_path(), "");
    log("agent loaded");

    let modules = loaded_module_names();
    if let Some(reason) = should_decline(&modules) {
        log(&format!("declined: protection detected ({:?})", reason));
        return 0;
    }
    log("respect gate passed");

    // One-shot struct-layout recon for the pointer-chasing approach (read-only).
    log("=== struct layout diagnostic ===");
    for line in unsafe { crate::il2cpp_ffi::dump_struct_diagnostics() } {
        log(&line);
    }
    log("=== end struct layout diagnostic ===");

    log("=== string anchor scan ===");
    {
        let needles = ["global-metadata.dat", "il2cpp_data", "mscorlib.dll"];
        let hits = scan_gameassembly_for_strings(&needles);
        for needle in needles {
            let count = hits.iter().filter(|(n, _)| n == needle).count();
            log(&format!("  '{}' -> {} hits", needle, count));
        }
        for (needle, addr) in hits.iter().take(24) {
            log(&format!("    {} @ {:#x}", needle, addr));
        }
    }
    log("=== end string anchor scan ===");

    // Diagnostic build: the blind full-process blob scans are removed here. We
    // established scanning for the decrypted blob is futile on this target (we
    // don't know where it lives and it may not persist), AND the unbounded
    // full-process read was the remaining crash risk. The string anchor above is
    // step 1 toward finding the metadata-load function to hook instead.
    // Log-only beyond this point; falls through to the safe standard-export wait.

    let api = unsafe {
        let mut attempts = 0;
        loop {
            if let Some(api) = Il2CppApi::resolve_from_game_assembly() {
                let domain = (api.domain_get)();
                if !domain.is_null() {
                    break api;
                }
            }
            attempts += 1;
            if attempts > 600 {
                log("gave up waiting for il2cpp runtime (60s)");
                return 0;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    log("il2cpp runtime ready");

    let runtime = RealRuntime::new(api);

    // --- Diagnostic: log the resolved thread_attach details ---
    unsafe {
        match runtime.thread_attach_ptr() {
            Some(ptr) => {
                let bytes: Vec<u8> = (0..16).map(|i| *ptr.add(i)).collect();
                let hex: Vec<String> = bytes.iter().map(|b| format!("{:02X}", b)).collect();
                log(&format!("thread_attach resolved at {:p}: {}", ptr, hex.join(" ")));
            }
            None => {
                log("thread_attach: UNAVAILABLE (throw-stub detected or export not found)");
                log("  -> The game likely stubbed out il2cpp_thread_attach.");
                log("  -> Proceeding without thread attachment (read-only metadata access).");
            }
        }
    }

    log("calling attach_thread...");
    let attached = unsafe { runtime.attach_thread() };
    if attached {
        log("attach_thread completed!");
    } else {
        log("attach_thread skipped (stub or unavailable, proceeding without attachment)");
    }

    // --- Detailed FFI diagnostics ---
    log("=== FFI Diagnostic Trace ===");
    unsafe {
        let domain = (runtime.api().domain_get)();
        log(&format!("domain_get() => {:p}", domain));

        // Wait for assemblies to become available (they load async after domain init)
        let mut asm_count: usize = 0;
        let mut asm_ptr = std::ptr::null_mut();
        let mut wait_attempts = 0;
        loop {
            asm_count = 0;
            asm_ptr = (runtime.api().domain_get_assemblies)(domain, &mut asm_count);
            if !asm_ptr.is_null() && asm_count > 0 {
                break;
            }
            wait_attempts += 1;
            if wait_attempts % 10 == 0 {
                log(&format!("waiting for assemblies... attempt {} (count={}, ptr={:p})", wait_attempts, asm_count, asm_ptr));
            }
            if wait_attempts > 300 {
                log("gave up waiting for assemblies after 30s");
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        log(&format!("domain_get_assemblies => ptr={:p}, count={}", asm_ptr, asm_count));

        if !asm_ptr.is_null() && asm_count > 0 {
            for i in 0..std::cmp::min(asm_count, 3) {
                let asm = *asm_ptr.add(i);
                log(&format!("  assembly[{}] = {:p}", i, asm));
                if !asm.is_null() {
                    let img = (runtime.api().assembly_get_image)(asm);
                    log(&format!("    assembly_get_image => {:p}", img));
                    if !img.is_null() {
                        let name_ptr = (runtime.api().image_get_name)(img);
                        let name = crate::il2cpp_ffi::cstr_to_string(name_ptr);
                        let class_count = (runtime.api().image_get_class_count)(img);
                        log(&format!("    image_get_name => '{}', class_count => {}", name, class_count));
                    }
                }
            }
        }
    }
    log("=== End FFI Diagnostic Trace ===");

    log("calling get_assemblies...");
    let assemblies = unsafe { runtime.get_assemblies() };
    log(&format!("get_assemblies completed! found {} assemblies", assemblies.len()));

    for (i, (asm, img, name)) in assemblies.iter().enumerate() {
        log(&format!("Assembly #{}: name='{}', asm={:?}, img={:?}", i, name, asm, img));
        
        log(&format!("Querying classes for assembly #{}...", i));
        let classes = unsafe { runtime.get_classes(*img) };
        log(&format!("Found {} classes in assembly #{}", classes.len(), i));
        
        for (j, (cls, cname, cnamespace)) in classes.iter().enumerate().take(5) {
            log(&format!("  Class #{}/{}: {}::{}", i, j, cnamespace, cname));
            log("    Querying class info...");
            if let Some((ns, name, fields)) = unsafe { runtime.get_class_info(*cls) } {
                log(&format!("    Class info: namespace={}, name={}, fields_count={}", ns, name, fields.len()));
            }
        }
    }

    log("Building full dump...");
    let dump = build_dump(&runtime);
    log(&format!(
        "read {} classes, {} fields",
        dump.class_count(),
        dump.total_fields()
    ));

    let text = format_dump(&dump);
    match write_text(&dump_path(), &text) {
        Ok(()) => log("wrote internals.txt"),
        Err(e) => log(&format!("failed to write internals.txt: {}", e)),
    }

    match TcpListener::bind("127.0.0.1:50051") {
        Ok(listener) => {
            log("TCP observer server listening on 127.0.0.1:50051");
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        log("Client connected");
                        handle_client(stream, &runtime);
                        log("Client disconnected");
                    }
                    Err(e) => {
                        log(&format!("Failed to accept connection: {}", e));
                    }
                }
            }
        }
        Err(e) => {
            log(&format!("Failed to bind TCP listener: {}", e));
        }
    }

    0
}

#[no_mangle]
pub extern "system" fn DllMain(_module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        unsafe {
            CreateThread(ptr::null(), 0, Some(worker), ptr::null(), 0, ptr::null_mut());
        }
    }
    TRUE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::io::{Write, BufReader, BufRead};

    // Dummy FFI functions
    unsafe extern "C" fn dummy_domain_get() -> *mut c_void {
        1 as *mut c_void
    }
    unsafe extern "C" fn dummy_domain_get_assemblies(_: *mut c_void, count: *mut usize) -> *mut *mut c_void {
        static ASSEMBLIES: [usize; 1] = [2];
        *count = 1;
        ASSEMBLIES.as_ptr() as *const *mut c_void as *mut *mut c_void
    }
    unsafe extern "C" fn dummy_assembly_get_image(_: *mut c_void) -> *mut c_void {
        3 as *mut c_void
    }
    unsafe extern "C" fn dummy_image_get_class_count(_: *mut c_void) -> usize {
        1
    }
    unsafe extern "C" fn dummy_image_get_class(_: *mut c_void, _: usize) -> *mut c_void {
        4 as *mut c_void
    }
    unsafe extern "C" fn dummy_class_get_name(_: *mut c_void) -> *const std::os::raw::c_char {
        b"DummyClass\0".as_ptr() as *const i8
    }
    unsafe extern "C" fn dummy_class_get_namespace(_: *mut c_void) -> *const std::os::raw::c_char {
        b"DummyNamespace\0".as_ptr() as *const i8
    }
    unsafe extern "C" fn dummy_class_get_fields(_: *mut c_void, iter: *mut *mut c_void) -> *mut c_void {
        if (*iter).is_null() {
            *iter = 99 as *mut c_void;
            5 as *mut c_void
        } else {
            std::ptr::null_mut()
        }
    }
    unsafe extern "C" fn dummy_field_get_name(_: *mut c_void) -> *const std::os::raw::c_char {
        b"dummy_field\0".as_ptr() as *const i8
    }
    unsafe extern "C" fn dummy_field_get_type(_: *mut c_void) -> *mut c_void {
        6 as *mut c_void
    }
    unsafe extern "C" fn dummy_type_get_name(_: *mut c_void) -> *mut std::os::raw::c_char {
        b"System.Int32\0".as_ptr() as *mut i8
    }
    unsafe extern "C" fn dummy_thread_attach(_: *mut c_void) -> *mut c_void {
        std::ptr::null_mut()
    }
    unsafe extern "C" fn dummy_image_get_name(_: *mut c_void) -> *const std::os::raw::c_char {
        b"DummyImage\0".as_ptr() as *const i8
    }

    #[test]
    fn test_tcp_server_commands() {
        let api = Il2CppApi {
            domain_get: dummy_domain_get,
            domain_get_assemblies: dummy_domain_get_assemblies,
            assembly_get_image: dummy_assembly_get_image,
            image_get_class_count: dummy_image_get_class_count,
            image_get_class: dummy_image_get_class,
            class_get_name: dummy_class_get_name,
            class_get_namespace: dummy_class_get_namespace,
            class_get_fields: dummy_class_get_fields,
            field_get_name: dummy_field_get_name,
            field_get_type: dummy_field_get_type,
            type_get_name: dummy_type_get_name,
            thread_attach: Some(dummy_thread_attach),
            image_get_name: dummy_image_get_name,
        };
        let runtime = RealRuntime::new(api);

        // Bind server to port 0 (OS chooses a free port)
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        // Spawn server thread
        let runtime_clone = std::sync::Arc::new(runtime);
        let runtime_for_server = runtime_clone.clone();
        let handle = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                handle_client(stream, &runtime_for_server);
            }
        });

        // Connect client
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());

        // 1. Test get_assemblies
        stream.write_all(b"{\"cmd\":\"get_assemblies\"}\n").unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("\"status\":\"success\""), "Expected success, got: {}", line);
        assert!(line.contains("\"image_name\":\"DummyImage\""), "Expected DummyImage, got: {}", line);
        assert!(line.contains("\"image_address\":\"0x3\""), "Expected 0x3, got: {}", line);

        // 2. Test get_classes
        stream.write_all(b"{\"cmd\":\"get_classes\",\"image\":\"0x3\"}\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("\"status\":\"success\""), "Expected success, got: {}", line);
        assert!(line.contains("\"name\":\"DummyClass\""), "Expected DummyClass, got: {}", line);
        assert!(line.contains("\"class_address\":\"0x4\""), "Expected 0x4, got: {}", line);

        // 3. Test get_fields
        stream.write_all(b"{\"cmd\":\"get_fields\",\"class\":\"0x4\"}\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(line.contains("\"status\":\"success\""), "Expected success, got: {}", line);
        assert!(line.contains("\"name\":\"DummyClass\""), "Expected DummyClass, got: {}", line);
        assert!(line.contains("\"type_name\":\"System.Int32\""), "Expected System.Int32, got: {}", line);
        assert!(line.contains("\"name\":\"dummy_field\""), "Expected dummy_field, got: {}", line);

        // Send EOF/disconnect client
        drop(stream);
        handle.join().unwrap();
    }
}

