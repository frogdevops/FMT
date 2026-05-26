use std::ffi::c_void;
use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, VirtualProtect, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
    PAGE_EXECUTE_READWRITE,
};
use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

#[derive(Debug)]
pub struct Hook {
    pub target: usize,
    pub detour: usize,
    pub trampoline: usize,
    pub original_bytes: Vec<u8>,
}

impl Hook {
    #[allow(dead_code)]
    pub fn trampoline_ptr(&self) -> usize {
        self.trampoline
    }

    pub unsafe fn remove(&mut self) {
        if self.target == 0 {
            return;
        }

        let len = self.original_bytes.len();
        let mut old_protect = 0u32;
        if VirtualProtect(self.target as *mut c_void, len, PAGE_EXECUTE_READWRITE, &mut old_protect) != 0 {
            std::ptr::copy_nonoverlapping(self.original_bytes.as_ptr(), self.target as *mut u8, len);
            let mut temp = 0u32;
            VirtualProtect(self.target as *mut c_void, len, old_protect, &mut temp);

            let process = GetCurrentProcess();
            FlushInstructionCache(process, self.target as *const c_void, len);
        }

        if self.trampoline != 0 {
            VirtualFree(self.trampoline as *mut c_void, 0, MEM_RELEASE);
            self.trampoline = 0;
        }

        self.target = 0;
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe {
            self.remove();
        }
    }
}

#[allow(dead_code)]
pub unsafe fn install(target: usize, detour: usize) -> Option<Hook> {
    // 1. Decode target instructions until we have >= 12 bytes
    let mut code_bytes = [0u8; 32];
    std::ptr::copy_nonoverlapping(target as *const u8, code_bytes.as_mut_ptr(), 32);

    let mut stolen_len = 0;
    let mut decoder = iced_x86::Decoder::with_ip(64, &code_bytes, target as u64, iced_x86::DecoderOptions::NONE);
    for instr in &mut decoder {
        stolen_len += instr.len();
        if stolen_len >= 12 {
            break;
        }
    }

    if stolen_len < 12 {
        crate::paths::log(&format!(
            "Hook failed: not enough bytes to steal at {:#x} (only got {})",
            target, stolen_len
        ));
        return None;
    }

    // 2. Allocate trampoline buffer
    let tramp_size = stolen_len + 14;
    let trampoline = VirtualAlloc(
        std::ptr::null(),
        tramp_size,
        MEM_COMMIT | MEM_RESERVE,
        PAGE_EXECUTE_READWRITE,
    );
    if trampoline.is_null() {
        crate::paths::log(&format!("Hook failed: VirtualAlloc trampoline failed for {:#x}", target));
        return None;
    }

    // 3. Write stolen bytes to trampoline
    std::ptr::copy_nonoverlapping(target as *const u8, trampoline as *mut u8, stolen_len);

    // 4. Write absolute jump back in trampoline: jmp [rip + 0] <target + stolen_len>
    let jmp_back_addr = target + stolen_len;
    let tramp_jmp_ptr = (trampoline as usize + stolen_len) as *mut u8;
    // FF 25 00 00 00 00
    std::ptr::write_unaligned(tramp_jmp_ptr.add(0), 0xFF);
    std::ptr::write_unaligned(tramp_jmp_ptr.add(1), 0x25);
    std::ptr::write_unaligned(tramp_jmp_ptr.add(2), 0x00);
    std::ptr::write_unaligned(tramp_jmp_ptr.add(3), 0x00);
    std::ptr::write_unaligned(tramp_jmp_ptr.add(4), 0x00);
    std::ptr::write_unaligned(tramp_jmp_ptr.add(5), 0x00);
    // <8-byte address>
    std::ptr::write_unaligned(tramp_jmp_ptr.add(6) as *mut usize, jmp_back_addr);

    // 5. Read and save original bytes at target
    let mut original_bytes = vec![0u8; stolen_len];
    std::ptr::copy_nonoverlapping(target as *const u8, original_bytes.as_mut_ptr(), stolen_len);

    // 6. Write detour jump to target (mov rax, detour; jmp rax)
    let mut old_protect = 0u32;
    if VirtualProtect(target as *mut c_void, stolen_len, PAGE_EXECUTE_READWRITE, &mut old_protect) == 0 {
        crate::paths::log(&format!("Hook failed: VirtualProtect target failed for {:#x}", target));
        VirtualFree(trampoline, 0, MEM_RELEASE);
        return None;
    }

    // Write mov rax, detour
    std::ptr::write_unaligned(target as *mut u8, 0x48);
    std::ptr::write_unaligned((target + 1) as *mut u8, 0xB8);
    std::ptr::write_unaligned((target + 2) as *mut usize, detour);
    // Write jmp rax
    std::ptr::write_unaligned((target + 10) as *mut u8, 0xFF);
    std::ptr::write_unaligned((target + 11) as *mut u8, 0xE0);

    // Fill remaining stolen bytes with NOPs (0x90) if any
    for i in 12..stolen_len {
        std::ptr::write_unaligned((target + i) as *mut u8, 0x90);
    }

    // Restore protection
    let mut temp = 0u32;
    VirtualProtect(target as *mut c_void, stolen_len, old_protect, &mut temp);

    // Flush cache
    let process = GetCurrentProcess();
    FlushInstructionCache(process, target as *const c_void, stolen_len);
    FlushInstructionCache(process, trampoline, tramp_size);

    Some(Hook {
        target,
        detour,
        trampoline: trampoline as usize,
        original_bytes,
    })
}
