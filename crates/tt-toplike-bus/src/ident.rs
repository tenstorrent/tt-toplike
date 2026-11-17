//! The identity of the machine a bus runs on, for a reader to tell whether a
//! bus describes its own chips.
//!
//! Chip keys are PCI addresses, so two identical boxes have the same keys.
//! A socket can be forwarded from another machine, so a reader compares
//! this identity before it draws a remote world on its own chips.
//!
//! The identity is the host name, plus `/` and a 64-bit FNV-1a hash of
//! `/etc/machine-id` (16 hex digits) when that file is readable. The raw
//! machine id is never sent: it is meant to stay on the machine. FNV-1a is
//! written out here so every build of the crate computes the same hash.

use std::sync::OnceLock;

/// FNV-1a, 64 bit.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The identity built from a host name and the machine id's contents, if
/// any. Whitespace around the machine id is ignored.
pub fn identity_from(hostname: &str, machine_id: Option<&str>) -> String {
    match machine_id.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => format!("{hostname}/{:016x}", fnv1a(m.as_bytes())),
        None => hostname.to_string(),
    }
}

/// This machine's host name, or `unknown`.
fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: `buf` is a valid writable buffer of the given length;
        // gethostname writes at most that many bytes.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            if let Ok(s) = std::str::from_utf8(&buf[..end]) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// This machine's identity (see the module doc). Read once per process.
pub fn host_identity() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let mid = std::fs::read_to_string("/etc/machine-id").ok();
        identity_from(&hostname(), mid.as_deref())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_hides_the_machine_id() {
        let id = identity_from("box", Some("0123456789abcdef0123456789abcdef\n"));
        assert!(id.starts_with("box/"), "{id}");
        assert_eq!(id.len(), "box/".len() + 16);
        assert!(!id.contains("0123456789abcdef0123"));
        assert_eq!(
            id,
            identity_from("box", Some("0123456789abcdef0123456789abcdef"))
        );
        assert_ne!(id, identity_from("box", Some("ffff")));
        assert_eq!(identity_from("box", None), "box");
        assert!(!host_identity().is_empty());
    }
}
