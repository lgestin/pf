use std::collections::BTreeSet;
use std::path::PathBuf;

/// Parse SSH config files and return all Host entries (excluding wildcards).
pub fn parse_ssh_hosts() -> Vec<String> {
    let mut hosts = BTreeSet::new();

    // Parse ~/.ssh/config
    if let Some(home) = dirs::home_dir() {
        parse_ssh_config_file(home.join(".ssh").join("config"), &mut hosts);
    }

    // Parse /etc/ssh/ssh_config
    parse_ssh_config_file(PathBuf::from("/etc/ssh/ssh_config"), &mut hosts);

    hosts.into_iter().collect()
}

fn parse_ssh_config_file(path: PathBuf, hosts: &mut BTreeSet<String>) {
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    for line in content.lines() {
        let trimmed = line.trim();
        // Skip comments and empty lines
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Match "Host" directive (case-insensitive)
        let lower = trimmed.to_lowercase();
        if lower.starts_with("host ") && !lower.starts_with("hostname") {
            let rest = &trimmed[5..];
            for alias in rest.split_whitespace() {
                // Skip wildcard patterns
                if alias.contains('*') || alias.contains('?') || alias.contains('!') {
                    continue;
                }
                hosts.insert(alias.to_string());
            }
        }
    }
}

/// Filter hosts by a prefix for autocomplete.
pub fn hosts_matching(prefix: &str) -> Vec<String> {
    let all = parse_ssh_hosts();
    if prefix.is_empty() {
        return all;
    }
    all.into_iter()
        .filter(|h| h.starts_with(prefix))
        .collect()
}
