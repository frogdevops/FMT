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

/// Resolved il2cpp entry points.
pub struct Il2CppApi {
    pub domain_get: DomainGet,
    pub domain_get_assemblies: DomainGetAssemblies,
    pub assembly_get_image: AssemblyGetImage,
    pub image_get_class_count: ImageGetClassCount,
    pub image_get_class: ImageGetClass,
    pub class_get_name: ClassGetName,
    pub class_get_namespace: ClassGetNamespace,
    pub class_get_fields: ClassGetFields,
    pub field_get_name: FieldGetName,
    pub field_get_type: FieldGetType,
    pub type_get_name: TypeGetName,
    pub thread_attach: ThreadAttach,
}

unsafe fn resolve(
    module: windows_sys::Win32::Foundation::HMODULE,
    name: &[u8],
) -> Option<*const c_void> {
    let proc = GetProcAddress(module, name.as_ptr());
    proc.map(|p| p as *const c_void)
}

impl Il2CppApi {
    /// Resolve all needed exports from GameAssembly.dll. Returns None if the
    /// module isn't loaded or any export is missing (e.g. a hostile runtime).
    pub unsafe fn resolve_from_game_assembly() -> Option<Il2CppApi> {
        let module = GetModuleHandleA(b"GameAssembly.dll\0".as_ptr());
        if module.is_null() {
            return None;
        }

        macro_rules! get {
            ($name:literal, $ty:ty) => {{
                let p = resolve(module, $name)?;
                std::mem::transmute::<*const c_void, $ty>(p)
            }};
        }

        Some(Il2CppApi {
            domain_get: get!(b"il2cpp_domain_get\0", DomainGet),
            domain_get_assemblies: get!(b"il2cpp_domain_get_assemblies\0", DomainGetAssemblies),
            assembly_get_image: get!(b"il2cpp_assembly_get_image\0", AssemblyGetImage),
            image_get_class_count: get!(b"il2cpp_image_get_class_count\0", ImageGetClassCount),
            image_get_class: get!(b"il2cpp_image_get_class\0", ImageGetClass),
            class_get_name: get!(b"il2cpp_class_get_name\0", ClassGetName),
            class_get_namespace: get!(b"il2cpp_class_get_namespace\0", ClassGetNamespace),
            class_get_fields: get!(b"il2cpp_class_get_fields\0", ClassGetFields),
            field_get_name: get!(b"il2cpp_field_get_name\0", FieldGetName),
            field_get_type: get!(b"il2cpp_field_get_type\0", FieldGetType),
            type_get_name: get!(b"il2cpp_type_get_name\0", TypeGetName),
            thread_attach: get!(b"il2cpp_thread_attach\0", ThreadAttach),
        })
    }
}

/// Convert a C string pointer from il2cpp into an owned Rust String.
pub unsafe fn cstr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}
