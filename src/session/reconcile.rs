use crate::session::ssh::SshControl;
use crate::session::{AttachStatus, DesiredForward, DesiredSession, ForwardObs};
use chrono::Utc;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Attach(DesiredForward),
    Cancel(ForwardObs),
}

/// Diff intent against reality. Pure: no ssh, no filesystem, no clock.
///
/// Cancels are emitted before attaches for the same name, so a port change
/// releases the old bind before the new one is requested.
pub fn reconcile(desired: &DesiredSession, observed: &[ForwardObs]) -> Vec<Action> {
    let mut actions = Vec::new();

    for want in &desired.forwards {
        match observed.iter().find(|o| o.name == want.name) {
            None => actions.push(Action::Attach(want.clone())),
            Some(have) => {
                let same_spec = have.local_port == want.local_port
                    && have.remote_port == want.remote_port
                    && have.remote_host == want.remote_host;

                if !same_spec {
                    actions.push(Action::Cancel(have.clone()));
                    actions.push(Action::Attach(want.clone()));
                } else if have.status == AttachStatus::Pending {
                    actions.push(Action::Attach(want.clone()));
                }
                // Forwarded: nothing to do.
                // Failed with an unchanged spec: deliberately left alone, so a
                // permanently conflicting port cannot spin a retry hot loop.
            }
        }
    }

    for have in observed {
        if !desired.forwards.iter().any(|w| w.name == have.name) {
            actions.push(Action::Cancel(have.clone()));
        }
    }

    actions
}

/// Execute `actions` and fold the outcomes into `observed`.
///
/// Returns log lines for the caller to write. A failed attach marks only its
/// own forward; the session and its neighbours are untouched.
pub fn apply(
    actions: &[Action],
    host: &str,
    ssh: &dyn SshControl,
    observed: &mut Vec<ForwardObs>,
) -> Vec<String> {
    let mut logs = Vec::new();

    for action in actions {
        match action {
            Action::Cancel(f) => {
                match ssh.cancel(host, f) {
                    Ok(()) => logs.push(format!("cancelled {}:{}", f.local_port, f.remote_port)),
                    Err(e) => logs.push(format!("cancel of {} failed: {e}", f.local_port)),
                }
                // Drop it either way: intent says it should be gone, and a
                // cancel that failed because it was never attached is fine.
                observed.retain(|o| o.name != f.name);
            }
            Action::Attach(f) => {
                let (status, attached_at, error) = match ssh.forward(host, f) {
                    Ok(()) => {
                        logs.push(format!(
                            "attached {} -> {}:{}",
                            f.local_port, f.remote_host, f.remote_port
                        ));
                        (AttachStatus::Forwarded, Some(Utc::now()), None)
                    }
                    Err(e) => {
                        logs.push(format!("attach of {} failed: {e}", f.local_port));
                        (AttachStatus::Failed, None, Some(e))
                    }
                };

                let obs = ForwardObs {
                    name: f.name.clone(),
                    local_port: f.local_port,
                    remote_host: f.remote_host.clone(),
                    remote_port: f.remote_port,
                    status,
                    attached_at,
                    error,
                };

                match observed.iter_mut().find(|o| o.name == f.name) {
                    Some(slot) => *slot = obs,
                    None => observed.push(obs),
                }
            }
        }
    }

    logs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AttachStatus, DesiredForward, DesiredSession, ForwardObs};
    use crate::watcher::RetryPolicy;

    fn desired(forwards: &[(&str, u16)]) -> DesiredSession {
        let mut s = DesiredSession::new("gpu-01".to_string(), true, RetryPolicy::default());
        for (name, port) in forwards {
            s.upsert(DesiredForward {
                name: name.to_string(),
                local_port: *port,
                remote_host: "localhost".to_string(),
                remote_port: *port,
            });
        }
        s
    }

    fn observed(forwards: &[(&str, u16, AttachStatus)]) -> Vec<ForwardObs> {
        forwards
            .iter()
            .map(|(name, port, status)| ForwardObs {
                name: name.to_string(),
                local_port: *port,
                remote_host: "localhost".to_string(),
                remote_port: *port,
                status: *status,
                attached_at: None,
                error: None,
            })
            .collect()
    }

    fn attached(names: &[(&str, u16)]) -> Vec<ForwardObs> {
        observed(
            &names
                .iter()
                .map(|(n, p)| (*n, *p, AttachStatus::Forwarded))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn nothing_to_do_when_reality_matches_intent() {
        let actions = reconcile(&desired(&[("a", 1)]), &attached(&[("a", 1)]));
        assert!(actions.is_empty(), "expected no actions, got {actions:?}");
    }

    #[test]
    fn a_new_forward_is_attached() {
        let actions = reconcile(&desired(&[("a", 1), ("b", 2)]), &attached(&[("a", 1)]));
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::Attach(f) if f.name == "b"));
    }

    #[test]
    fn a_removed_forward_is_cancelled() {
        let actions = reconcile(&desired(&[("a", 1)]), &attached(&[("a", 1), ("b", 2)]));
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::Cancel(f) if f.name == "b"));
    }

    #[test]
    fn a_changed_port_is_cancelled_then_reattached() {
        let actions = reconcile(&desired(&[("a", 9)]), &attached(&[("a", 1)]));
        assert_eq!(actions.len(), 2, "got {actions:?}");
        // Cancel must precede attach, or the new bind races the old one.
        assert!(matches!(&actions[0], Action::Cancel(f) if f.local_port == 1));
        assert!(matches!(&actions[1], Action::Attach(f) if f.local_port == 9));
    }

    #[test]
    fn an_empty_observed_set_reattaches_everything() {
        // This is the post-reconnect case: the master died and came back with
        // nothing attached. Same function, no separate recovery path.
        let actions = reconcile(&desired(&[("a", 1), ("b", 2)]), &[]);
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().all(|a| matches!(a, Action::Attach(_))));
    }

    #[test]
    fn an_empty_desired_set_cancels_everything() {
        let actions = reconcile(&desired(&[]), &attached(&[("a", 1), ("b", 2)]));
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().all(|a| matches!(a, Action::Cancel(_))));
    }

    #[test]
    fn a_pending_forward_is_attached() {
        let actions = reconcile(
            &desired(&[("a", 1)]),
            &observed(&[("a", 1, AttachStatus::Pending)]),
        );
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::Attach(f) if f.name == "a"));
    }

    #[test]
    fn a_failed_forward_is_not_retried() {
        // Retrying a permanently-conflicting port every tick would be a hot
        // loop writing unbounded log spam. It waits for intent to change or
        // for a reconnect to reset observed state.
        let actions = reconcile(
            &desired(&[("a", 1)]),
            &observed(&[("a", 1, AttachStatus::Failed)]),
        );
        assert!(actions.is_empty(), "failed forward was retried: {actions:?}");
    }

    #[test]
    fn a_failed_forward_whose_port_changed_is_retried() {
        // Intent changed, so the user has plausibly fixed the conflict.
        let actions = reconcile(
            &desired(&[("a", 9)]),
            &observed(&[("a", 1, AttachStatus::Failed)]),
        );
        assert_eq!(actions.len(), 2, "got {actions:?}");
        assert!(matches!(&actions[0], Action::Cancel(_)));
        assert!(matches!(&actions[1], Action::Attach(f) if f.local_port == 9));
    }

    #[test]
    fn a_failed_forward_removed_from_desired_is_cancelled() {
        let actions = reconcile(&desired(&[]), &observed(&[("a", 1, AttachStatus::Failed)]));
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::Cancel(_)));
    }

    use crate::session::ssh::{FakeSsh, SshControl};

    #[test]
    fn applying_an_attach_records_it_as_attached() {
        let ssh = FakeSsh::new();
        ssh.connected.set(true);
        let d = desired(&[("a", 1)]);
        let mut obs: Vec<ForwardObs> = Vec::new();

        let actions = reconcile(&d, &obs);
        let logs = apply(&actions, "gpu-01", &ssh, &mut obs);

        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].name, "a");
        assert_eq!(obs[0].status, AttachStatus::Forwarded);
        assert!(obs[0].attached_at.is_some());
        assert!(obs[0].error.is_none());
        assert_eq!(logs.len(), 1);
        assert!(logs[0].contains("attached"), "unexpected log: {:?}", logs[0]);
    }

    #[test]
    fn a_failed_attach_is_recorded_without_poisoning_its_neighbours() {
        let ssh = FakeSsh::new();
        ssh.connected.set(true);
        ssh.fail_ports.borrow_mut().insert(2);

        let d = desired(&[("a", 1), ("b", 2)]);
        let mut obs: Vec<ForwardObs> = Vec::new();
        apply(&reconcile(&d, &obs), "gpu-01", &ssh, &mut obs);

        let a = obs.iter().find(|f| f.name == "a").unwrap();
        let b = obs.iter().find(|f| f.name == "b").unwrap();
        assert_eq!(a.status, AttachStatus::Forwarded, "healthy forward was affected");
        assert_eq!(b.status, AttachStatus::Failed);
        assert!(b.error.as_ref().unwrap().contains("Address already in use"));
    }

    #[test]
    fn a_failed_attach_is_not_retried_on_the_next_pass() {
        let ssh = FakeSsh::new();
        ssh.connected.set(true);
        ssh.fail_ports.borrow_mut().insert(1);

        let d = desired(&[("a", 1)]);
        let mut obs: Vec<ForwardObs> = Vec::new();
        apply(&reconcile(&d, &obs), "gpu-01", &ssh, &mut obs);
        let after_first = ssh.calls.borrow().len();

        apply(&reconcile(&d, &obs), "gpu-01", &ssh, &mut obs);
        assert_eq!(
            ssh.calls.borrow().len(),
            after_first,
            "a permanently failing forward was retried: {:?}",
            ssh.calls.borrow()
        );
    }

    #[test]
    fn applying_a_cancel_drops_the_forward_from_observed() {
        let ssh = FakeSsh::new();
        ssh.connected.set(true);

        let mut obs = attached(&[("a", 1), ("b", 2)]);
        let d = desired(&[("a", 1)]);
        apply(&reconcile(&d, &obs), "gpu-01", &ssh, &mut obs);

        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].name, "a");
        assert!(ssh.calls.borrow().iter().any(|c| c == "cancel 2:localhost:2"));
    }

    #[test]
    fn a_reconnect_reattaches_everything_through_the_same_path() {
        let ssh = FakeSsh::new();
        ssh.connected.set(true);
        let d = desired(&[("a", 1), ("b", 2)]);

        let mut obs: Vec<ForwardObs> = Vec::new();
        apply(&reconcile(&d, &obs), "gpu-01", &ssh, &mut obs);
        assert_eq!(obs.len(), 2);

        // Master died: observed resets. No separate recovery code path.
        obs.clear();
        apply(&reconcile(&d, &obs), "gpu-01", &ssh, &mut obs);

        assert_eq!(obs.len(), 2);
        assert!(obs.iter().all(|f| f.status == AttachStatus::Forwarded));
    }
}
