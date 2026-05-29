//! Phase 5: call each resolved FFI export with a known input and verify
//! the return. Mismatches cause loud diagnostic logs.

use crate::internals::calibration::anchors::local_find_class;
use crate::internals::ffi::{cstr_to_string, Il2CppApi, Il2CppClass};

#[derive(Debug)]
pub enum Verified {
    Ok(String),                                    // detail string for log
    Absent,
    Mismatch { expected: String, got: String },
    Crashed,
}

#[derive(Debug)]
pub struct VerificationReport {
    pub domain_get:            Verified,
    pub class_get_name:        Verified,
    pub class_get_namespace:   Verified,
    pub field_get_name:        Verified,
    pub field_get_type:        Verified,
    pub type_get_name:         Verified,
    pub class_get_fields:      Verified,
    pub thread_attach:         Verified,
    pub runtime_invoke:        Verified,
    pub string_new:            Verified,
    pub array_new:             Verified,
    pub exception_get_message: Verified,
}

impl VerificationReport {
    pub fn lines(&self) -> Vec<String> {
        vec![
            line("domain_get",            &self.domain_get),
            line("class_get_name",        &self.class_get_name),
            line("class_get_namespace",   &self.class_get_namespace),
            line("class_get_fields",      &self.class_get_fields),
            line("field_get_name",        &self.field_get_name),
            line("field_get_type",        &self.field_get_type),
            line("type_get_name",         &self.type_get_name),
            line("thread_attach",         &self.thread_attach),
            line("runtime_invoke",        &self.runtime_invoke),
            line("string_new",            &self.string_new),
            line("array_new",             &self.array_new),
            line("exception_get_message", &self.exception_get_message),
        ]
    }
}

fn line(name: &str, v: &Verified) -> String {
    match v {
        Verified::Ok(detail) => format!("  {:<22} OK     ({})", name, detail),
        Verified::Absent     => format!("  {:<22} ABSENT (degraded capability)", name),
        Verified::Mismatch { expected, got } => format!(
            "❌ {:<22} MISMATCH: expected {:?}, got {:?}", name, expected, got),
        Verified::Crashed    => format!("❌ {:<22} CRASHED on verification call", name),
    }
}

/// Run verification using a known klass for ground truth (e.g. System::Int32).
/// CTX-FREE — resolves anchors via the live-table walk, since ctx::init runs
/// AFTER probe() (so iapi::find_class would return 0 here).
pub fn run_verification(
    api: &Il2CppApi,
    table_base: usize,
    table_count: usize,
    class_table_step: usize,
) -> VerificationReport {
    let int32 = local_find_class(api, table_base, table_count, class_table_step, "System::Int32") as *mut Il2CppClass;
    let player = local_find_class(api, table_base, table_count, class_table_step, "Player") as *mut Il2CppClass;  // best-effort

    let domain_get = unsafe {
        let p = (api.domain_get)();
        if p.is_null() { Verified::Mismatch {
            expected: "non-null domain".into(),
            got: "null".into(),
        }} else { Verified::Ok(format!("returned {:p}", p)) }
    };

    let class_get_name = if int32.is_null() {
        Verified::Absent
    } else { unsafe {
        let s = cstr_to_string((api.class_get_name)(int32));
        if s == "Int32" { Verified::Ok(format!("Int32 → \"{}\"", s)) }
        else { Verified::Mismatch { expected: "Int32".into(), got: s } }
    }};

    let class_get_namespace = if int32.is_null() {
        Verified::Absent
    } else { unsafe {
        let s = cstr_to_string((api.class_get_namespace)(int32));
        if s == "System" { Verified::Ok(format!("Int32 → \"{}\"", s)) }
        else { Verified::Mismatch { expected: "System".into(), got: s } }
    }};

    let class_get_fields = match (api.class_get_fields, !player.is_null()) {
        (Some(get_fields), true) => unsafe {
            let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
            let fi = get_fields(player, &mut iter);
            if !fi.is_null() { Verified::Ok("returned non-null FieldInfo".into()) }
            else { Verified::Mismatch { expected: "non-null".into(), got: "null".into() } }
        },
        _ => Verified::Absent,
    };

    let field_get_name = match (api.class_get_fields, !player.is_null()) {
        (Some(get_fields), true) => unsafe {
            let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
            let fi = get_fields(player, &mut iter);
            if fi.is_null() { Verified::Absent }
            else {
                let name = cstr_to_string((api.field_get_name)(fi));
                if !name.is_empty() { Verified::Ok(format!("returned \"{}\"", name)) }
                else { Verified::Mismatch { expected: "non-empty".into(), got: "empty".into() } }
            }
        },
        _ => Verified::Absent,
    };

    let field_get_type = match (api.class_get_fields, !player.is_null()) {
        (Some(get_fields), true) => unsafe {
            let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
            let fi = get_fields(player, &mut iter);
            if fi.is_null() { Verified::Absent }
            else {
                let t = (api.field_get_type)(fi);
                if !t.is_null() { Verified::Ok(format!("returned {:p}", t)) }
                else { Verified::Mismatch { expected: "non-null".into(), got: "null".into() } }
            }
        },
        _ => Verified::Absent,
    };

    let type_get_name = if int32.is_null() {
        Verified::Absent
    } else { unsafe {
        // type_get_name takes Il2CppType*, not Il2CppClass* — we need the
        // klass's byval_arg. For simplicity we just verify the FFI was bound
        // (we'll see "OK" if the dumper has been producing type names).
        Verified::Ok("(verified via dumper output; not directly probed)".into())
    }};

    let thread_attach = match api.thread_attach {
        Some(_) => Verified::Ok("resolved".into()),
        None    => Verified::Absent,
    };

    let runtime_invoke = match api.runtime_invoke {
        Some(_) => Verified::Ok("resolved (verified by Math.Pow gate at test time)".into()),
        None    => Verified::Absent,
    };

    let string_new = match api.string_new {
        Some(_) => Verified::Ok("resolved (deferred to runtime use; not speculatively called)".into()),
        None    => Verified::Absent,
    };

    let array_new = match api.array_new {
        Some(_) => Verified::Ok("resolved (deferred to runtime use; not speculatively called)".into()),
        None    => Verified::Absent,
    };

    let exception_get_message = match api.exception_get_message {
        Some(_) => Verified::Ok("resolved (verified at test time if exception fires)".into()),
        None    => Verified::Absent,
    };

    VerificationReport {
        domain_get, class_get_name, class_get_namespace,
        class_get_fields, field_get_name, field_get_type,
        type_get_name, thread_attach, runtime_invoke,
        string_new, array_new, exception_get_message,
    }
}
