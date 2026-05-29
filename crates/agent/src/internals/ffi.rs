use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

// Opaque il2cpp handles — we only ever hold pointers to them.
pub type Il2CppDomain = c_void;
pub type Il2CppAssembly = c_void;
pub type Il2CppImage = c_void;
pub type Il2CppClass = c_void;
pub type FieldInfo = c_void;
pub type Il2CppType = c_void;
pub type Il2CppThread = c_void;
pub type MethodInfo      = c_void;
pub type Il2CppString    = c_void;
pub type Il2CppArray     = c_void;
pub type Il2CppException = c_void;

type DomainGet = unsafe extern "C" fn() -> *mut Il2CppDomain;
type DomainGetAssemblies =
    unsafe extern "C" fn(*mut Il2CppDomain, *mut usize) -> *mut *mut Il2CppAssembly;
type AssemblyGetImage = unsafe extern "C" fn(*mut Il2CppAssembly) -> *mut Il2CppImage;
type ImageGetClassCount = unsafe extern "C" fn(*mut Il2CppImage) -> usize;
type ImageGetClass = unsafe extern "C" fn(*mut Il2CppImage, usize) -> *mut Il2CppClass;
type ClassGetName = unsafe extern "C" fn(*mut Il2CppClass) -> *const c_char;
type ClassGetNamespace = unsafe extern "C" fn(*mut Il2CppClass) -> *const c_char;
type ClassGetFields = unsafe extern "C" fn(*mut Il2CppClass, *mut *mut c_void) -> *mut FieldInfo;
type FieldGetName = unsafe extern "C" fn(*mut FieldInfo) -> *const c_char;
type FieldGetType = unsafe extern "C" fn(*mut FieldInfo) -> *mut Il2CppType;
type TypeGetName = unsafe extern "C" fn(*mut Il2CppType) -> *mut c_char;
type ThreadAttach = unsafe extern "C" fn(*mut Il2CppDomain) -> *mut Il2CppThread;
type ImageGetName = unsafe extern "C" fn(*mut Il2CppImage) -> *const c_char;
type RuntimeInvoke = unsafe extern "C" fn(
    *mut MethodInfo,
    *mut c_void,
    *mut *mut c_void,
    *mut *mut Il2CppException,
) -> *mut c_void;
type StringNew           = unsafe extern "C" fn(*const c_char) -> *mut Il2CppString;
type ArrayNew            = unsafe extern "C" fn(*mut Il2CppClass, usize) -> *mut Il2CppArray;
type ExceptionGetMessage = unsafe extern "C" fn(*mut Il2CppException) -> *mut Il2CppString;

/// Resolved il2cpp entry points.
///
/// Many fields look unused to the Rust dead-code lint because they're only
/// invoked through transmuted function pointers — the lint can't see across
/// FFI boundaries. They are all called from the dump pipeline.
#[allow(dead_code)]
#[derive(Clone)]
pub struct Il2CppApi {
    pub domain_get: DomainGet,
    pub domain_get_assemblies: DomainGetAssemblies,
    pub assembly_get_image: AssemblyGetImage,
    pub image_get_class_count: ImageGetClassCount,
    pub image_get_class: ImageGetClass,
    pub class_get_name: ClassGetName,
    pub class_get_namespace: ClassGetNamespace,
    /// Optional: stateful field iterator. Has no simple bytecode shape and
    /// in obfuscated builds we may fail to fingerprint it. When `None`, the
    /// dumper falls back to walking `klass->fields` (at klass+0x80) directly
    /// from process memory — still gives us classes; field enumeration via
    /// FFI just becomes a no-op.
    pub class_get_fields: Option<ClassGetFields>,
    pub field_get_name: FieldGetName,
    pub field_get_type: FieldGetType,
    pub type_get_name: TypeGetName,
    pub thread_attach: Option<ThreadAttach>,
    pub image_get_name: ImageGetName,
    pub runtime_invoke:        Option<RuntimeInvoke>,
    pub string_new:            Option<StringNew>,
    pub array_new:             Option<ArrayNew>,
    pub exception_get_message: Option<ExceptionGetMessage>,
}

unsafe fn resolve(
    module: windows_sys::Win32::Foundation::HMODULE,
    name: &[u8],
) -> Option<*const c_void> {
    let proc = GetProcAddress(module, name.as_ptr());
    proc.map(|p| p as *const c_void)
}

#[allow(dead_code)]
struct ExportedFunc {
    name: String,
    _rva: u32,
    final_addr: *const u8,
    code_slice: &'static [u8],
}

/// Dynamic signature scanner for resolving scrambled/obfuscated exports.
unsafe fn resolve_scrambled_exports(
    module: windows_sys::Win32::Foundation::HMODULE,
) -> Option<Il2CppApi> {
    let base_ptr = module as *const u8;
    
    // Parse PE Headers in Memory
    let dos_header = &*(base_ptr as *const windows_sys::Win32::System::SystemServices::IMAGE_DOS_HEADER);
    if dos_header.e_magic != 0x5A4D { // MZ
        return None;
    }
    
    let nt_headers = &*(base_ptr.add(dos_header.e_lfanew as usize) as *const windows_sys::Win32::System::Diagnostics::Debug::IMAGE_NT_HEADERS64);
    if nt_headers.Signature != 0x00004550 { // PE\0\0
        return None;
    }
    
    let export_dir_entry = nt_headers.OptionalHeader.DataDirectory[0]; // IMAGE_DIRECTORY_ENTRY_EXPORT
    if export_dir_entry.VirtualAddress == 0 {
        return None;
    }
    
    let export_dir = &*(base_ptr.add(export_dir_entry.VirtualAddress as usize) as *const windows_sys::Win32::System::SystemServices::IMAGE_EXPORT_DIRECTORY);
    
    let num_names = export_dir.NumberOfNames as usize;
    let funcs_rvas = std::slice::from_raw_parts(
        base_ptr.add(export_dir.AddressOfFunctions as usize) as *const u32,
        export_dir.NumberOfFunctions as usize,
    );
    let names_rvas = std::slice::from_raw_parts(
        base_ptr.add(export_dir.AddressOfNames as usize) as *const u32,
        num_names,
    );
    let ordinals = std::slice::from_raw_parts(
        base_ptr.add(export_dir.AddressOfNameOrdinals as usize) as *const u16,
        num_names,
    );
    
    let mut resolved_exports = Vec::with_capacity(num_names);
    
    for i in 0..num_names {
        let name_ptr = base_ptr.add(names_rvas[i] as usize) as *const i8;
        let name = CStr::from_ptr(name_ptr).to_string_lossy().into_owned();
        
        let ord = ordinals[i] as usize;
        if ord >= funcs_rvas.len() {
            continue;
        }
        let rva = funcs_rvas[ord];
        if rva == 0 {
            continue;
        }
        
        // Resolve JMP (0xE9) instructions dynamically (up to a small depth to avoid cycles)
        let mut curr_addr = base_ptr.add(rva as usize);
        let mut visited = 0;
        loop {
            if visited > 10 {
                break;
            }
            if *curr_addr == 0xE9 {
                // Read 32-bit offset
                let mut offset_bytes = [0u8; 4];
                std::ptr::copy_nonoverlapping(curr_addr.add(1), offset_bytes.as_mut_ptr(), 4);
                let offset = i32::from_le_bytes(offset_bytes);
                curr_addr = curr_addr.add(5).offset(offset as isize);
                visited += 1;
            } else {
                break;
            }
        }
        
        let code_slice = std::slice::from_raw_parts(curr_addr, 64);
        resolved_exports.push(ExportedFunc {
            name,
            _rva: rva,
            final_addr: curr_addr,
            code_slice,
        });
    }

    // --- Dynamic Heuristic Resolvers ---
    
    // Helper: matches a byte signature with wildcards (indicated by 0x100)
    let matches_pattern = |code: &[u8], pattern: &[u16]| -> bool {
        if code.len() < pattern.len() {
            return false;
        }
        for (i, &p) in pattern.iter().enumerate() {
            if p != 0x100 && code[i] != (p as u8) {
                return false;
            }
        }
        true
    };

    // 1. domain_get -> Matches `mov rax, [rip + offset]; ret`
    // Pattern: `48 8B 05 ?? ?? ?? ?? C3`
    // Multiple exports share this shape (different domain-state getters in the
    // same binary). The first match correct often enough; if a specific game
    // needs disambiguation (e.g. multiple candidates point to different globals),
    // add a heuristic that selects the one with the highest cross-reference count.
    let mut domain_get_candidates = Vec::new();
    let pat_domain_get = [0x48, 0x8B, 0x05, 0x100, 0x100, 0x100, 0x100, 0xC3];
    for exp in &resolved_exports {
        if matches_pattern(exp.code_slice, &pat_domain_get) {
            domain_get_candidates.push(exp);
        }
    }
    crate::paths::log(&format!(
        "  sig-scan: domain_get candidates matched = {} (using first @ {:?})",
        domain_get_candidates.len(),
        domain_get_candidates.first().map(|c| c.final_addr)
    ));
    let domain_get_func = domain_get_candidates.first()?;

    // 2. domain_get_assemblies -> Computes assembly count: (end_ptr - start_ptr) >> 3
    // Pattern: `48 8B 05 ?? ?? ?? ?? 48 2B 05 ?? ?? ?? ?? 48 C1 F8 03 48 89 02 48 8B 05 ?? ?? ?? ?? C3`
    let pat_domain_assemblies = [
        0x48, 0x8B, 0x05, 0x100, 0x100, 0x100, 0x100, // mov rax, [rip + s_assemblies_end]
        0x48, 0x2B, 0x05, 0x100, 0x100, 0x100, 0x100, // sub rax, [rip + s_assemblies_begin]
        0x48, 0xC1, 0xF8, 0x03,                       // sar rax, 3
        0x48, 0x89, 0x02,                             // mov [rdx], rax
        0x48, 0x8B, 0x05, 0x100, 0x100, 0x100, 0x100, // mov rax, [rip + s_assemblies_begin]
        0xC3,                                         // ret
    ];
    let domain_get_assemblies_func = resolved_exports.iter().find(|exp| {
        matches_pattern(exp.code_slice, &pat_domain_assemblies)
    })?;

    // 3. assembly_get_image -> Reads assembly->image pointer (first field, offset 0x0).
    // Pattern: `48 8B 01 C3`
    let pat_offset_0 = [0x48, 0x8B, 0x01, 0xC3];
    let assembly_get_image_func = resolved_exports.iter().find(|exp| {
        matches_pattern(exp.code_slice, &pat_offset_0)
    })?;

    // 4. image_get_class_count -> Reads u16 class-count at image+0x08.
    // Pattern: `0F B7 41 08 C3`
    let pat_class_count = [0x0F, 0xB7, 0x41, 0x08, 0xC3];
    let image_get_class_count_func = resolved_exports.iter().find(|exp| {
        matches_pattern(exp.code_slice, &pat_class_count)
    })?;

    // 5. image_get_class -> Indexes into image's class-type array: `types[index]`
    // Pattern: `0F B6 41 ?? 3B D0 73 ?? 48 8B 41 ?? 8B D2 48 8B 04 D0 C3`
    let pat_image_get_class = [
        0x0F, 0xB6, 0x41, 0x100, // movzx eax, byte ptr [rcx + typeCount_offset]
        0x3B, 0xD0,              // cmp edx, eax
        0x73, 0x100,             // jae ...
        0x48, 0x8B, 0x41, 0x100, // mov rax, qword ptr [rcx + types_offset]
        0x8B, 0xD2,              // mov edx, edx
        0x48, 0x8B, 0x04, 0xD0,  // mov rax, qword ptr [rax + rdx*8]
        0xC3,                    // ret
    ];
    let image_get_class_func = resolved_exports.iter().find(|exp| {
        matches_pattern(exp.code_slice, &pat_image_get_class)
    })?;

    // 6. class_get_name -> Reads klass->name string pointer (offset 0x10).
    // Pattern: `48 8B 41 10 C3`
    let pat_offset_10 = [0x48, 0x8B, 0x41, 0x10, 0xC3];
    let class_get_name_func = resolved_exports.iter().find(|exp| {
        matches_pattern(exp.code_slice, &pat_offset_10)
    })?;

    // 7. class_get_namespace -> Reads klass->namespace string pointer (offset 0x18).
    // Pattern: `48 8B 41 18 C3`
    let pat_offset_18 = [0x48, 0x8B, 0x41, 0x18, 0xC3];
    let class_get_namespace_func = resolved_exports.iter().find(|exp| {
        matches_pattern(exp.code_slice, &pat_offset_18)
    })?;

    // 8. class_get_fields — stateful iterator, shaped differently per Unity
    // version so there's no single reliable fingerprint. We try the only shape
    // that's common across v24–v29: `mov rax, [rcx+0x80/0x88]` (load klass->fields
    // pointer into rax). If absent we leave it None and the dumper walks
    // klass->fields directly from memory instead — safer than guessing wrong.
    let class_get_fields_func = resolved_exports.iter().find(|exp| {
        let c = exp.code_slice;
        if c.len() < 8 { return false; }
        // mov rax, [rcx+0x80/0x88]
        c[0] == 0x48 && c[1] == 0x8B && c[2] == 0x81
            && (c[3] == 0x80 || c[3] == 0x88)
            && c[4] == 0x00 && c[5] == 0x00 && c[6] == 0x00
    });

    // 9. field_get_name -> Identical 4-byte stub to assembly_get_image; both read
    // the first pointer field (offset 0x0). Reusing the already-found function is
    // structurally correct for any il2cpp build.
    let field_get_name_func = assembly_get_image_func;

    // 10. field_get_type -> Reads field's Il2CppType pointer at field+0x08.
    // Pattern: `48 8B 41 08 C3`
    let pat_offset_8 = [0x48, 0x8B, 0x41, 0x08, 0xC3];
    let field_get_type_func = resolved_exports.iter().find(|exp| {
        matches_pattern(exp.code_slice, &pat_offset_8)
    })?;

    // 11. type_get_name -> Reads Il2CppType's name string at type+0x20.
    // Pattern: `48 8B 41 20 C3`
    let pat_offset_20 = [0x48, 0x8B, 0x41, 0x20, 0xC3];
    let type_get_name_func = resolved_exports.iter().find(|exp| {
        matches_pattern(exp.code_slice, &pat_offset_20)
    })?;

    // 12. thread_attach -> Try to find it, but it may be a throw-stub.
    // Detect throw-stubs: sub rsp, 0x28; xor edx, edx; call <addr>; int3
    let is_throw_stub = |code: &[u8]| -> bool {
        code.len() >= 12
            && code[0] == 0x48 && code[1] == 0x83 && code[2] == 0xEC && code[3] == 0x28  // sub rsp, 0x28
            && code[4] == 0x33 && code[5] == 0xD2  // xor edx, edx
            && code[6] == 0xE8  // call rel32
            && code[11] == 0xCC  // int3 right after call
    };
    // No reliable signature for thread_attach across builds — when it's a
    // throw-stub (obfuscated runtimes often replace it), we leave it as None
    // and the caller skips explicit thread attach (the runtime auto-attaches
    // the calling OS thread on first call from a non-managed thread anyway).
    let thread_attach_opt: Option<&ExportedFunc> = None;
    let _ = is_throw_stub; // silence dead-code lint when we don't probe a name

    // 13. runtime_invoke — large body. PW-derived prologue pattern (stable across v24
    //     within PW; may need re-fingerprinting for other obfuscated games).
    //     Typical opening: `sub rsp, X; mov [rsp+0x20], r9; mov r10, rdx`.
    //     If the pattern doesn't match, the entire sig-scan resolver returns None
    //     and invoke stays disabled — non-fatal for the rest of the agent.
    let pat_runtime_invoke: [u16; 12] = [
        0x48, 0x83, 0xEC, 0x100,        // sub rsp, X
        0x4C, 0x89, 0x4C, 0x24, 0x20,   // mov [rsp+0x20], r9   (the exc out-param)
        0x49, 0x89, 0xD2,                // mov r10, rdx           (this ptr scratch)
    ];
    let runtime_invoke_func = resolved_exports.iter().find(|exp| {
        matches_pattern(exp.code_slice, &pat_runtime_invoke)
    });
    crate::paths::log(&format!(
        "  sig-scan: runtime_invoke found = {}",
        runtime_invoke_func.is_some()
    ));

    // 14. string_new — `il2cpp_string_new` forwards to a small constructor.
    //     Pattern: `48 89 5C 24 ?? 57 48 83 EC 20` (push rbx + shadow space).
    let pat_string_new: [u16; 10] = [
        0x48, 0x89, 0x5C, 0x24, 0x100,
        0x57, 0x48, 0x83, 0xEC, 0x20,
    ];
    let string_new_func = resolved_exports.iter().find(|exp| {
        matches_pattern(exp.code_slice, &pat_string_new)
    });

    // 15. array_new — same prologue shape as string_new; for PW, the second match
    //     in the export table is the right one.
    let array_new_candidates: Vec<_> = resolved_exports.iter().filter(|exp| {
        matches_pattern(exp.code_slice, &pat_string_new)
    }).collect();
    let array_new_func = array_new_candidates.get(1).copied();

    // 16. exception_get_message — reads exc->message at offset 0x18 typically.
    //     Pattern: `48 8B 41 18 C3` (we already use this for class_get_namespace —
    //     pick the candidate that's NOT class_get_namespace by exclusion).
    let exception_get_message_candidates: Vec<_> = resolved_exports.iter().filter(|exp| {
        matches_pattern(exp.code_slice, &pat_offset_18)
    }).collect();
    let exception_get_message_func = exception_get_message_candidates.iter()
        .find(|f| f.final_addr != class_get_namespace_func.final_addr)
        .copied();

    Some(Il2CppApi {
        domain_get: std::mem::transmute::<*const u8, DomainGet>(domain_get_func.final_addr),
        domain_get_assemblies: std::mem::transmute::<*const u8, DomainGetAssemblies>(domain_get_assemblies_func.final_addr),
        assembly_get_image: std::mem::transmute::<*const u8, AssemblyGetImage>(assembly_get_image_func.final_addr),
        image_get_class_count: std::mem::transmute::<*const u8, ImageGetClassCount>(image_get_class_count_func.final_addr),
        image_get_class: std::mem::transmute::<*const u8, ImageGetClass>(image_get_class_func.final_addr),
        class_get_name: std::mem::transmute::<*const u8, ClassGetName>(class_get_name_func.final_addr),
        class_get_namespace: std::mem::transmute::<*const u8, ClassGetNamespace>(class_get_namespace_func.final_addr),
        class_get_fields: class_get_fields_func.map(|f| std::mem::transmute::<*const u8, ClassGetFields>(f.final_addr)),
        field_get_name: std::mem::transmute::<*const u8, FieldGetName>(field_get_name_func.final_addr),
        field_get_type: std::mem::transmute::<*const u8, FieldGetType>(field_get_type_func.final_addr),
        type_get_name: std::mem::transmute::<*const u8, TypeGetName>(type_get_name_func.final_addr),
        thread_attach: thread_attach_opt.map(|f| std::mem::transmute::<*const u8, ThreadAttach>(f.final_addr)),
        image_get_name: std::mem::transmute::<*const u8, ImageGetName>(assembly_get_image_func.final_addr),
        runtime_invoke: runtime_invoke_func.map(|f|
            std::mem::transmute::<*const u8, RuntimeInvoke>(f.final_addr)
        ),
        string_new: string_new_func.map(|f|
            std::mem::transmute::<*const u8, StringNew>(f.final_addr)
        ),
        array_new: array_new_func.map(|f|
            std::mem::transmute::<*const u8, ArrayNew>(f.final_addr)
        ),
        exception_get_message: exception_get_message_func.map(|f|
            std::mem::transmute::<*const u8, ExceptionGetMessage>(f.final_addr)
        ),
    })
}

impl Il2CppApi {
    /// Top-level resolver. Tries the standard `il2cpp_*` exports first (works
    /// on every non-obfuscated Unity game), then falls back to bytecode-pattern
    /// signature scanning for obfuscated builds whose exports are mangled to
    /// per-build random identifiers.
    ///
    /// Polls briefly until the il2cpp domain has been initialised so callers
    /// receive a ready-to-use API. Never crashes: returns `None` if the runtime
    /// isn't there or its layout is too foreign for either path.
    pub unsafe fn resolve() -> Option<Il2CppApi> {
        if let Some(api) = Self::resolve_from_game_assembly() {
            crate::paths::log("  il2cpp API resolved via standard exports");
            crate::paths::log(&format!(
                "    invoke caps: runtime_invoke={} string_new={} array_new={} exception_get_message={}",
                api.runtime_invoke.is_some(),
                api.string_new.is_some(),
                api.array_new.is_some(),
                api.exception_get_message.is_some(),
            ));
            return Some(api);
        }
        match Self::resolve_obfuscated_api() {
            Some(api) => {
                crate::paths::log("  il2cpp API resolved via signature scan (obfuscated build)");
                crate::paths::log(&format!(
                    "    invoke caps: runtime_invoke={} string_new={} array_new={} exception_get_message={}",
                    api.runtime_invoke.is_some(),
                    api.string_new.is_some(),
                    api.array_new.is_some(),
                    api.exception_get_message.is_some(),
                ));
                Some(api)
            }
            None => {
                crate::paths::log("  il2cpp API resolution FAILED (neither standard exports nor signature scan)");
                None
            }
        }
    }

    /// Bytecode-pattern resolver for obfuscated runtimes. Polls briefly for
    /// domain init so we don't return a half-loaded API to the caller.
    pub unsafe fn resolve_obfuscated_api() -> Option<Il2CppApi> {
    let module = GetModuleHandleA(b"GameAssembly.dll\0".as_ptr());
    if module.is_null() { return None; }
    // Poll briefly for domain init so the caller gets a ready-to-use API.
    use std::thread::sleep;
    use std::time::Duration;
    for _ in 0..30 {
        if let Some(api) = resolve_scrambled_exports(module) {
            if !(api.domain_get)().is_null() {
                return Some(api);
            }
        }
        sleep(Duration::from_millis(200));
    }
    // Final try without domain check — caller can handle null domain.
    resolve_scrambled_exports(module)
}

pub unsafe fn resolve_from_game_assembly() -> Option<Il2CppApi> {
        let module = GetModuleHandleA(b"GameAssembly.dll\0".as_ptr());
        if module.is_null() {
            return None;
        }

        // Try standard resolution first within a closure to catch `None` and fall back.
        let resolve_std = || -> Option<Il2CppApi> {
            macro_rules! get_std {
                ($name:literal, $ty:ty) => {{
                    let p = resolve(module, $name)?;
                    std::mem::transmute::<*const c_void, $ty>(p)
                }};
            }
            // Optional variant of get_std: returns Some on success, None if the export
            // isn't found by that exact name. Used for FFI slots that aren't required
            // for the agent to start.
            macro_rules! try_get_std {
                ($name:literal, $ty:ty) => {{
                    resolve(module, $name).map(|p| std::mem::transmute::<*const c_void, $ty>(p))
                }};
            }
            Some(Il2CppApi {
                domain_get: get_std!(b"il2cpp_domain_get\0", DomainGet),
                domain_get_assemblies: get_std!(b"il2cpp_domain_get_assemblies\0", DomainGetAssemblies),
                assembly_get_image: get_std!(b"il2cpp_assembly_get_image\0", AssemblyGetImage),
                image_get_class_count: get_std!(b"il2cpp_image_get_class_count\0", ImageGetClassCount),
                image_get_class: get_std!(b"il2cpp_image_get_class\0", ImageGetClass),
                class_get_name: get_std!(b"il2cpp_class_get_name\0", ClassGetName),
                class_get_namespace: get_std!(b"il2cpp_class_get_namespace\0", ClassGetNamespace),
                class_get_fields: Some(get_std!(b"il2cpp_class_get_fields\0", ClassGetFields)),
                field_get_name: get_std!(b"il2cpp_field_get_name\0", FieldGetName),
                field_get_type: get_std!(b"il2cpp_field_get_type\0", FieldGetType),
                type_get_name: get_std!(b"il2cpp_type_get_name\0", TypeGetName),
                thread_attach: Some(get_std!(b"il2cpp_thread_attach\0", ThreadAttach)),
                image_get_name: get_std!(b"il2cpp_image_get_name\0", ImageGetName),
                runtime_invoke:        try_get_std!(b"il2cpp_runtime_invoke\0",        RuntimeInvoke),
                string_new:            try_get_std!(b"il2cpp_string_new\0",            StringNew),
                array_new:             try_get_std!(b"il2cpp_array_new\0",             ArrayNew),
                exception_get_message: try_get_std!(b"il2cpp_exception_get_message\0", ExceptionGetMessage),
            })
        };

        // Standard exports only. This succeeds on non-obfuscated Unity games.
        // Obfuscated builds (mangled export names) return None here and the
        // caller (`resolve`) falls back to the bytecode signature scanner.
        resolve_std()
    }
}

use windows_sys::Win32::System::Memory::{
    VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
};

/// How many bytes from `ptr` are committed + readable, capped at `max`.
/// Uses VirtualQuery so we never dereference unmapped memory.
pub unsafe fn readable_len(ptr: *const u8, max: usize) -> usize {
    if ptr.is_null() {
        return 0;
    }
    let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
    let n = VirtualQuery(
        ptr as *const c_void,
        &mut mbi,
        std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
    );
    if n == 0 {
        return 0;
    }
    if mbi.State != MEM_COMMIT {
        return 0;
    }
    let readable_mask = PAGE_READONLY
        | PAGE_READWRITE
        | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ
        | PAGE_EXECUTE_READWRITE
        | PAGE_EXECUTE_WRITECOPY;
    if (mbi.Protect & readable_mask) == 0 {
        return 0;
    }
    if (mbi.Protect & PAGE_GUARD) != 0 {
        return 0;
    }
    let region_end = (mbi.BaseAddress as usize).wrapping_add(mbi.RegionSize);
    let avail = region_end.saturating_sub(ptr as usize);
    avail.min(max)
}

/// Convert a C string pointer into an owned Rust String, safely.
/// Returns "" for null/unreadable pointers; bounded so a non-terminated
/// string can't run off the end of mapped memory.
pub unsafe fn cstr_to_string(ptr: *const c_char) -> String {
    let avail = readable_len(ptr as *const u8, 1024);
    if avail == 0 {
        return String::new();
    }
    let bytes = std::slice::from_raw_parts(ptr as *const u8, avail);
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(avail);
    String::from_utf8_lossy(&bytes[..len]).into_owned()
}
