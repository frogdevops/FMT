use std::os::raw::c_void;
use std::ptr;

use agent_core::runtime::{Il2CppRuntime, RawClass, RawField};

use crate::il2cpp_ffi::{cstr_to_string, Il2CppApi};

pub struct RealRuntime {
    api: Il2CppApi,
}

impl RealRuntime {
    pub fn new(api: Il2CppApi) -> Self {
        RealRuntime { api }
    }

    /// Attach the current thread to the il2cpp domain so API calls are valid.
    pub unsafe fn attach_thread(&self) {
        let domain = (self.api.domain_get)();
        if !domain.is_null() {
            (self.api.thread_attach)(domain);
        }
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

                let class_count = (self.api.image_get_class_count)(image);
                for ci in 0..class_count {
                    let class = (self.api.image_get_class)(image, ci);
                    if class.is_null() {
                        continue;
                    }

                    let name = cstr_to_string((self.api.class_get_name)(class));
                    let namespace = cstr_to_string((self.api.class_get_namespace)(class));

                    let mut fields = Vec::new();
                    let mut iter: *mut c_void = ptr::null_mut();
                    loop {
                        let field = (self.api.class_get_fields)(class, &mut iter);
                        if field.is_null() {
                            break;
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
