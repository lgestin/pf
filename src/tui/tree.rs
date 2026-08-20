//! The machine/forward tree: model, flattening, and selection.
//!
//! All of this is pure — no ratatui, no filesystem — so the parts that decide
//! *what* is on screen can be tested without a terminal. `ui.rs` only decides
//! how the resulting rows are painted.

use crate::session::{ForwardObs, SessionState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};

/// Where a machine row came from. Also the sort key: live sessions first, then
/// hosts you have profiles for, then the rest of `~/.ssh/config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MachineSource {
    Live,
    Profile,
    SshConfig,
}

/// Which machines the tree lists. `[tui] machine_source` in config.toml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineListMode {
    /// Every host in ~/.ssh/config, plus profile hosts and live sessions.
    #[default]
    AllHosts,
    /// Hosts with a live session or a saved profile.
    Configured,
    /// Hosts with a live session only — v0.1.5's set, one level deeper.
    Live,
}

impl MachineListMode {
    pub fn cycle(self) -> Self {
        match self {
            Self::AllHosts => Self::Configured,
            Self::Configured => Self::Live,
            Self::Live => Self::AllHosts,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AllHosts => "all hosts",
            Self::Configured => "configured",
            Self::Live => "live only",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MachineRow {
    pub host: String,
    /// `None` means idle — no watcher running for this host.
    pub session: Option<SessionState>,
    pub forwards: Vec<ForwardObs>,
    pub source: MachineSource,
}

impl MachineRow {
    pub fn is_live(&self) -> bool {
        self.session.is_some()
    }
}

/// One line of the flattened tree. Indices point into the machine list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Machine(usize),
    Forward(usize, usize),
}

/// A selection that survives a refresh, keyed by identity rather than position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sel {
    Machine(String),
    /// (host, forward name)
    Forward(String, String),
}

impl Sel {
    pub fn host(&self) -> &str {
        match self {
            Sel::Machine(h) => h,
            Sel::Forward(h, _) => h,
        }
    }
}

fn matches_filter(row: &MachineRow, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let needle = needle.to_lowercase();
    row.host.to_lowercase().contains(&needle)
        || row
            .forwards
            .iter()
            .any(|f| f.name.to_lowercase().contains(&needle))
}

/// Assemble the machine list from live sessions, saved profiles, and ssh config.
///
/// A host appears at most once, taking the strongest provenance it qualifies
/// for: a host that is both configured and live is `Live`.
pub fn build_machines(
    sessions: Vec<SessionState>,
    profile_hosts: &BTreeSet<String>,
    ssh_hosts: &[String],
    mode: MachineListMode,
    filter: &str,
) -> Vec<MachineRow> {
    let mut rows: Vec<MachineRow> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for session in sessions {
        seen.insert(session.host.clone());
        rows.push(MachineRow {
            host: session.host.clone(),
            forwards: session.forwards.clone(),
            session: Some(session),
            source: MachineSource::Live,
        });
    }

    if mode != MachineListMode::Live {
        for host in profile_hosts {
            if seen.insert(host.clone()) {
                rows.push(MachineRow {
                    host: host.clone(),
                    session: None,
                    forwards: Vec::new(),
                    source: MachineSource::Profile,
                });
            }
        }
    }

    if mode == MachineListMode::AllHosts {
        for host in ssh_hosts {
            if seen.insert(host.clone()) {
                rows.push(MachineRow {
                    host: host.clone(),
                    session: None,
                    forwards: Vec::new(),
                    source: MachineSource::SshConfig,
                });
            }
        }
    }

    rows.retain(|r| matches_filter(r, filter));
    rows.sort_by(|a, b| a.source.cmp(&b.source).then_with(|| a.host.cmp(&b.host)));
    rows
}

/// Fold state for a fresh tree: live machines open, everything else closed, so
/// `pf tui` opens showing exactly what is running.
pub fn default_expanded(machines: &[MachineRow]) -> HashSet<String> {
    machines
        .iter()
        .filter(|m| m.is_live())
        .map(|m| m.host.clone())
        .collect()
}

/// Project the tree onto the flat row list the table renders.
pub fn flatten(machines: &[MachineRow], expanded: &HashSet<String>) -> Vec<Row> {
    let mut rows = Vec::new();
    for (mi, machine) in machines.iter().enumerate() {
        rows.push(Row::Machine(mi));
        if expanded.contains(&machine.host) {
            for fi in 0..machine.forwards.len() {
                rows.push(Row::Forward(mi, fi));
            }
        }
    }
    rows
}

/// What is selected at a flat index.
pub fn sel_at(rows: &[Row], machines: &[MachineRow], idx: usize) -> Option<Sel> {
    match rows.get(idx)? {
        Row::Machine(mi) => Some(Sel::Machine(machines.get(*mi)?.host.clone())),
        Row::Forward(mi, fi) => {
            let machine = machines.get(*mi)?;
            Some(Sel::Forward(
                machine.host.clone(),
                machine.forwards.get(*fi)?.name.clone(),
            ))
        }
    }
}

/// Find `sel` again after a rebuild.
///
/// Falls back to the parent machine when a forward has disappeared — which is
/// what happens every time you stop one — and clamps if even the machine is
/// gone. Without this the 1s refresh would fling the cursor around.
pub fn resolve(rows: &[Row], machines: &[MachineRow], sel: &Sel) -> usize {
    if let Some(exact) = (0..rows.len()).find(|&i| sel_at(rows, machines, i).as_ref() == Some(sel)) {
        return exact;
    }

    // Forward vanished — which happens every time you stop one. Fall back to
    // the machine it lived under rather than jumping to the top.
    if let Some(mi) = machines.iter().position(|m| m.host == sel.host()) {
        if let Some(pos) = rows.iter().position(|r| *r == Row::Machine(mi)) {
            return pos;
        }
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AttachStatus, ForwardObs, SessionState, SessionStatus};
    use crate::watcher::RetryPolicy;

    fn obs(name: &str, port: u16) -> ForwardObs {
        ForwardObs {
            name: name.to_string(),
            local_port: port,
            remote_host: "localhost".to_string(),
            remote_port: port,
            status: AttachStatus::Attached,
            attached_at: None,
            error: None,
        }
    }

    fn live(host: &str, forwards: &[(&str, u16)]) -> SessionState {
        let mut s = SessionState::new(host.to_string(), 1, true, RetryPolicy::default());
        s.status = SessionStatus::Connected;
        s.forwards = forwards.iter().map(|(n, p)| obs(n, *p)).collect();
        s
    }

    fn profiles(hosts: &[&str]) -> BTreeSet<String> {
        hosts.iter().map(|h| h.to_string()).collect()
    }

    fn hosts(v: &[&str]) -> Vec<String> {
        v.iter().map(|h| h.to_string()).collect()
    }

    #[test]
    fn all_hosts_mode_unions_every_source() {
        let m = build_machines(
            vec![live("gpu-01", &[("a", 1)])],
            &profiles(&["bastion"]),
            &hosts(&["nas", "gpu-01"]),
            MachineListMode::AllHosts,
            "",
        );
        let names: Vec<&str> = m.iter().map(|r| r.host.as_str()).collect();
        assert_eq!(names, vec!["gpu-01", "bastion", "nas"]);
    }

    #[test]
    fn a_host_that_is_both_configured_and_live_appears_once_as_live() {
        let m = build_machines(
            vec![live("gpu-01", &[])],
            &profiles(&["gpu-01"]),
            &hosts(&["gpu-01"]),
            MachineListMode::AllHosts,
            "",
        );
        assert_eq!(m.len(), 1, "host duplicated: {:?}", m.iter().map(|r| &r.host).collect::<Vec<_>>());
        assert_eq!(m[0].source, MachineSource::Live);
    }

    #[test]
    fn configured_mode_drops_bare_ssh_config_hosts() {
        let m = build_machines(
            vec![live("gpu-01", &[])],
            &profiles(&["bastion"]),
            &hosts(&["nas"]),
            MachineListMode::Configured,
            "",
        );
        let names: Vec<&str> = m.iter().map(|r| r.host.as_str()).collect();
        assert_eq!(names, vec!["gpu-01", "bastion"]);
    }

    #[test]
    fn live_mode_shows_only_running_sessions() {
        let m = build_machines(
            vec![live("gpu-01", &[])],
            &profiles(&["bastion"]),
            &hosts(&["nas"]),
            MachineListMode::Live,
            "",
        );
        let names: Vec<&str> = m.iter().map(|r| r.host.as_str()).collect();
        assert_eq!(names, vec!["gpu-01"]);
    }

    #[test]
    fn live_machines_sort_above_configured_above_idle() {
        let m = build_machines(
            vec![live("zzz-live", &[])],
            &profiles(&["aaa-profile"]),
            &hosts(&["aaa-idle"]),
            MachineListMode::AllHosts,
            "",
        );
        let names: Vec<&str> = m.iter().map(|r| r.host.as_str()).collect();
        // Alphabetically zzz-live would be last; provenance wins.
        assert_eq!(names, vec!["zzz-live", "aaa-profile", "aaa-idle"]);
    }

    #[test]
    fn the_filter_matches_host_or_forward_name() {
        let sessions = vec![live("gpu-01", &[("jupyter", 8888)])];
        let all = || sessions.clone();

        let by_host = build_machines(all(), &profiles(&[]), &hosts(&["nas"]), MachineListMode::AllHosts, "gpu");
        assert_eq!(by_host.len(), 1);
        assert_eq!(by_host[0].host, "gpu-01");

        // A forward name should surface its machine even when the host does not match.
        let by_forward = build_machines(all(), &profiles(&[]), &hosts(&["nas"]), MachineListMode::AllHosts, "jupy");
        assert_eq!(by_forward.len(), 1);
        assert_eq!(by_forward[0].host, "gpu-01");

        let by_nothing = build_machines(all(), &profiles(&[]), &hosts(&["nas"]), MachineListMode::AllHosts, "zzzz");
        assert!(by_nothing.is_empty());
    }

    #[test]
    fn the_filter_is_case_insensitive() {
        let m = build_machines(
            vec![live("GPU-01", &[])],
            &profiles(&[]),
            &hosts(&[]),
            MachineListMode::AllHosts,
            "gpu",
        );
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn flattening_hides_forwards_under_collapsed_machines() {
        let machines = build_machines(
            vec![live("gpu-01", &[("a", 1), ("b", 2)])],
            &profiles(&[]),
            &hosts(&[]),
            MachineListMode::AllHosts,
            "",
        );

        let collapsed = flatten(&machines, &HashSet::new());
        assert_eq!(collapsed, vec![Row::Machine(0)]);

        let expanded: HashSet<String> = ["gpu-01".to_string()].into_iter().collect();
        assert_eq!(
            flatten(&machines, &expanded),
            vec![Row::Machine(0), Row::Forward(0, 0), Row::Forward(0, 1)]
        );
    }

    #[test]
    fn live_machines_start_expanded_and_idle_ones_do_not() {
        let machines = build_machines(
            vec![live("gpu-01", &[("a", 1)])],
            &profiles(&["bastion"]),
            &hosts(&[]),
            MachineListMode::AllHosts,
            "",
        );
        let exp = default_expanded(&machines);
        assert!(exp.contains("gpu-01"), "live machine should open");
        assert!(!exp.contains("bastion"), "idle machine should stay closed");
    }

    #[test]
    fn selection_survives_a_rebuild_at_a_different_position() {
        let before = build_machines(
            vec![live("gpu-01", &[("a", 1)]), live("gpu-02", &[("b", 2)])],
            &profiles(&[]),
            &hosts(&[]),
            MachineListMode::AllHosts,
            "",
        );
        let exp = default_expanded(&before);
        let rows_before = flatten(&before, &exp);

        // gpu-02's forward "b" sits at index 3: [m0, f0, m1, f1]
        let sel = sel_at(&rows_before, &before, 3).unwrap();
        assert_eq!(sel, Sel::Forward("gpu-02".to_string(), "b".to_string()));

        // gpu-01 gains a forward, pushing everything down one row.
        let after = build_machines(
            vec![live("gpu-01", &[("a", 1), ("a2", 3)]), live("gpu-02", &[("b", 2)])],
            &profiles(&[]),
            &hosts(&[]),
            MachineListMode::AllHosts,
            "",
        );
        let rows_after = flatten(&after, &exp);
        assert_eq!(resolve(&rows_after, &after, &sel), 4, "selection did not follow its forward");
    }

    #[test]
    fn a_vanished_forward_falls_back_to_its_machine() {
        let before = build_machines(
            vec![live("gpu-01", &[("a", 1), ("b", 2)])],
            &profiles(&[]),
            &hosts(&[]),
            MachineListMode::AllHosts,
            "",
        );
        let exp = default_expanded(&before);
        let sel = Sel::Forward("gpu-01".to_string(), "b".to_string());

        // "b" is stopped.
        let after = build_machines(
            vec![live("gpu-01", &[("a", 1)])],
            &profiles(&[]),
            &hosts(&[]),
            MachineListMode::AllHosts,
            "",
        );
        let rows_after = flatten(&after, &exp);
        assert_eq!(
            resolve(&rows_after, &after, &sel),
            0,
            "should fall back to the gpu-01 machine row"
        );
        let _ = before;
    }

    #[test]
    fn a_vanished_machine_clamps_to_the_top() {
        let after = build_machines(
            vec![live("gpu-02", &[])],
            &profiles(&[]),
            &hosts(&[]),
            MachineListMode::AllHosts,
            "",
        );
        let rows = flatten(&after, &HashSet::new());
        let sel = Sel::Machine("gone".to_string());
        assert_eq!(resolve(&rows, &after, &sel), 0);
    }

    #[test]
    fn an_empty_tree_resolves_without_panicking() {
        let machines: Vec<MachineRow> = Vec::new();
        let rows = flatten(&machines, &HashSet::new());
        assert!(rows.is_empty());
        assert_eq!(resolve(&rows, &machines, &Sel::Machine("x".into())), 0);
        assert!(sel_at(&rows, &machines, 0).is_none());
    }

    #[test]
    fn the_list_mode_cycles_through_all_three() {
        let m = MachineListMode::AllHosts;
        assert_eq!(m.cycle(), MachineListMode::Configured);
        assert_eq!(m.cycle().cycle(), MachineListMode::Live);
        assert_eq!(m.cycle().cycle().cycle(), MachineListMode::AllHosts);
    }
}
