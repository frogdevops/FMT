# M0 — Il2Cpp Internals `.txt` Dumper DLL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust Windows DLL that, when loaded into a running Il2Cpp Unity game, reads the game's type metadata (classes + fields) via the il2cpp C API and dumps it to a readable `internals.txt` — the proof that the agent works.

**Architecture:** A Cargo workspace with two crates. `agent-core` holds all pure, testable logic (data model, text formatting, the dump pipeline behind an `Il2CppRuntime` trait, the respect-gate decision, file writing) and is unit-tested natively on Linux. `agent` is the `cdylib` glue: it resolves the il2cpp exports from `GameAssembly.dll`, implements `Il2CppRuntime` for real, enumerates loaded modules for the respect gate, and runs the dump from `DllMain`. The Windows-only code is `#[cfg(target_os = "windows")]`-gated so the workspace still builds and tests natively on Linux; the DLL itself is cross-compiled to `x86_64-pc-windows-gnu`.

**Tech Stack:** Rust (edition 2021), `windows-sys` for Win32 FFI, `tempfile` (dev-only) for tests, cross-compiled with the `x86_64-pc-windows-gnu` target + mingw-w64.

**Out of scope for M0:** sockets, the wire protocol, the JetBrains plugin, the injector, reading live *instance* values (M0 dumps type *structure* — namespaces, class names, field names + types — which proves the API works and names are readable). Reading instance values requires instance discovery and belongs to a later plan.

---

### Task 1: Cargo workspace + crate skeletons

**Files:**
- Modify: `Cargo.toml` (convert package → workspace)
- Delete: `src/main.rs`, `src/` (the old Frog binary scaffold)
- Create: `crates/agent-core/Cargo.toml`
- Create: `crates/agent-core/src/lib.rs`
- Create: `crates/agent/Cargo.toml`
- Create: `crates/agent/src/lib.rs`

- [ ] **Step 1: Replace the root `Cargo.toml` with a workspace manifest**

```toml
[workspace]
resolver = "2"
members = ["crates/agent-core", "crates/agent"]
```

- [ ] **Step 2: Remove the old binary scaffold and ignore IDE/build dirs**

The repo has no commits yet and `src/` is untracked, so use a plain filesystem remove (not `git rm`):
```bash
rm -rf src
printf '/target\n/.idea\n' > .gitignore
```
Expected: `src/main.rs` is gone (the workspace defines no root package, so the old `Frog` binary is removed), and `.gitignore` ignores build output and the IDE folder.

- [ ] **Step 3: Create the `agent-core` library crate**

`crates/agent-core/Cargo.toml`:
```toml
[package]
name = "agent-core"
version = "0.1.0"
edition = "2021"

[dependencies]

[dev-dependencies]
tempfile = "3"
```

`crates/agent-core/src/lib.rs`:
```rust
pub mod model;
pub mod format;
pub mod runtime;
pub mod dump;
pub mod respect;
pub mod logfile;
```

(The referenced modules are created in later tasks. To compile now, create each as an empty file.)

Run:
```bash
mkdir -p crates/agent-core/src
touch crates/agent-core/src/model.rs crates/agent-core/src/format.rs crates/agent-core/src/runtime.rs crates/agent-core/src/dump.rs crates/agent-core/src/respect.rs crates/agent-core/src/logfile.rs
```

- [ ] **Step 4: Create the `agent` cdylib crate**

`crates/agent/Cargo.toml`:
```toml
[package]
name = "agent"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
agent-core = { path = "../agent-core" }

[target.'cfg(target_os = "windows")'.dependencies]
windows-sys = { version = "0.59", features = [
    "Win32_Foundation",
    "Win32_System_LibraryLoader",
    "Win32_System_Threading",
    "Win32_System_Diagnostics_ToolHelp",
] }
```

`crates/agent/src/lib.rs`:
```rust
// Windows-only agent glue lives behind cfg gates so the workspace
// still builds and tests natively on non-Windows hosts.
#[cfg(target_os = "windows")]
mod il2cpp_ffi;
#[cfg(target_os = "windows")]
mod real_runtime;
#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "windows")]
mod entry;
```

- [ ] **Step 5: Verify the workspace builds and tests cleanly on Linux**

Run:
```bash
cargo build
cargo test
```
Expected: both succeed. `agent` compiles to an empty cdylib on Linux (all glue is cfg-gated out); `agent-core` has no tests yet.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "chore: convert Frog scaffold to agent workspace"
```

---

### Task 2: Core data model + text formatter

**Files:**
- Create/modify: `crates/agent-core/src/model.rs`
- Create/modify: `crates/agent-core/src/format.rs`
- Test: in `crates/agent-core/src/format.rs` (`#[cfg(test)]` module)

- [ ] **Step 1: Write the data model**

`crates/agent-core/src/model.rs`:
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpedField {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpedClass {
    pub namespace: String,
    pub name: String,
    pub fields: Vec<DumpedField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dump {
    pub classes: Vec<DumpedClass>,
}

impl Dump {
    pub fn class_count(&self) -> usize {
        self.classes.len()
    }

    pub fn total_fields(&self) -> usize {
        self.classes.iter().map(|c| c.fields.len()).sum()
    }
}
```

- [ ] **Step 2: Write the failing formatter test**

Append to `crates/agent-core/src/format.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dump, DumpedClass, DumpedField};

    #[test]
    fn formats_classes_and_fields() {
        let dump = Dump {
            classes: vec![DumpedClass {
                namespace: "Game".to_string(),
                name: "Player".to_string(),
                fields: vec![
                    DumpedField { name: "health".to_string(), type_name: "System.Int32".to_string() },
                    DumpedField { name: "name".to_string(), type_name: "System.String".to_string() },
                ],
            }],
        };

        let text = format_dump(&dump);

        let expected = "\
# Unity internals dump
# classes: 1, fields: 2

class Game.Player {
    System.Int32 health;
    System.String name;
}

";
        assert_eq!(text, expected);
    }

    #[test]
    fn omits_namespace_when_empty() {
        let dump = Dump {
            classes: vec![DumpedClass {
                namespace: String::new(),
                name: "Bare".to_string(),
                fields: vec![],
            }],
        };

        assert!(format_dump(&dump).contains("class Bare {\n}"));
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p agent-core formats_classes_and_fields`
Expected: FAIL — `cannot find function 'format_dump'`.

- [ ] **Step 4: Implement the formatter**

Prepend to `crates/agent-core/src/format.rs` (above the test module):
```rust
use crate::model::Dump;

pub fn format_dump(dump: &Dump) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Unity internals dump\n# classes: {}, fields: {}\n\n",
        dump.class_count(),
        dump.total_fields()
    ));
    for class in &dump.classes {
        let full = if class.namespace.is_empty() {
            class.name.clone()
        } else {
            format!("{}.{}", class.namespace, class.name)
        };
        out.push_str(&format!("class {} {{\n", full));
        for field in &class.fields {
            out.push_str(&format!("    {} {};\n", field.type_name, field.name));
        }
        out.push_str("}\n\n");
    }
    out
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p agent-core`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/model.rs crates/agent-core/src/format.rs
git commit -m "feat(agent-core): dump model and text formatter"
```

---

### Task 3: `Il2CppRuntime` trait + dump pipeline

**Files:**
- Create/modify: `crates/agent-core/src/runtime.rs`
- Create/modify: `crates/agent-core/src/dump.rs`
- Test: in `crates/agent-core/src/dump.rs` (`#[cfg(test)]` module)

- [ ] **Step 1: Define the runtime abstraction**

`crates/agent-core/src/runtime.rs`:
```rust
/// Raw, flat data read from the runtime — plain Rust, no FFI types,
/// so the dump pipeline can be exercised with a fake in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawField {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawClass {
    pub namespace: String,
    pub name: String,
    pub fields: Vec<RawField>,
}

/// Abstraction over the il2cpp runtime. The real implementation (in the
/// `agent` crate) calls the il2cpp C API; tests use a fake.
pub trait Il2CppRuntime {
    fn enumerate_classes(&self) -> Vec<RawClass>;
}
```

- [ ] **Step 2: Write the failing pipeline test**

Append to `crates/agent-core/src/dump.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DumpedClass, DumpedField};
    use crate::runtime::{Il2CppRuntime, RawClass, RawField};

    struct FakeRuntime {
        classes: Vec<RawClass>,
    }

    impl Il2CppRuntime for FakeRuntime {
        fn enumerate_classes(&self) -> Vec<RawClass> {
            self.classes.clone()
        }
    }

    #[test]
    fn builds_sorted_dump_skipping_compiler_generated() {
        let rt = FakeRuntime {
            classes: vec![
                RawClass { namespace: "Game".into(), name: "Player".into(), fields: vec![
                    RawField { name: "health".into(), type_name: "System.Int32".into() },
                ]},
                RawClass { namespace: String::new(), name: "<PrivateImplementationDetails>".into(), fields: vec![] },
                RawClass { namespace: "Game".into(), name: "Enemy".into(), fields: vec![] },
            ],
        };

        let dump = build_dump(&rt);

        // Compiler-generated `<...>` class is filtered out; rest sorted by (namespace, name).
        assert_eq!(dump.classes, vec![
            DumpedClass { namespace: "Game".into(), name: "Enemy".into(), fields: vec![] },
            DumpedClass { namespace: "Game".into(), name: "Player".into(), fields: vec![
                DumpedField { name: "health".into(), type_name: "System.Int32".into() },
            ]},
        ]);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p agent-core builds_sorted_dump`
Expected: FAIL — `cannot find function 'build_dump'`.

- [ ] **Step 4: Implement the pipeline**

Prepend to `crates/agent-core/src/dump.rs` (above the test module):
```rust
use crate::model::{Dump, DumpedClass, DumpedField};
use crate::runtime::Il2CppRuntime;

pub fn build_dump(rt: &dyn Il2CppRuntime) -> Dump {
    let mut classes: Vec<DumpedClass> = rt
        .enumerate_classes()
        .into_iter()
        .filter(|c| !c.name.starts_with('<')) // skip compiler-generated types
        .map(|c| DumpedClass {
            namespace: c.namespace,
            name: c.name,
            fields: c
                .fields
                .into_iter()
                .map(|f| DumpedField { name: f.name, type_name: f.type_name })
                .collect(),
        })
        .collect();

    classes.sort_by(|a, b| {
        (a.namespace.as_str(), a.name.as_str()).cmp(&(b.namespace.as_str(), b.name.as_str()))
    });

    Dump { classes }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p agent-core`
Expected: PASS (3 tests total).

- [ ] **Step 6: Commit**

```bash
git add crates/agent-core/src/runtime.rs crates/agent-core/src/dump.rs
git commit -m "feat(agent-core): Il2CppRuntime trait and dump pipeline"
```

---

### Task 4: Respect-gate decision

**Files:**
- Create/modify: `crates/agent-core/src/respect.rs`
- Test: in `crates/agent-core/src/respect.rs` (`#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

Append to `crates/agent-core/src/respect.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_when_no_protection_present() {
        let modules = vec![
            "GameAssembly.dll".to_string(),
            "UnityPlayer.dll".to_string(),
        ];
        assert_eq!(should_decline(&modules), None);
    }

    #[test]
    fn declines_on_easyanticheat_case_insensitive() {
        let modules = vec!["EasyAntiCheat.dll".to_string()];
        assert_eq!(
            should_decline(&modules),
            Some(DeclineReason::AntiCheat("EasyAntiCheat.dll".to_string()))
        );
    }

    #[test]
    fn declines_on_battleye() {
        let modules = vec!["BEClient_x64.dll".to_string()];
        assert_eq!(
            should_decline(&modules),
            Some(DeclineReason::AntiCheat("BEClient_x64.dll".to_string()))
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p agent-core respect`
Expected: FAIL — `cannot find type 'DeclineReason'` / function `should_decline`.

- [ ] **Step 3: Implement the respect gate**

Prepend to `crates/agent-core/src/respect.rs` (above the test module):
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclineReason {
    AntiCheat(String),
}

/// Module-name markers for active anti-cheat / anti-tamper. If any loaded
/// module matches, we decline out of respect — we never try to bypass it.
const ANTICHEAT_MARKERS: &[&str] = &[
    "easyanticheat",
    "battleye",
    "beclient",
    "vanguard",
    "denuvo",
    "xigncode",
];

/// Returns `Some(reason)` if the process is protected and we must not engage.
pub fn should_decline(loaded_modules: &[String]) -> Option<DeclineReason> {
    for module in loaded_modules {
        let lower = module.to_lowercase();
        if ANTICHEAT_MARKERS.iter().any(|m| lower.contains(m)) {
            return Some(DeclineReason::AntiCheat(module.clone()));
        }
    }
    None
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p agent-core`
Expected: PASS (6 tests total).

- [ ] **Step 5: Commit**

```bash
git add crates/agent-core/src/respect.rs
git commit -m "feat(agent-core): respect-gate anti-cheat decision"
```

---

### Task 5: Log + dump file writer

**Files:**
- Create/modify: `crates/agent-core/src/logfile.rs`
- Test: in `crates/agent-core/src/logfile.rs` (`#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

Append to `crates/agent-core/src/logfile.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn write_text_overwrites_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("internals.txt");

        write_text(&path, "first").unwrap();
        write_text(&path, "second").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    }

    #[test]
    fn append_log_adds_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.log");

        append_log(&path, "loaded").unwrap();
        append_log(&path, "attached").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "loaded\nattached\n");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p agent-core logfile`
Expected: FAIL — `cannot find function 'write_text'` / `append_log`.

- [ ] **Step 3: Implement the writer**

Prepend to `crates/agent-core/src/logfile.rs` (above the test module):
```rust
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

/// Overwrite `path` with `contents` (used for the internals dump).
pub fn write_text(path: &Path, contents: &str) -> io::Result<()> {
    std::fs::write(path, contents)
}

/// Append a single line (used for progress logging).
pub fn append_log(path: &Path, line: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", line)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p agent-core`
Expected: PASS (8 tests total).

- [ ] **Step 5: Commit**

```bash
git add crates/agent-core/src/logfile.rs
git commit -m "feat(agent-core): log and dump file writers"
```

---

### Task 6: il2cpp FFI declarations + resolver

> Glue task. The il2cpp C API exports stable `il2cpp_*` C functions from `GameAssembly.dll`. These signatures follow that standard API. There are no unit tests here — this code is exercised by the M0 run (Task 10). Keep it small and `#[cfg(target_os = "windows")]`-only.

**Files:**
- Create: `crates/agent/src/il2cpp_ffi.rs`

- [ ] **Step 1: Declare the function-pointer types and resolver**

`crates/agent/src/il2cpp_ffi.rs`:
```rust
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

/// Resolve a single export from GameAssembly.dll, transmuting it to the
/// given function-pointer type. Returns None if missing.
unsafe fn resolve(module: isize, name: &[u8]) -> Option<*const c_void> {
    // `name` must be a NUL-terminated byte string.
    let proc = GetProcAddress(module, name.as_ptr());
    proc.map(|p| p as *const c_void)
}

impl Il2CppApi {
    /// Resolve all needed exports from GameAssembly.dll. Returns None if the
    /// module isn't loaded or any export is missing (e.g. a hostile runtime).
    pub unsafe fn resolve_from_game_assembly() -> Option<Il2CppApi> {
        let module = GetModuleHandleA(b"GameAssembly.dll\0".as_ptr());
        if module == 0 {
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
/// Returns an empty string for null pointers.
pub unsafe fn cstr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}
```

- [ ] **Step 2: Verify it cross-compiles (host build skips it via cfg)**

Run: `cargo build`
Expected: PASS on Linux (module is cfg-gated out). Cross-compile is verified in Task 10.

- [ ] **Step 3: Commit**

```bash
git add crates/agent/src/il2cpp_ffi.rs
git commit -m "feat(agent): il2cpp FFI declarations and export resolver"
```

---

### Task 7: Real `Il2CppRuntime` implementation

> Glue task, `#[cfg(target_os = "windows")]`-only. Implements the `agent-core` trait by walking the il2cpp API: every assembly → its image → every class → its fields. Verified by the M0 run.

**Files:**
- Create: `crates/agent/src/real_runtime.rs`

- [ ] **Step 1: Implement the real runtime**

`crates/agent/src/real_runtime.rs`:
```rust
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
```

- [ ] **Step 2: Verify host build still passes**

Run: `cargo build`
Expected: PASS on Linux (cfg-gated out).

- [ ] **Step 3: Commit**

```bash
git add crates/agent/src/real_runtime.rs
git commit -m "feat(agent): real Il2CppRuntime over the il2cpp C API"
```

---

### Task 8: Loaded-module enumeration (for the respect gate)

> Glue task, `#[cfg(target_os = "windows")]`-only. Lists loaded module names via the ToolHelp snapshot API so the respect gate can check for anti-cheat. Verified by the M0 run.

**Files:**
- Create: `crates/agent/src/win.rs`

- [ ] **Step 1: Implement module enumeration**

`crates/agent/src/win.rs`:
```rust
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
};
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};

/// Return the base names of all modules loaded in the current process.
pub fn loaded_module_names() -> Vec<String> {
    let mut names = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return names;
        }

        let mut entry: MODULEENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;

        if Module32FirstW(snapshot, &mut entry) != 0 {
            loop {
                names.push(wide_to_string(&entry.szModule));
                if Module32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }

        CloseHandle(snapshot);
    }
    names
}

/// Convert a NUL-terminated UTF-16 fixed buffer into a String.
fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}
```

- [ ] **Step 2: Verify host build still passes**

Run: `cargo build`
Expected: PASS on Linux (cfg-gated out).

- [ ] **Step 3: Commit**

```bash
git add crates/agent/src/win.rs
git commit -m "feat(agent): enumerate loaded modules for respect gate"
```

---

### Task 9: `DllMain` orchestration

> Glue task, `#[cfg(target_os = "windows")]`-only. On `DLL_PROCESS_ATTACH`, spawn a worker thread (via `CreateThread` to avoid loader-lock issues) that: waits for the runtime to be ready, runs the respect gate, and either declines or dumps internals to a `.txt`. The dump and log go next to the executable's working directory as `internals.txt` and `agent.log`. Verified by the M0 run.

**Files:**
- Create: `crates/agent/src/entry.rs`

- [ ] **Step 1: Implement the entry point and worker**

`crates/agent/src/entry.rs`:
```rust
use std::os::raw::c_void;
use std::path::PathBuf;
use std::ptr;
use std::time::Duration;

use agent_core::dump::build_dump;
use agent_core::format::format_dump;
use agent_core::logfile::{append_log, write_text};
use agent_core::respect::should_decline;

use windows_sys::Win32::Foundation::{BOOL, HMODULE, TRUE};
use windows_sys::Win32::System::Threading::CreateThread;

use crate::il2cpp_ffi::Il2CppApi;
use crate::real_runtime::RealRuntime;
use crate::win::loaded_module_names;

const DLL_PROCESS_ATTACH: u32 = 1;

fn log_path() -> PathBuf {
    PathBuf::from("agent.log")
}

fn dump_path() -> PathBuf {
    PathBuf::from("internals.txt")
}

fn log(line: &str) {
    let _ = append_log(&log_path(), line);
}

/// The worker: runs off the loader lock on its own thread.
extern "system" fn worker(_param: *mut c_void) -> u32 {
    // Fresh log each run.
    let _ = write_text(&log_path(), "");
    log("agent loaded");

    // Respect gate first — never proceed on a protected process.
    let modules = loaded_module_names();
    if let Some(reason) = should_decline(&modules) {
        log(&format!("declined: protection detected ({:?})", reason));
        return 0;
    }
    log("respect gate passed");

    // Wait until the il2cpp runtime is initialized (domain available).
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
    unsafe { runtime.attach_thread() };

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

    0
}

/// Standard Windows DLL entry point.
#[no_mangle]
pub extern "system" fn DllMain(_module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        unsafe {
            CreateThread(
                ptr::null(),
                0,
                Some(worker),
                ptr::null(),
                0,
                ptr::null_mut(),
            );
        }
    }
    TRUE
}
```

- [ ] **Step 2: Verify host build still passes**

Run: `cargo build`
Expected: PASS on Linux (cfg-gated out).

- [ ] **Step 3: Commit**

```bash
git add crates/agent/src/entry.rs
git commit -m "feat(agent): DllMain worker that dumps internals.txt"
```

---

### Task 10: Cross-compile the DLL + M0 manual verification

> This task produces the actual `agent.dll` and runs the M0 integration test: inject it and confirm `internals.txt` appears with readable class names. The live read can only be verified here — there is no automated substitute.

**Files:**
- None (build + manual verification).

- [ ] **Step 1: Install the Windows cross-compilation toolchain**

Run:
```bash
rustup target add x86_64-pc-windows-gnu
```
Then ensure the mingw-w64 linker is installed (Arch/zen kernel implies Arch — adjust for your distro):
```bash
which x86_64-w64-mingw32-gcc || sudo pacman -S --needed mingw-w64-gcc
```
Expected: the target is added and `x86_64-w64-mingw32-gcc` resolves to a path.

- [ ] **Step 2: Cross-compile the agent DLL**

Run:
```bash
cargo build -p agent --target x86_64-pc-windows-gnu --release
```
Expected: PASS, producing `target/x86_64-pc-windows-gnu/release/agent.dll`.

- [ ] **Step 3: Confirm MiSide's runtime (resolves spec open question)**

Locate the install and check for Il2Cpp:
```bash
find ~/.steam ~/.local/share/Steam -iname "GameAssembly.dll" 2>/dev/null
find ~/.steam ~/.local/share/Steam -ipath "*MiSide*Managed*" -name "Assembly-CSharp.dll" 2>/dev/null
```
Expected: a `GameAssembly.dll` under MiSide = Il2Cpp (proceed). If instead an `Assembly-CSharp.dll` shows up under a `Managed` folder, the game is Mono and this DLL won't apply — stop and note it for the Mono-backend plan.

- [ ] **Step 4: Inject and run (manual)**

Get `agent.dll` loaded into the running game by whatever means is handy for this first test (an existing loader, or a Proton doorstop launch wrapper — `WINEDLLOVERRIDES` + a proxy DLL named `winhttp.dll`, with `agent.dll` loaded from it). The dedicated injector is a later plan; the only goal here is "the DLL gets loaded."

- [ ] **Step 5: Verify the M0 result**

After launching the game with the DLL loaded, check the game's working directory (under the Wine prefix, e.g. `.../MiSide.exe`'s folder):
```bash
# adjust to the actual game directory found in Step 3
cat "<game-dir>/agent.log"
cat "<game-dir>/internals.txt" | head -50
```
Expected:
- `agent.log` shows: `agent loaded` → `respect gate passed` → `il2cpp runtime ready` → `read N classes, M fields` → `wrote internals.txt`.
- `internals.txt` lists real, readable class names (e.g. `class <namespace>.SomethingController { ... }`).

**This is the M0 green light.** Readable class names in `internals.txt` = the DLL works, the il2cpp API works, and the agent can read internals. Proceed to the next plan (socket + protocol, then the plugin).

- [ ] **Step 6: Commit the build notes (optional)**

If you captured the working injection command or game path, add it to the spec's open questions or a short `crates/agent/README.md`, then:
```bash
git add -A
git commit -m "docs: record M0 injection/run notes"
```

---

## Notes for the implementer

- **The il2cpp signatures** in Task 6 follow the standard, stable il2cpp embedding C API. If a specific export is missing on the target (the resolver returns `None` and the worker logs a gave-up/decline), confirm the exact export name against the game's `GameAssembly.dll` (e.g. with `nm`/`dumpbin`-style inspection) and adjust that one line.
- **`windows-sys` handle types are version-sensitive.** Across versions, `HMODULE`/`HANDLE` are sometimes `isize` and sometimes `*mut c_void`, and `FARPROC`/`INVALID_HANDLE_VALUE` shift to match. The code uses integer comparisons (`module == 0`, `snapshot == INVALID_HANDLE_VALUE`). If the cross-compile in Task 10 errors on a type mismatch there, switch those checks to pointer form (`module.is_null()`, etc.) — or pin a known version (`windows-sys = "=0.52.0"`, where they are `isize`). This is the most likely place the cross-compile needs a one-line adjustment.
- **`il2cpp_type_get_name`** returns a heap-allocated string in some il2cpp versions. For M0 we accept the small leak (the dump runs once). A later plan can free it via `il2cpp_free` if needed.
- **Output location:** the worker writes `internals.txt`/`agent.log` to the process's current working directory, which for an injected game DLL is normally the game folder. If the files don't appear there after a run, the CWD differs — log an absolute path instead (e.g. derive from the game's known directory) as a quick adjustment.
- **Main-thread reads:** M0 reads *metadata* (classes/fields), which is safe from the attached worker thread. Reading live *instance values* (a later plan) must move onto the game's main thread under a per-frame budget, as the spec describes.
