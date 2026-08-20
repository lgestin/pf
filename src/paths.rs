use crate::error::{PfError, Result};
use std::path::PathBuf;

pub fn base_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| PfError::Other("Cannot find home directory".into()))?;
    Ok(home.join(".pf"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(base_dir()?.join("config.toml"))
}

pub fn run_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("run"))
}

pub fn log_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("logs"))
}

pub fn log_file(name: &str) -> Result<PathBuf> {
    Ok(log_dir()?.join(format!("{name}.log")))
}

/// Upper bound on a sanitized host key. Chosen so that
/// `<home>/.pf/run/<key>.desired.json` stays comfortably inside the ~104-byte
/// `sun_path` limit that `ControlPath` inherits on macOS.
pub const MAX_HOST_KEY: usize = 48;

/// FNV-1a, 64-bit, as lowercase hex.
///
/// Hand-rolled rather than `DefaultHasher` on purpose: `DefaultHasher`'s
/// output is explicitly not stable across Rust releases, so a truncated host
/// key derived from it would change under the user's feet on a toolchain
/// upgrade and orphan the live control socket it names.
fn fnv1a_hex(s: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Turn an arbitrary SSH host alias into a safe, bounded filename component.
pub fn sanitize_host(host: &str) -> String {
    let safe: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();

    if safe.len() <= MAX_HOST_KEY {
        return safe;
    }

    // Hash the *original* host so distinct inputs stay distinct even when
    // their sanitized prefixes are identical.
    let suffix = &fnv1a_hex(host)[..8];
    let keep = MAX_HOST_KEY - suffix.len() - 1;
    format!("{}-{}", &safe[..keep], suffix)
}

pub fn desired_file_in(run: &std::path::Path, host: &str) -> PathBuf {
    run.join(format!("{host}.desired.json"))
}

pub fn state_file_in(run: &std::path::Path, host: &str) -> PathBuf {
    run.join(format!("{host}.state.json"))
}

pub fn lock_file_in(run: &std::path::Path, host: &str) -> PathBuf {
    run.join(format!("{host}.lock"))
}

pub fn socket_file_in(run: &std::path::Path, host: &str) -> PathBuf {
    run.join(format!("{host}.sock"))
}

pub fn session_log_file(host: &str) -> Result<PathBuf> {
    Ok(log_dir()?.join(format!("{host}.log")))
}

pub fn ensure_dirs() -> Result<()> {
    std::fs::create_dir_all(run_dir()?)?;
    std::fs::create_dir_all(log_dir()?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_files_are_distinct_and_correctly_suffixed() {
        let run = std::path::Path::new("/tmp/pf-run");

        assert_eq!(desired_file_in(run, "gpu-01"), run.join("gpu-01.desired.json"));
        assert_eq!(state_file_in(run, "gpu-01"), run.join("gpu-01.state.json"));
        assert_eq!(lock_file_in(run, "gpu-01"), run.join("gpu-01.lock"));
        assert_eq!(socket_file_in(run, "gpu-01"), run.join("gpu-01.sock"));
    }

    #[test]
    fn ordinary_hosts_pass_through_unchanged() {
        assert_eq!(sanitize_host("gpu-01"), "gpu-01");
        assert_eq!(sanitize_host("build.example.com"), "build.example.com");
        assert_eq!(sanitize_host("user_box"), "user_box");
    }

    #[test]
    fn path_and_glob_characters_are_replaced() {
        // Dots survive — real hostnames are full of them. Separators do not,
        // which is what actually prevents traversal.
        assert_eq!(sanitize_host("../etc/passwd"), ".._etc_passwd");
        assert_eq!(sanitize_host("a b"), "a_b");
        assert_eq!(sanitize_host("we*rd?"), "we_rd_");
    }

    #[test]
    fn no_input_can_escape_the_run_directory() {
        for hostile in ["../etc/passwd", "/etc/passwd", "..", ".", "a/../../b", "x\0y"] {
            let key = sanitize_host(hostile);
            assert!(!key.contains('/'), "{hostile:?} kept a separator: {key:?}");
            assert!(!key.contains('\0'), "{hostile:?} kept a NUL: {key:?}");

            let run = std::path::Path::new("/tmp/pf-run");
            let path = desired_file_in(run, &key);
            assert_eq!(
                path.parent(),
                Some(run),
                "{hostile:?} escaped the run directory: {path:?}"
            );
        }
    }

    #[test]
    fn long_hosts_are_truncated_with_a_hash_suffix() {
        let long = "a".repeat(200);
        let got = sanitize_host(&long);
        assert!(got.len() <= MAX_HOST_KEY, "got {} chars: {got}", got.len());
    }

    #[test]
    fn two_different_long_hosts_do_not_collide() {
        let a = sanitize_host(&format!("{}x", "a".repeat(200)));
        let b = sanitize_host(&format!("{}y", "a".repeat(200)));
        assert_ne!(a, b, "truncation collapsed two distinct hosts");
    }

    #[test]
    fn the_hash_is_stable_across_runs_and_toolchains() {
        // FNV-1a 64-bit of "hello", lowercase hex, first 8 chars.
        // Pinned deliberately: DefaultHasher is NOT stable across Rust
        // releases, and a socket path that changes on a toolchain bump
        // would orphan live control sockets.
        assert_eq!(&fnv1a_hex("hello")[..8], "a430d846");
    }

    #[test]
    fn the_control_path_fits_in_sun_path() {
        let run = std::path::Path::new("/Users/someone-with-a-long-name/.pf/run");
        let sock = socket_file_in(run, &sanitize_host(&"a".repeat(200)));
        assert!(
            sock.as_os_str().len() < 104,
            "control path is {} bytes: {:?}",
            sock.as_os_str().len(),
            sock
        );
    }
}
