use crate::session::{AttachStatus, DesiredForward, DesiredSession, ForwardObs};

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
                // Attached: nothing to do.
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
                .map(|(n, p)| (*n, *p, AttachStatus::Attached))
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
}
