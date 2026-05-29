//! One-shot probe (opt-in `FROG_EXPORT_DUMP`): walks the GameAssembly.dll
//! export table, follows JMP forwarding, and logs each export's name + the
//! first 32 bytes of its actual code body. Used to:
//!  - On Highrise: see the ground-truth bytes of `il2cpp_runtime_invoke`
//!    (named by symbol).
//!  - On Pixel Worlds: dump every scrambled export so we can search for the
//!    one matching the Highrise body.
//!
//! Cross-referencing yields a sig-scan pattern that works on both.

use std::ffi::CStr;

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;

use crate::paths::log;

pub fn run_export_dump_probe() {
    log("=== EXPORT DUMP PROBE ===");
    unsafe {
        let module = GetModuleHandleA(b"GameAssembly.dll\0".as_ptr());
        if module.is_null() {
            log("export dump: GameAssembly.dll not loaded");
            return;
        }
        let base_ptr = module as *const u8;

        let dos_header = &*(base_ptr as *const windows_sys::Win32::System::SystemServices::IMAGE_DOS_HEADER);
        if dos_header.e_magic != 0x5A4D { log("export dump: bad DOS magic"); return; }

        let nt_headers = &*(base_ptr.add(dos_header.e_lfanew as usize)
            as *const windows_sys::Win32::System::Diagnostics::Debug::IMAGE_NT_HEADERS64);
        if nt_headers.Signature != 0x00004550 { log("export dump: bad PE magic"); return; }

        let export_dir_entry = nt_headers.OptionalHeader.DataDirectory[0];
        if export_dir_entry.VirtualAddress == 0 {
            log("export dump: no export directory");
            return;
        }

        let export_dir = &*(base_ptr.add(export_dir_entry.VirtualAddress as usize)
            as *const windows_sys::Win32::System::SystemServices::IMAGE_EXPORT_DIRECTORY);

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

        log(&format!("export dump: {} named exports", num_names));

        for i in 0..num_names {
            let name_ptr = base_ptr.add(names_rvas[i] as usize) as *const i8;
            let name = CStr::from_ptr(name_ptr).to_string_lossy().into_owned();

            let ord = ordinals[i] as usize;
            if ord >= funcs_rvas.len() { continue; }
            let rva = funcs_rvas[ord];
            if rva == 0 { continue; }

            // Follow up to 10 levels of JMP (0xE9) forwarding.
            let mut curr = base_ptr.add(rva as usize);
            for _ in 0..10 {
                if *curr != 0xE9 { break; }
                let mut off_bytes = [0u8; 4];
                std::ptr::copy_nonoverlapping(curr.add(1), off_bytes.as_mut_ptr(), 4);
                let off = i32::from_le_bytes(off_bytes);
                curr = curr.add(5).offset(off as isize);
            }

            let code = std::slice::from_raw_parts(curr, 32);
            let hex = code.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");
            log(&format!("  {:40} {:?}  {}", name, curr, hex));
        }
    }
    log("=== end EXPORT DUMP PROBE ===");
}
