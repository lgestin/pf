use crate::paths;
use crate::session::{DesiredForward, ForwardObs};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// `-L`-style forward specification.
pub fn forward_spec(local_port: u16, remote_host: &str, remote_port: u16) -> String {
    format!("{local_port}:{remote_host}:{remote_port}")
}

/// Every ssh invocation the watcher makes, behind one seam so the reconcile
/// loop can be tested without a network.
///
/// Errors are `String` because they are stored verbatim in `ForwardObs.error`
/// and shown to the user.
pub trait SshControl {
    /// Is the multiplexing master alive and accepting requests?
    fn check(&self, host: &str) -> bool;
    fn forward(&self, host: &str, f: &DesiredForward) -> std::result::Result<(), String>;
    fn cancel(&self, host: &str, f: &ForwardObs) -> std::result::Result<(), String>;
    /// Ask the master to exit. Best-effort.
    fn exit(&self, host: &str);
}

pub struct RealSsh {
    pub run_dir: PathBuf,
}

impl RealSsh {
    pub fn new(run_dir: PathBuf) -> Self {
        Self { run_dir }
    }

    fn socket(&self, host: &str) -> PathBuf {
        paths::socket_file_in(&self.run_dir, &paths::sanitize_host(host))
    }

    /// The long-lived master. Carries **no** `-L`: forwards attach afterwards
    /// via `-O forward`, so one failing bind cannot kill the others. For the
    /// same reason `ExitOnForwardFailure` is deliberately absent — it belongs
    /// to the old one-process-per-forward model.
    pub fn master_command(&self, host: &str) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.args([
            "-M",
            "-S",
            &self.socket(host).to_string_lossy(),
            "-N",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ConnectTimeout=10",
            host,
        ]);
        cmd
    }

    fn control(&self, host: &str, args: &[&str]) -> std::result::Result<(), String> {
        let out = Command::new("ssh")
            .arg("-S")
            .arg(self.socket(host))
            .args(args)
            .arg(host)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("failed to run ssh: {e}"))?;

        if out.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("ssh exited with {}", out.status)
        } else {
            stderr
        })
    }
}

impl SshControl for RealSsh {
    fn check(&self, host: &str) -> bool {
        self.control(host, &["-O", "check"]).is_ok()
    }

    fn forward(&self, host: &str, f: &DesiredForward) -> std::result::Result<(), String> {
        let spec = forward_spec(f.local_port, &f.remote_host, f.remote_port);
        self.control(host, &["-O", "forward", "-L", &spec])
    }

    fn cancel(&self, host: &str, f: &ForwardObs) -> std::result::Result<(), String> {
        let spec = forward_spec(f.local_port, &f.remote_host, f.remote_port);
        self.control(host, &["-O", "cancel", "-L", &spec])
    }

    fn exit(&self, host: &str) {
        let _ = self.control(host, &["-O", "exit"]);
    }
}

#[cfg(test)]
pub struct FakeSsh {
    pub calls: std::cell::RefCell<Vec<String>>,
    pub fail_ports: std::cell::RefCell<std::collections::HashSet<u16>>,
    pub connected: std::cell::Cell<bool>,
}

#[cfg(test)]
impl FakeSsh {
    pub fn new() -> Self {
        Self {
            calls: std::cell::RefCell::new(Vec::new()),
            fail_ports: std::cell::RefCell::new(std::collections::HashSet::new()),
            connected: std::cell::Cell::new(false),
        }
    }
}

#[cfg(test)]
impl SshControl for FakeSsh {
    fn check(&self, _host: &str) -> bool {
        self.connected.get()
    }

    fn forward(&self, _host: &str, f: &DesiredForward) -> std::result::Result<(), String> {
        let spec = forward_spec(f.local_port, &f.remote_host, f.remote_port);
        self.calls.borrow_mut().push(format!("forward {spec}"));
        if self.fail_ports.borrow().contains(&f.local_port) {
            return Err(format!(
                "bind [127.0.0.1]:{}: Address already in use",
                f.local_port
            ));
        }
        Ok(())
    }

    fn cancel(&self, _host: &str, f: &ForwardObs) -> std::result::Result<(), String> {
        let spec = forward_spec(f.local_port, &f.remote_host, f.remote_port);
        self.calls.borrow_mut().push(format!("cancel {spec}"));
        Ok(())
    }

    fn exit(&self, _host: &str) {
        self.calls.borrow_mut().push("exit".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forward_spec_is_local_remotehost_remoteport() {
        assert_eq!(forward_spec(8888, "localhost", 8888), "8888:localhost:8888");
        assert_eq!(forward_spec(5432, "db.internal", 5432), "5432:db.internal:5432");
    }

    #[test]
    fn the_master_command_multiplexes_and_carries_no_forwards() {
        let ssh = RealSsh {
            run_dir: std::path::PathBuf::from("/tmp/pf-run"),
        };
        let cmd = ssh.master_command("gpu-01");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();

        assert!(args.contains(&"-M".to_string()), "master mode missing: {args:?}");
        assert!(args.contains(&"-N".to_string()), "no-command missing: {args:?}");
        assert!(
            args.iter().any(|a| a.ends_with("gpu-01.sock")),
            "control socket missing: {args:?}"
        );
        assert_eq!(args.last().unwrap(), "gpu-01", "host must be the last arg");

        // The whole point: a shared master carries no -L, so one failing bind
        // cannot tear down its neighbours.
        assert!(!args.contains(&"-L".to_string()), "master must carry no -L: {args:?}");
        assert!(
            !args.iter().any(|a| a.starts_with("ExitOnForwardFailure")),
            "ExitOnForwardFailure belongs to the old per-forward model: {args:?}"
        );
        assert!(
            args.iter().any(|a| a.starts_with("ServerAliveInterval")),
            "keepalive missing: {args:?}"
        );
    }

    #[test]
    fn the_fake_records_calls_and_can_be_told_to_fail() {
        let fake = FakeSsh::new();
        fake.connected.set(true);

        let f = crate::session::DesiredForward {
            name: "jupyter".to_string(),
            local_port: 8888,
            remote_host: "localhost".to_string(),
            remote_port: 8888,
        };
        assert!(fake.forward("gpu-01", &f).is_ok());

        fake.fail_ports.borrow_mut().insert(6006);
        let bad = crate::session::DesiredForward {
            name: "tensorboard".to_string(),
            local_port: 6006,
            remote_host: "localhost".to_string(),
            remote_port: 6006,
        };
        let err = fake.forward("gpu-01", &bad).unwrap_err();
        assert!(err.contains("6006"), "error should name the port: {err}");

        assert_eq!(
            fake.calls.borrow().as_slice(),
            ["forward 8888:localhost:8888", "forward 6006:localhost:6006"]
        );
    }
}
