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
