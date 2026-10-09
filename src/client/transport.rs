use crate::protocol::message::Message;
use crate::protocol::{read_message, write_message};
use anyhow::{Context, Result, bail};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub trait Transport {
    fn send(&mut self, msg: &Message) -> Result<()>;
    fn recv(&mut self) -> Result<Message>;
}

/// Generic transport over any Read + Write pair.
pub struct StreamTransport<R: Read, W: Write> {
    reader: R,
    writer: W,
}

impl<R: Read, W: Write> StreamTransport<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }
}

impl<R: Read, W: Write> Transport for StreamTransport<R, W> {
    fn send(&mut self, msg: &Message) -> Result<()> {
        write_message(&mut self.writer, msg).context("Failed to write message to stream")?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Message> {
        match read_message::<_, Message>(&mut self.reader)? {
            Some(msg) => Ok(msg),
            None => bail!("Stream reached EOF unexpectedly"),
        }
    }
}

/// Local transport that spawns the agent as a child process using stdin/stdout pipes.
pub struct LocalProcessTransport {
    _child: Child,
    inner: StreamTransport<BufReader<ChildStdout>, BufWriter<ChildStdin>>,
}

impl LocalProcessTransport {
    pub fn spawn(agent_bin: &std::path::Path, base_dir: Option<&std::path::Path>) -> Result<Self> {
        let mut cmd = Command::new(agent_bin);
        cmd.arg("agent");
        if let Some(dir) = base_dir {
            cmd.arg("--base-dir").arg(dir);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn local agent at {}", agent_bin.display()))?;

        let stdin = child.stdin.take().expect("Child stdin must be piped");
        let stdout = child.stdout.take().expect("Child stdout must be piped");

        let stream = StreamTransport::new(BufReader::new(stdout), BufWriter::new(stdin));

        Ok(Self {
            _child: child,
            inner: stream,
        })
    }
}

impl Transport for LocalProcessTransport {
    fn send(&mut self, msg: &Message) -> Result<()> {
        self.inner.send(msg)
    }

    fn recv(&mut self) -> Result<Message> {
        self.inner.recv()
    }
}

/// Wraps a string in single quotes according to POSIX shell escaping rules.
/// Internal single quotes are safely escaped as `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Builds command-line argument list for invoking remote agent via OpenSSH.
///
/// Ensures OpenSSH options: `-T -o BatchMode=yes <destination> <single remote shell command>`.
pub fn build_ssh_args(destination: &str, agent_bin: &str, base_dir: Option<&str>) -> Vec<String> {
    let mut remote_parts = vec![shell_quote(agent_bin), shell_quote("agent")];
    if let Some(dir) = base_dir {
        remote_parts.push(shell_quote("--base-dir"));
        remote_parts.push(shell_quote(dir));
    }
    let remote_cmd = remote_parts.join(" ");

    vec![
        "-T".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        destination.to_string(),
        remote_cmd,
    ]
}

/// Creates a `std::process::Command` configured to invoke OpenSSH with safe arguments.
pub fn build_ssh_command(destination: &str, agent_bin: &str, base_dir: Option<&str>) -> Command {
    let args = build_ssh_args(destination, agent_bin, base_dir);
    let mut cmd = Command::new("ssh");
    cmd.args(args);
    cmd
}

/// Check SSH reachability without invoking the agent protocol. A nonzero remote `true` is a
/// connection failure for failover purposes; agent/protocol/build failures happen later.
pub fn probe_ssh(destination: &str) -> Result<()> {
    let output = Command::new("ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            destination,
            "true",
        ])
        .output()
        .with_context(|| format!("spawning SSH probe for {destination}"))?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "SSH connection probe failed for {destination} ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

/// Build this checkout's agent with the worker's native Cargo, then atomically install it.
pub fn build_and_install_native_agent(
    destination: &str,
    agent_bin: &str,
    source: &Path,
) -> Result<()> {
    let mut tar = Command::new("tar");
    tar.current_dir(source)
        .args(["-czf", "-", "--", "Cargo.toml", "Cargo.lock", "src"])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut tar = tar.spawn().context("starting source archive")?;
    let mut ssh = Command::new("ssh");
    ssh.args(["-T", "-o", "BatchMode=yes", destination]);
    let script = format!(
        "set -eu; d=$(mktemp -d); target={}; case \"$target\" in */*) ;; *) target=$(command -v \"$target\") ;; esac; parent=$(dirname \"$target\"); mkdir -p \"$parent\"; tmp=\"$parent/.rbuild-agent.$$\"; trap 'rm -rf \"$d\"; rm -f \"$tmp\"' EXIT; tar -xzf - -C \"$d\"; cargo build --release --manifest-path \"$d/Cargo.toml\"; install -m 755 \"$d/target/release/rbuild\" \"$tmp\"; mv -f \"$tmp\" \"$target\"",
        shell_quote(agent_bin)
    );
    ssh.arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut ssh = ssh
        .spawn()
        .context("starting native worker build via SSH")?;
    let mut archive = tar.stdout.take().expect("tar stdout is piped");
    let mut remote_stdin = ssh.stdin.take().expect("ssh stdin is piped");
    std::io::copy(&mut archive, &mut remote_stdin).context("streaming rbuild source to worker")?;
    drop(archive);
    drop(remote_stdin);
    let status = ssh.wait().context("waiting for native worker build")?;
    let tar_status = match tar.try_wait()? {
        Some(status) => status,
        None => {
            tar.kill().context("stopping source archive process")?;
            tar.wait()?
        }
    };
    if !status.success() || !tar_status.success() {
        bail!("native agent build/install failed (ssh={status}, tar={tar_status})");
    }
    Ok(())
}

/// SSH transport that spawns external OpenSSH and multiplexes protocol messages over pipes.
///
/// Connects child stdin/stdout to protocol framing and forwards stderr directly to user stderr.
pub struct SshTransport {
    child: Option<Child>,
    inner: Option<StreamTransport<BufReader<ChildStdout>, BufWriter<ChildStdin>>>,
}

impl SshTransport {
    pub fn spawn(destination: &str, agent_bin: &str, base_dir: Option<&str>) -> Result<Self> {
        // A leading '-' would be parsed by ssh as an option (e.g. -oProxyCommand=...).
        if destination.is_empty()
            || destination.starts_with('-')
            || destination.chars().any(char::is_whitespace)
        {
            bail!(
                "Invalid SSH destination '{}': expected host or user@host without leading '-' or whitespace",
                destination
            );
        }
        let mut cmd = build_ssh_command(destination, agent_bin, base_dir);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()); // Connect ssh stderr to user stderr

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "Failed to spawn OpenSSH client for destination '{}'",
                destination
            )
        })?;

        let stdin = child.stdin.take().expect("Child stdin must be piped");
        let stdout = child.stdout.take().expect("Child stdout must be piped");

        let stream = StreamTransport::new(BufReader::new(stdout), BufWriter::new(stdin));

        Ok(Self {
            child: Some(child),
            inner: Some(stream),
        })
    }
}

impl SshTransport {
    /// Adds the ssh exit status to a stream error when ssh has already terminated.
    /// Remote failures usually surface as EOF; the status points to the cause.
    fn annotate_failure(&mut self, err: anyhow::Error) -> anyhow::Error {
        match self
            .child
            .as_mut()
            .and_then(|c| c.try_wait().ok().flatten())
        {
            Some(status) => err.context(format!("ssh exited with {status}; see stderr above")),
            None => err,
        }
    }
}

impl Transport for SshTransport {
    fn send(&mut self, msg: &Message) -> Result<()> {
        let result = self
            .inner
            .as_mut()
            .expect("SSH stream is present")
            .send(msg);
        result.map_err(|e| self.annotate_failure(e))
    }

    fn recv(&mut self) -> Result<Message> {
        let result = self.inner.as_mut().expect("SSH stream is present").recv();
        result.map_err(|e| self.annotate_failure(e))
    }
}

impl Drop for SshTransport {
    fn drop(&mut self) {
        // Closing SSH stdin lets the remote agent exit its frame loop before we reap ssh.
        drop(self.inner.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_quote_rules() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("simple"), "'simple'");
        assert_eq!(shell_quote("hello world"), "'hello world'");
        assert_eq!(shell_quote("don't"), "'don'\\''t'");
        assert_eq!(shell_quote("$FOO `rm -rf /`"), "'$FOO `rm -rf /`'");
        assert_eq!(shell_quote("a'b'c"), "'a'\\''b'\\''c'");
    }

    #[test]
    fn test_build_ssh_args_generation() {
        let destination = "ishigakiyua@100.64.0.12";
        let agent_bin = "rbuild";
        let base_dir = Some("/srv/build");

        let args = build_ssh_args(destination, agent_bin, base_dir);
        assert_eq!(
            args,
            vec![
                "-T",
                "-o",
                "BatchMode=yes",
                "ishigakiyua@100.64.0.12",
                "'rbuild' 'agent' '--base-dir' '/srv/build'",
            ]
        );
    }

    #[test]
    fn test_build_ssh_args_custom_binary_and_spaces() {
        let destination = "user@example.com";
        let agent_bin = "/opt/rbuild tools/bin";
        let base_dir = Some("/home/user/build root's");

        let args = build_ssh_args(destination, agent_bin, base_dir);
        assert_eq!(
            args,
            vec![
                "-T",
                "-o",
                "BatchMode=yes",
                "user@example.com",
                "'/opt/rbuild tools/bin' 'agent' '--base-dir' '/home/user/build root'\\''s'",
            ]
        );
    }

    #[test]
    fn test_ssh_destination_rejects_option_like_values() {
        // Rejected before ssh is spawned, so no network access happens here.
        for bad in ["", "-oProxyCommand=id", "user host"] {
            let err = SshTransport::spawn(bad, "rbuild", None)
                .err()
                .expect("invalid destination must be rejected");
            assert!(err.to_string().contains("Invalid SSH destination"));
        }
    }

    #[test]
    fn test_build_ssh_args_without_base_dir() {
        let destination = "host";
        let agent_bin = "rbuild";

        let args = build_ssh_args(destination, agent_bin, None);
        assert_eq!(
            args,
            vec!["-T", "-o", "BatchMode=yes", "host", "'rbuild' 'agent'",]
        );
    }
}
