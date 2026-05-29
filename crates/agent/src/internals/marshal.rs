//! Marshalling layer: bridges the script-visible `InvokeArg` vocabulary with
//! il2cpp's `void**` boxed-arg convention and (later) the universal-shim
//! `RegArgs` layout. Per-type table is small: primitives + instance + null in
//! this module's first cut; string/struct/array add in a follow-up.

use std::ffi::c_void;

use agent_core::mem_value::{Value, ValType, valtype_from_tc};
use agent_core::spine::{InvokeArg, InvokeError, Instance, MethodPtr};

fn utf8_to_cstring(s: &str) -> std::ffi::CString {
    std::ffi::CString::new(s).unwrap_or_else(|_| std::ffi::CString::new("").unwrap())
}

fn read_il2cpp_string(ptr: *const c_void) -> Option<String> {
    if ptr.is_null() { return None; }
    let p = ptr as usize;
    let len = cache::read_u32(p + 0x10)? as usize;
    if len > 8192 { return None; }
    let mut chars: Vec<u16> = Vec::with_capacity(len);
    for i in 0..len {
        let c = cache::read_u16(p + 0x14 + i * 2)?;
        chars.push(c);
    }
    Some(String::from_utf16_lossy(&chars))
}

use crate::external::cache;
use crate::internals::ctx;

/// Cached, structurally-read method signature: arg ValTypes + return ValType.
/// Reads via the probed offsets in cfg; never hardcoded.
#[derive(Debug, Clone)]
pub struct MethodSignature {
    pub param_types:  Vec<ValType>,
    pub return_type:  ValType,
    pub return_tc:    u8,         // raw IL2CPP_TYPE_* code; distinguishes value vs reference types
    pub is_static:    bool,
}

const METHOD_ATTRIBUTE_STATIC: u32 = 0x0010;

/// Read the signature of `method` by walking ParamInfo + return Il2CppType.
/// Returns NotFound if the method ptr is unreadable.
pub fn read_signature(method: MethodPtr) -> Result<MethodSignature, InvokeError> {
    let c = ctx::get().ok_or(InvokeError::InternalFailure("internals ctx not initialized"))?;
    let m = method.as_u64() as usize;

    let flags = cache::read_u32(m + c.cfg.method_flags_off)
        .ok_or(InvokeError::NotFound)?;
    let is_static = flags & METHOD_ATTRIBUTE_STATIC != 0;

    let param_count = cache::read_u8(m + c.cfg.method_param_count_off)
        .ok_or(InvokeError::NotFound)? as usize;

    let params_ptr = cache::read_u64(m + c.cfg.method_parameters_off)
        .ok_or(InvokeError::NotFound)? as usize;

    let mut param_types = Vec::with_capacity(param_count);
    for i in 0..param_count {
        let pi = params_ptr + i * c.cfg.param_info_size;
        let type_ptr = cache::read_u64(pi + c.cfg.param_info_type_off).unwrap_or(0) as usize;
        let chunk = cache::read_u64(type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
        let tc = ((chunk >> c.cfg.discrim_shift) & 0xFF) as u8;
        let vt = valtype_from_tc(tc).unwrap_or(ValType::U64);
        param_types.push(vt);
    }

    let ret_type_ptr = cache::read_u64(m + c.cfg.method_return_type_off).unwrap_or(0) as usize;
    let ret_chunk = cache::read_u64(ret_type_ptr + c.cfg.il2cpp_type_discrim_read_at).unwrap_or(0);
    let ret_tc = ((ret_chunk >> c.cfg.discrim_shift) & 0xFF) as u8;
    let return_type = valtype_from_tc(ret_tc).unwrap_or(ValType::U64);

    Ok(MethodSignature { param_types, return_type, return_tc: ret_tc, is_static })
}

/// Stable storage for one invoke call's args. Slabs are owned Vec<u8>; ptrs into
/// them are valid until the InvokeContext is dropped.
pub(crate) struct InvokeContext {
    pub args_ptrs: Vec<*mut c_void>,
    pub arg_slabs: Vec<Vec<u8>>,
}

impl InvokeContext {
    pub fn new() -> Self {
        Self { args_ptrs: Vec::new(), arg_slabs: Vec::new() }
    }

    /// Pack ONE argument into the context. Updates args_ptrs to point at the
    /// stable storage.
    ///
    /// Supports: primitives + Instance + Null in this first cut. String / Struct /
    /// Array come in the follow-up task.
    pub fn pack(&mut self, idx: u8, arg: &InvokeArg) -> Result<(), InvokeError> {
        match arg {
            InvokeArg::Prim(v) => {
                let bytes = v.encode();
                self.arg_slabs.push(bytes);
                let ptr = self.arg_slabs.last_mut().unwrap().as_mut_ptr() as *mut c_void;
                self.args_ptrs.push(ptr);
                Ok(())
            }
            InvokeArg::Instance(h) => {
                // Instance is already a pointer — store it as a void* directly.
                // We still slab the u64 so args_ptrs entries are all heap-stable.
                let bytes = h.as_u64().to_le_bytes().to_vec();
                self.arg_slabs.push(bytes);
                let ptr = self.arg_slabs.last_mut().unwrap().as_mut_ptr() as *mut c_void;
                self.args_ptrs.push(ptr);
                Ok(())
            }
            InvokeArg::Null => {
                self.args_ptrs.push(std::ptr::null_mut());
                Ok(())
            }
            InvokeArg::String(s) => {
                let c = ctx::get().ok_or(InvokeError::InternalFailure("ctx"))?;
                let string_new = c.api.string_new.ok_or(InvokeError::MarshalFailed {
                    idx,
                    reason: "string_new FFI not available on this build",
                })?;
                let cstr = utf8_to_cstring(s);
                let il2_str = unsafe { (string_new)(cstr.as_ptr()) };
                if il2_str.is_null() {
                    return Err(InvokeError::MarshalFailed { idx, reason: "string_new returned null" });
                }
                // The Il2CppString* IS the void* arg. Slab the u64 so args_ptrs entries
                // are heap-stable.
                let bytes = (il2_str as u64).to_le_bytes().to_vec();
                self.arg_slabs.push(bytes);
                let ptr = self.arg_slabs.last_mut().unwrap().as_mut_ptr() as *mut c_void;
                self.args_ptrs.push(ptr);
                Ok(())
            }
            InvokeArg::Struct(bytes) => {
                self.arg_slabs.push(bytes.clone());
                let ptr = self.arg_slabs.last_mut().unwrap().as_mut_ptr() as *mut c_void;
                self.args_ptrs.push(ptr);
                Ok(())
            }
            InvokeArg::Array(_elems) => {
                Err(InvokeError::MarshalFailed {
                    idx,
                    reason: "Array arg marshalling deferred to tier-2",
                })
            }
        }
    }
}

/// Unpack a return value pointer into an InvokeArg. First cut: primitives only.
/// Returns InvokeArg::Null for void-typed returns (signature says Void/U64 with
/// no slot, caller can tag).
pub fn unpack_return(return_type: ValType, return_tc: u8, ret_ptr: *mut c_void) -> Result<InvokeArg, InvokeError> {
    if ret_ptr.is_null() {
        return Ok(InvokeArg::Null);
    }
    // il2cpp_runtime_invoke returns VALUE TYPES as a boxed Il2CppObject (the value
    // sits at offset 0x10 past the klass + monitor header). REFERENCE TYPES are
    // returned as the Il2CppObject* directly — no second boxing.
    //   value types:  tc in 0x02..=0x0D (primitives) or 0x11 (VALUETYPE struct)
    //   reference:    tc in 0x0E (string), 0x12 (class), 0x14 (array), etc.
    let is_value_type = matches!(return_tc, 0x02..=0x0D | 0x11);
    let value_ptr = if is_value_type {
        (ret_ptr as usize) + 0x10  // skip Il2CppObject header
    } else {
        ret_ptr as usize
    };
    let width = return_type.fixed_width().unwrap_or(8);
    let bytes = unsafe { std::slice::from_raw_parts(value_ptr as *const u8, width) };
    let v = Value::decode(return_type, bytes).ok_or(
        InvokeError::InternalFailure("return decode failed")
    )?;
    Ok(InvokeArg::Prim(v))
}

/// End-to-end method invocation: resolves signature, packs args, calls
/// `runtime_invoke`, checks for managed exceptions, and unpacks the return
/// value.
///
/// Returns `InvokeError::InternalFailure` if `runtime_invoke` was not
/// resolved on this build (Option is None). For `exception_get_message`
/// being absent, a fallback string is used instead — non-fatal.
pub fn invoke_method(
    method: MethodPtr,
    instance: Option<Instance>,
    args: &[InvokeArg],
) -> Result<InvokeArg, InvokeError> {
    let c = ctx::get().ok_or(InvokeError::InternalFailure("internals ctx"))?;

    // 1. Resolve runtime_invoke FFI (None on builds where the export wasn't found).
    let runtime_invoke = c.api.runtime_invoke
        .ok_or(InvokeError::InternalFailure("runtime_invoke FFI not available on this build"))?;

    // 2. Read signature (uses probed config offsets).
    let sig = read_signature(method)?;

    // 3. Arg-count check.
    if args.len() != sig.param_types.len() {
        return Err(InvokeError::ArgCountMismatch {
            expected: sig.param_types.len() as u8,
            got: args.len() as u8,
        });
    }

    // 4. Static / instance check.
    let this_ptr = match (sig.is_static, instance) {
        (true,  _)            => std::ptr::null_mut::<c_void>(),
        (false, Some(h))      => h.as_u64() as *mut c_void,
        (false, None)         => return Err(InvokeError::NullInstance),
    };

    // 5. Pack args into a stable context.
    let mut context = InvokeContext::new();
    for (i, arg) in args.iter().enumerate() {
        context.pack(i as u8, arg)?;
    }

    // 6. Call runtime_invoke with an exception out-param.
    let mut exc: *mut crate::internals::ffi::Il2CppException = std::ptr::null_mut();
    let args_ptr = if context.args_ptrs.is_empty() {
        std::ptr::null_mut()
    } else {
        context.args_ptrs.as_mut_ptr()
    };
    let ret_ptr = unsafe {
        (runtime_invoke)(
            method.as_u64() as *mut crate::internals::ffi::MethodInfo,
            this_ptr,
            args_ptr,
            &mut exc,
        )
    };

    // 7. Check for managed exception.
    if !exc.is_null() {
        let msg = match c.api.exception_get_message {
            Some(get_msg) => {
                let msg_ptr = unsafe { (get_msg)(exc) } as *const c_void;
                read_il2cpp_string(msg_ptr).unwrap_or_else(|| "<unreadable>".to_string())
            }
            None => "<exception_get_message FFI not available>".to_string(),
        };
        crate::paths::log(&format!("invoke: managed exception raw ptr={:p} msg={}", exc, msg));
        return Err(InvokeError::ManagedException(msg));
    }

    // 8. Unpack return value.
    unpack_return(sig.return_type, sig.return_tc, ret_ptr)
}

// ═══════════════════════════════════════════════════════════════════════
// Hook-side marshal — bridges captured RegArgs ↔ InvokeArg vocabulary.
// Used by dispatch_rust (regargs → args), il2cpp.hook_set_arg (args → regargs),
// il2cpp.hook_set_return / dispatcher return (value → regargs ret slot).
// ═══════════════════════════════════════════════════════════════════════

use crate::internals::hook_runtime::regargs::RegArgs;

const METHOD_ATTRIBUTE_STATIC_BIT: u32 = 0x10;

/// Convert captured RegArgs to InvokeArg[] for the wasm handler.
/// Respects the "this-shift" for instance methods (first physical reg is `this`,
/// declared args start at the next slot).
pub fn regargs_to_args(
    method: MethodPtr,
    regs: &RegArgs,
) -> Result<Vec<InvokeArg>, InvokeError> {
    let sig = read_signature(method)?;
    let reg_offset = if sig.is_static { 0 } else { 1 };
    let mut out = Vec::with_capacity(sig.param_types.len());
    for (declared_idx, vt) in sig.param_types.iter().enumerate() {
        let physical = declared_idx + reg_offset;
        let arg = read_one_arg_from_regs(*vt, physical, regs)?;
        out.push(arg);
    }
    Ok(out)
}

fn read_one_arg_from_regs(
    vt: ValType,
    physical_slot: usize,
    regs: &RegArgs,
) -> Result<InvokeArg, InvokeError> {
    use agent_core::mem_value::Value;
    // Args 5+ live on the stack; v1 limits to 4 for now (reads zero/sentinel).
    if physical_slot >= 4 {
        return Err(InvokeError::MarshalFailed {
            idx: physical_slot as u8,
            reason: "args beyond slot 4 are not captured by RegArgs in v1",
        });
    }
    let v = match vt {
        ValType::U8  => Value::U8(regs.int_args[physical_slot] as u8),
        ValType::U16 => Value::U16(regs.int_args[physical_slot] as u16),
        ValType::U32 => Value::U32(regs.int_args[physical_slot] as u32),
        ValType::U64 => Value::U64(regs.int_args[physical_slot]),
        ValType::I8  => Value::I8(regs.int_args[physical_slot] as i8),
        ValType::I16 => Value::I16(regs.int_args[physical_slot] as i16),
        ValType::I32 => Value::I32(regs.int_args[physical_slot] as i32),
        ValType::I64 => Value::I64(regs.int_args[physical_slot] as i64),
        ValType::F32 => Value::F32(regs.float_args[physical_slot] as f32),
        ValType::F64 => Value::F64(regs.float_args[physical_slot]),
        ValType::Bytes | ValType::Cstr => {
            return Err(InvokeError::MarshalFailed {
                idx: physical_slot as u8,
                reason: "variable-length arg types not directly readable from regs",
            });
        }
    };
    Ok(InvokeArg::Prim(v))
}

/// Write modified InvokeArg[] back into RegArgs (used before call_original).
pub fn args_to_regargs(
    method: MethodPtr,
    args: &[InvokeArg],
    regs: &mut RegArgs,
) -> Result<(), InvokeError> {
    let sig = read_signature(method)?;
    if args.len() != sig.param_types.len() {
        return Err(InvokeError::ArgCountMismatch {
            expected: sig.param_types.len() as u8,
            got: args.len() as u8,
        });
    }
    let reg_offset = if sig.is_static { 0 } else { 1 };
    for (declared_idx, (vt, arg)) in sig.param_types.iter().zip(args.iter()).enumerate() {
        let physical = declared_idx + reg_offset;
        write_one_arg_to_regs(*vt, arg_as_value(arg, *vt, physical)?.clone(), physical, regs)?;
    }
    Ok(())
}

fn arg_as_value<'a>(
    arg: &'a InvokeArg,
    expected: ValType,
    idx: usize,
) -> Result<&'a agent_core::mem_value::Value, InvokeError> {
    match arg {
        InvokeArg::Prim(v) if v.val_type() == expected => Ok(v),
        InvokeArg::Prim(v) => Err(InvokeError::ArgTypeMismatch {
            idx: idx as u8,
            expected,
            got: v.val_type(),
        }),
        _ => Err(InvokeError::MarshalFailed {
            idx: idx as u8,
            reason: "hook arg writeback only supports primitives in v1",
        }),
    }
}

fn write_one_arg_to_regs(
    vt: ValType,
    v: agent_core::mem_value::Value,
    physical_slot: usize,
    regs: &mut RegArgs,
) -> Result<(), InvokeError> {
    use agent_core::mem_value::Value;
    if physical_slot >= 4 {
        return Err(InvokeError::MarshalFailed {
            idx: physical_slot as u8,
            reason: "args beyond slot 4 cannot be written via RegArgs in v1",
        });
    }
    match (vt, v) {
        (ValType::U8,  Value::U8(x))  => regs.int_args[physical_slot] = x as u64,
        (ValType::U16, Value::U16(x)) => regs.int_args[physical_slot] = x as u64,
        (ValType::U32, Value::U32(x)) => regs.int_args[physical_slot] = x as u64,
        (ValType::U64, Value::U64(x)) => regs.int_args[physical_slot] = x,
        (ValType::I8,  Value::I8(x))  => regs.int_args[physical_slot] = x as i64 as u64,
        (ValType::I16, Value::I16(x)) => regs.int_args[physical_slot] = x as i64 as u64,
        (ValType::I32, Value::I32(x)) => regs.int_args[physical_slot] = x as i64 as u64,
        (ValType::I64, Value::I64(x)) => regs.int_args[physical_slot] = x as u64,
        (ValType::F32, Value::F32(x)) => regs.float_args[physical_slot] = x as f64,
        (ValType::F64, Value::F64(x)) => regs.float_args[physical_slot] = x,
        _ => return Err(InvokeError::ArgTypeMismatch {
            idx: physical_slot as u8,
            expected: vt,
            got: vt,
        }),
    }
    Ok(())
}

/// Pack a wasm-handler-supplied return value into RegArgs' ret slots.
/// The dispatcher's shim loads BOTH ret_int and ret_float into RAX+XMM0 on
/// exit — the original caller picks whichever matches the method's return
/// type. So we ALWAYS write to whichever slot matches return_type, and zero
/// the other so a wrong-type read returns 0/NaN deterministically.
///
/// Boxed-value semantics: the value travels DIRECTLY through ret_int/ret_float
/// (no Il2CppObject* box). The trampoline-handoff convention is value-by-value
/// for primitives, because we're writing to RAX/XMM0 which IS where the
/// original method would put its un-boxed return value.
pub fn pack_return_into_regargs(
    return_type: ValType,
    _return_tc: u8,
    value: &InvokeArg,
    regs: &mut RegArgs,
) -> Result<(), InvokeError> {
    use agent_core::mem_value::Value;
    regs.ret_int = 0;
    regs.ret_float = 0.0;
    if let InvokeArg::Null = value { return Ok(()); }
    let v = match value {
        InvokeArg::Prim(v) => v,
        _ => return Err(InvokeError::MarshalFailed {
            idx: 0,
            reason: "hook return only supports primitives in v1",
        }),
    };
    match (return_type, v) {
        (ValType::U8,  Value::U8(x))  => regs.ret_int = *x as u64,
        (ValType::U16, Value::U16(x)) => regs.ret_int = *x as u64,
        (ValType::U32, Value::U32(x)) => regs.ret_int = *x as u64,
        (ValType::U64, Value::U64(x)) => regs.ret_int = *x,
        (ValType::I8,  Value::I8(x))  => regs.ret_int = *x as i64 as u64,
        (ValType::I16, Value::I16(x)) => regs.ret_int = *x as i64 as u64,
        (ValType::I32, Value::I32(x)) => regs.ret_int = *x as i64 as u64,
        (ValType::I64, Value::I64(x)) => regs.ret_int = *x as u64,
        (ValType::F32, Value::F32(x)) => regs.ret_float = *x as f64,
        (ValType::F64, Value::F64(x)) => regs.ret_float = *x,
        _ => return Err(InvokeError::ArgTypeMismatch {
            idx: 0,
            expected: return_type,
            got: v.val_type(),
        }),
    }
    Ok(())
}
