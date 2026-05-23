use std::os::raw::c_void;
use std::ptr;
use agent_core::runtime::{Il2CppRuntime, RawClass, RawField};
use crate::il2cpp_ffi::{cstr_to_string, Il2CppApi, Il2CppImage, Il2CppClass};

pub struct RealRuntime {
    api: Il2CppApi,
}

impl RealRuntime {
    pub fn new(api: Il2CppApi) -> Self {
        RealRuntime { api }
    }

    /// Attach the current thread to the il2cpp domain so API calls are valid.
    /// Returns true if attachment succeeded, false if thread_attach is unavailable.
    pub unsafe fn attach_thread(&self) -> bool {
        if let Some(thread_attach) = self.api.thread_attach {
            let domain = (self.api.domain_get)();
            if !domain.is_null() {
                thread_attach(domain);
                return true;
            }
        }
        false
    }

    /// Returns the raw function pointer for thread_attach (if resolved),
    /// so callers can inspect its bytecode for diagnostics.
    pub fn thread_attach_ptr(&self) -> Option<*const u8> {
        self.api.thread_attach.map(|f| f as *const u8)
    }

    /// Expose the API for direct diagnostic calls.
    pub fn api(&self) -> &Il2CppApi {
        &self.api
    }

    pub unsafe fn get_assemblies(&self) -> Vec<(*mut c_void, *mut c_void, String)> {
        let mut out = Vec::new();
        let domain = (self.api.domain_get)();
        if domain.is_null() {
            return out;
        }

        let mut assembly_count: usize = 0;
        let assemblies = (self.api.domain_get_assemblies)(domain, &mut assembly_count);
        if assemblies.is_null() {
            return out;
        }

        for i in 0..assembly_count {
            let assembly = *assemblies.add(i);
            if assembly.is_null() {
                continue;
            }
            let image = (self.api.assembly_get_image)(assembly);
            if image.is_null() {
                continue;
            }
            let name = cstr_to_string((self.api.image_get_name)(image));
            out.push((assembly as *mut c_void, image as *mut c_void, name));
        }
        out
    }

    pub unsafe fn get_classes(&self, image: *mut c_void) -> Vec<(*mut c_void, String, String)> {
        let mut out = Vec::new();
        if image.is_null() {
            return out;
        }
        // image_get_class_count can be mis-resolved on obfuscated builds (returns a
        // bogus count), so don't trust it: iterate until image_get_class returns null,
        // bounded, validating every class pointer before reading it.
        const HARD_CAP: usize = 100_000;
        let mut consecutive_bad = 0usize;
        for ci in 0..HARD_CAP {
            let class = (self.api.image_get_class)(image as *mut Il2CppImage, ci);
            if class.is_null() {
                break;
            }
            if !crate::il2cpp_ffi::mem_readable(class as *const u8, 0x30) {
                consecutive_bad += 1;
                if consecutive_bad > 256 {
                    break;
                }
                continue;
            }
            consecutive_bad = 0;
            let name = cstr_to_string((self.api.class_get_name)(class));
            let namespace = cstr_to_string((self.api.class_get_namespace)(class));
            out.push((class as *mut c_void, name, namespace));
        }
        out
    }

    pub unsafe fn get_class_info(&self, class: *mut c_void) -> Option<(String, String, Vec<(String, String)>)> {
        if class.is_null() || !crate::il2cpp_ffi::mem_readable(class as *const u8, 0x30) {
            return None;
        }
        let name = cstr_to_string((self.api.class_get_name)(class as *mut Il2CppClass));
        let namespace = cstr_to_string((self.api.class_get_namespace)(class as *mut Il2CppClass));
        let mut fields = Vec::new();
        let mut iter: *mut c_void = ptr::null_mut();
        let mut field_guard = 0usize;
        loop {
            field_guard += 1;
            if field_guard > 100_000 {
                break;
            }
            let field = (self.api.class_get_fields)(class as *mut Il2CppClass, &mut iter);
            if field.is_null() {
                break;
            }
            if !crate::il2cpp_ffi::mem_readable(field as *const u8, 0x10) {
                continue;
            }
            let fname = cstr_to_string((self.api.field_get_name)(field));
            let ftype_ptr = (self.api.field_get_type)(field);
            let type_name = if ftype_ptr.is_null() {
                String::new()
            } else {
                cstr_to_string((self.api.type_get_name)(ftype_ptr))
            };
            fields.push((fname, type_name));
        }
        Some((namespace, name, fields))
    }
}

impl Il2CppRuntime for RealRuntime {
    fn enumerate_classes(&self) -> Vec<RawClass> {
        let mut out = Vec::new();
        unsafe {
            let domain = (self.api.domain_get)();
            if domain.is_null() {
                return out;
            }

            let mut assembly_count: usize = 0;
            let assemblies = (self.api.domain_get_assemblies)(domain, &mut assembly_count);
            if assemblies.is_null() {
                return out;
            }

            for i in 0..assembly_count {
                let assembly = *assemblies.add(i);
                if assembly.is_null() {
                    continue;
                }
                let image = (self.api.assembly_get_image)(assembly);
                if image.is_null() {
                    continue;
                }

                const HARD_CAP: usize = 100_000;
                let mut consecutive_bad = 0usize;
                for ci in 0..HARD_CAP {
                    let class = (self.api.image_get_class)(image, ci);
                    if class.is_null() {
                        break;
                    }
                    if !crate::il2cpp_ffi::mem_readable(class as *const u8, 0x30) {
                        consecutive_bad += 1;
                        if consecutive_bad > 256 {
                            break;
                        }
                        continue;
                    }
                    consecutive_bad = 0;

                    let name = cstr_to_string((self.api.class_get_name)(class));
                    let namespace = cstr_to_string((self.api.class_get_namespace)(class));

                    let mut fields = Vec::new();
                    let mut iter: *mut c_void = ptr::null_mut();
                    let mut field_guard = 0usize;
                    loop {
                        field_guard += 1;
                        if field_guard > 100_000 {
                            break;
                        }
                        let field = (self.api.class_get_fields)(class, &mut iter);
                        if field.is_null() {
                            break;
                        }
                        if !crate::il2cpp_ffi::mem_readable(field as *const u8, 0x10) {
                            continue;
                        }
                        let fname = cstr_to_string((self.api.field_get_name)(field));
                        let ftype_ptr = (self.api.field_get_type)(field);
                        let type_name = if ftype_ptr.is_null() {
                            String::new()
                        } else {
                            cstr_to_string((self.api.type_get_name)(ftype_ptr))
                        };
                        fields.push(RawField { name: fname, type_name });
                    }

                    out.push(RawClass { namespace, name, fields });
                }
            }
        }
        out
    }
}
