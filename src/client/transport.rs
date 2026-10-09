use crate::protocol::message::Message;
use crate::protocol::{read_message, write_message};
use anyhow::{Context, Result, bail};
use std::io::{BufReader, BufWriter, Read, Write};
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
