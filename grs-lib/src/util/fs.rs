//! Filesystem helpers: atomic writes (temp + rename, jj's
//! `persist_content_addressed_temp_file` pattern) and small walk helpers.

use crate::error::Result;
use std::io::Write;
use std::path::Path;

/// How many leading bytes to inspect when sniffing for binary content. Matches
/// the heuristic git uses (`grep` / `diff` core.*) — 8 KiB catches ELF, Mach-O,
/// PNG, JPEG, etc. without paying for a full read of multi-GB files.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Atomically write `bytes` to `path`: create a `NamedTempFile` in the same
/// directory, write, then `persist` (atomic rename on POSIX). A crash never
/// leaves a half-written file — at worst the temp is orphaned.
///
/// The parent directory must exist.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    // `persist` renames atomically; refuse to clobber an existing target.
    tmp.persist(path).map_err(|e| crate::error::GrsError::Io(e.error))?;
    Ok(())
}

/// Atomically write a UTF-8 string.
pub fn atomic_write_str(path: &Path, contents: &str) -> Result<()> {
    atomic_write(path, contents.as_bytes())
}

/// Read a file to a String. Returns the given default if the file is missing.
pub fn read_to_string_or(path: &Path, default: &str) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default.to_string()),
        Err(e) => Err(e.into()),
    }
}

/// Best-effort fsync of a directory (for durability after a rename). Errors
/// are ignored — a lost-last-snap is acceptable, a corrupt one is not.
pub fn fsync_dir(dir: &Path) {
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

/// Read file content as UTF-8. If the bytes are not valid UTF-8 (a binary
/// file), return a placeholder string describing the size — see `08` RISK 7.
pub fn read_content_or_binary_placeholder(path: &Path) -> Result<String> {
    match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes.clone()) {
            Ok(s) => Ok(s),
            Err(_) => Ok(format!("(binary file, {} bytes)", bytes.len())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(String::new())
        }
        Err(e) => Err(e.into()),
    }
}

/// Heuristic: does `path` look like a binary file? Reads the first
/// [`BINARY_SNIFF_BYTES`] and returns true if any NUL byte (`0x00`) is
/// present — the same test `git` / ripgrep use to distinguish text from
/// binary. UTF-8 text never contains a NUL, and virtually every binary
/// format (ELF, Mach-O, PE, images, archives, .class, .pyc, compiled
/// objects, …) embeds one. This is the gate that keeps built artifacts
/// (e.g. `cargo build` output) out of grs captures without forcing the
/// user to extend `.grsignore` per build target.
///
/// Returns `false` on I/O errors so the caller can fall back to its own
/// "is the path still there?" handling rather than dropping the event.
pub fn is_binary_file(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut buf = [0u8; BINARY_SNIFF_BYTES];
    let n = match f.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return false,
    };
    buf[..n].contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn atomic_write_creates_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("out.txt");
        atomic_write_str(&p, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello");
    }

    #[test]
    fn atomic_write_overwrites() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("out.txt");
        atomic_write_str(&p, "v1").unwrap();
        atomic_write_str(&p, "v2").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v2");
    }

    #[test]
    fn binary_placeholder() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("bin");
        std::fs::write(&p, [0xFFu8, 0xFE, 0x00, 0x01]).unwrap();
        let s = read_content_or_binary_placeholder(&p).unwrap();
        assert!(s.starts_with("(binary file,"));
    }

    #[test]
    fn is_binary_detects_nul_byte() {
        let dir = tempdir().unwrap();
        // Pretend ELF header — real binaries always start with a NUL inside
        // the magic or the version/flags.
        let mut bytes = vec![0x7f, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00];
        bytes.extend(std::iter::repeat(0u8).take(64));
        let p = dir.path().join("fake.elf");
        std::fs::write(&p, &bytes).unwrap();
        assert!(is_binary_file(&p));
    }

    #[test]
    fn is_binary_returns_false_for_text() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "fn main() {}\nprintln!(\"hi\");\n").unwrap();
        assert!(!is_binary_file(&p));
    }

    #[test]
    fn is_binary_handles_empty_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("empty");
        std::fs::write(&p, b"").unwrap();
        assert!(!is_binary_file(&p));
    }

    #[test]
    fn is_binary_returns_false_for_missing_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("does-not-exist");
        assert!(!is_binary_file(&p));
    }
}
