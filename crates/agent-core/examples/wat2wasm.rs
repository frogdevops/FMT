//! Workspace-local wat→wasm compiler using the `wat` crate (already a
//! dev-dependency of agent-core). Replaces the system `wat2wasm` CLI which
//! isn't installed on this machine. Persists across compactions via
//! `~/.claude/projects/.../memory/wat-wasm-pipeline.md`.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: wat2wasm <input.wat> <output.wasm>");
        std::process::exit(2);
    }
    let bytes = wat::parse_file(&args[1]).unwrap_or_else(|e| {
        eprintln!("parse error: {}", e);
        std::process::exit(1);
    });
    std::fs::write(&args[2], &bytes).unwrap_or_else(|e| {
        eprintln!("write error: {}", e);
        std::process::exit(1);
    });
    println!("compiled {} -> {} ({} bytes)", args[1], args[2], bytes.len());
}
