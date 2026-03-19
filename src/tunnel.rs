use std::process::{Command, Stdio};

pub struct TunnelParams {
    pub host: String,
    pub local_port: u16,
    pub remote_port: u16,
    pub remote_host: String,
}

impl TunnelParams {
    pub fn ssh_command(&self) -> Command {
        let forward = format!(
            "{}:{}:{}",
            self.local_port, self.remote_host, self.remote_port
        );

        let mut cmd = Command::new("ssh");
        cmd.args([
            "-N",
            "-L",
            &forward,
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ConnectTimeout=10",
            &self.host,
        ]);
        cmd
    }

    pub fn spawn(&self, log_file: std::fs::File) -> std::io::Result<std::process::Child> {
        let log_err = log_file.try_clone()?;
        self.ssh_command()
            .stdin(Stdio::null())
            .stdout(log_file)
            .stderr(log_err)
            .spawn()
    }
}
