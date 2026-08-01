use crate::{event::AppEvent, input_store::InputStore, template::ViewId};
use anyhow::{Context, Result, anyhow, bail};
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, FdFlag, fcntl},
    libc,
    pty::{Winsize, openpty},
    sys::signal::{Signal, killpg},
    unistd::{Pid, dup, pipe},
};
use std::{
    fs::File,
    io::{ErrorKind, Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt as _},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::mpsc::UnboundedSender;

const TERMINATION_GRACE: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct RunningProcess {
    child: Option<Child>,
    process_group: Pid,
    master: File,
    cancelled: Arc<AtomicBool>,
    input: InputStore,
    terminated: bool,
}

impl RunningProcess {
    pub fn spawn(
        arguments: &[String],
        rows: u16,
        columns: u16,
        input: &InputStore,
        events: &UnboundedSender<AppEvent>,
        view: ViewId,
        generation: u64,
    ) -> Result<Self> {
        let executable = arguments
            .first()
            .filter(|value| !value.is_empty())
            .context("empty executable")?;
        let size = Winsize {
            ws_row: rows.max(1),
            ws_col: columns.max(1),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&size), None).context("failed to create PTY")?;
        let stdout_slave = dup(&pty.slave).context("failed to duplicate PTY slave")?;
        let (stdin_reader, stdin_writer) = pipe().context("failed to create child input pipe")?;
        for descriptor in [&pty.master, &pty.slave, &stdout_slave, &stdin_reader, &stdin_writer] {
            fcntl(descriptor, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).context("failed to protect child descriptors")?;
        }

        let mut command = Command::new(executable);
        command
            .args(&arguments[1..])
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(stdin_reader))
            .stdout(Stdio::from(stdout_slave))
            .stderr(Stdio::from(pty.slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCSCTTY, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn {executable:?}"))?;
        let Ok(raw_pid) = i32::try_from(child.id()) else {
            let _ = child.kill();
            let _ = child.wait();
            bail!("child PID exceeds i32");
        };
        let process_group = Pid::from_raw(raw_pid);
        let master = File::from(pty.master);
        let cancelled = Arc::new(AtomicBool::new(false));
        let process = Self {
            child: Some(child),
            process_group,
            master,
            cancelled,
            input: input.clone(),
            terminated: false,
        };

        let reader = process.master.try_clone().context("failed to clone PTY master")?;
        spawn_pty_reader(reader, events.clone(), view, generation)?;
        input.spawn_pump(
            File::from(stdin_writer),
            Arc::clone(&process.cancelled),
            events.clone(),
            view,
            generation,
        )?;

        Ok(process)
    }

    pub fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        match self.master.write_all(bytes) {
            Ok(()) => Ok(()),
            Err(error) if matches!(error.kind(), ErrorKind::BrokenPipe | ErrorKind::NotConnected) => Ok(()),
            Err(error) if error.raw_os_error() == Some(Errno::EIO as i32) => Ok(()),
            Err(error) => Err(error).context("failed to write interactive input to PTY"),
        }
    }

    pub fn resize(&self, rows: u16, columns: u16) -> Result<()> {
        let size = Winsize {
            ws_row: rows.max(1),
            ws_col: columns.max(1),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        if result == -1 {
            bail!(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn poll_exit(&mut self) -> Result<Option<ExitStatus>> {
        let Some(child) = &mut self.child else {
            return Ok(None);
        };
        let status = child.try_wait().context("failed to poll child")?;
        if status.is_some() {
            self.child = None;
        }
        Ok(status)
    }

    pub fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        self.cancelled.store(true, Ordering::Release);
        self.input.wake_pumps();
        signal_group(self.process_group, Signal::SIGTERM);

        let deadline = Instant::now() + TERMINATION_GRACE;
        while Instant::now() < deadline {
            if let Some(child) = &mut self.child {
                match child.try_wait() {
                    Ok(Some(_)) => self.child = None,
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
            thread::sleep(Duration::from_millis(5));
        }

        signal_group(self.process_group, Signal::SIGKILL);
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

impl Drop for RunningProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn signal_group(process_group: Pid, signal: Signal) {
    if let Err(error) = killpg(process_group, signal)
        && error != Errno::ESRCH
    {
        let _ = error;
    }
}

fn spawn_pty_reader(mut reader: File, events: UnboundedSender<AppEvent>, view: ViewId, generation: u64) -> Result<()> {
    thread::Builder::new()
        .name(format!("prism-pty-reader-{view}-{generation}"))
        .spawn(move || {
            let mut buffer = vec![0; 64 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(count) => {
                        if events
                            .send(AppEvent::PtyOutput {
                                view,
                                generation,
                                bytes: buffer[..count].to_vec(),
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(error) if error.raw_os_error() == Some(Errno::EIO as i32) => return,
                    Err(error) => {
                        let _ = events.send(AppEvent::WorkerFailed {
                            view: Some(view),
                            generation: Some(generation),
                            error: anyhow!(error).context("PTY output reader failed").to_string(),
                        });
                        return;
                    }
                }
            }
        })
        .context("failed to start PTY output reader")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc;

    fn drain_output(receiver: &mut mpsc::UnboundedReceiver<AppEvent>, output: &mut Vec<u8>) {
        while let Ok(event) = receiver.try_recv() {
            if let AppEvent::PtyOutput { bytes, .. } = event {
                output.extend(bytes);
            }
        }
    }

    fn wait_for_text(receiver: &mut mpsc::UnboundedReceiver<AppEvent>, output: &mut Vec<u8>, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            drain_output(receiver, output);
            if String::from_utf8_lossy(output).contains(needle) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "timed out waiting for {needle:?}; output: {:?}",
            String::from_utf8_lossy(output)
        );
    }

    fn process_is_running(process: Pid) -> bool {
        if nix::sys::signal::kill(process, None).is_err() {
            return false;
        }
        #[cfg(target_os = "linux")]
        {
            let path = format!("/proc/{}/stat", process.as_raw());
            std::fs::read_to_string(path)
                .ok()
                .and_then(|stat| stat.rsplit_once(") ").map(|(_, fields)| fields.starts_with('Z')))
                == Some(false)
        }
        #[cfg(not(target_os = "linux"))]
        true
    }

    #[test]
    fn child_has_hybrid_terminal_and_merged_output() {
        let (events, mut receiver) = mpsc::unbounded_channel();
        let input = InputStore::start(Box::new(std::io::Cursor::new(b"abc\0def".to_vec())), events.clone()).unwrap();
        let script = concat!(
            "if [ -t 0 ]; then echo stdin=tty; else echo stdin=pipe; fi; ",
            "if [ -t 1 ]; then echo stdout=tty; else echo stdout=pipe; fi; ",
            "if [ -t 2 ]; then echo stderr=tty >&2; else echo stderr=pipe >&2; fi; ",
            "if : </dev/tty; then echo devtty=yes; else echo devtty=no; fi; ",
            "printf '\\033[31mansi\\033[0m\\n'; ",
            "wc -c | tr -d ' ' | sed 's/^/input=/'"
        );
        let arguments = vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()];
        let mut process = RunningProcess::spawn(&arguments, 20, 80, &input, &events, 0, 1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut exited = false;
        while Instant::now() < deadline {
            drain_output(&mut receiver, &mut output);
            if process.poll_exit().unwrap().is_some() {
                exited = true;
                thread::sleep(Duration::from_millis(20));
                drain_output(&mut receiver, &mut output);
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let output = String::from_utf8_lossy(&output);
        assert!(exited, "hybrid PTY child did not exit; output: {output:?}");
        assert!(output.contains("stdin=pipe"), "{output:?}");
        assert!(output.contains("stdout=tty"), "{output:?}");
        assert!(output.contains("stderr=tty"), "{output:?}");
        assert!(output.contains("devtty=yes"), "{output:?}");
        assert!(output.contains("\x1b[31mansi\x1b[0m"), "{output:?}");
        assert!(output.contains("input=7"), "{output:?}");
    }

    #[test]
    fn interactive_keys_and_resize_reach_controlling_terminal() {
        let (events, mut receiver) = mpsc::unbounded_channel();
        let input = InputStore::start(Box::new(std::io::Cursor::new(Vec::<u8>::new())), events.clone()).unwrap();
        let script = concat!(
            "stty -echo -icanon min 1 time 0 </dev/tty; ",
            "echo interactive-ready; ",
            "key=$(dd bs=1 count=1 </dev/tty 2>/dev/null); ",
            "stty size </dev/tty; echo key=$key"
        );
        let arguments = vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()];
        let mut process = RunningProcess::spawn(&arguments, 20, 80, &input, &events, 0, 1).unwrap();
        let mut output = Vec::new();
        wait_for_text(&mut receiver, &mut output, "interactive-ready");
        process.resize(13, 47).unwrap();
        process.write_input(b"Z").unwrap();
        wait_for_text(&mut receiver, &mut output, "key=Z");
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("13 47"), "{output:?}");
    }

    #[test]
    fn termination_kills_process_group_descendants() {
        let (events, mut receiver) = mpsc::unbounded_channel();
        let input = InputStore::start(Box::new(std::io::Cursor::new(Vec::<u8>::new())), events.clone()).unwrap();
        let arguments = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "sleep 30 & echo descendant=$!; wait".to_owned(),
        ];
        let mut process = RunningProcess::spawn(&arguments, 20, 80, &input, &events, 0, 1).unwrap();
        let mut output = Vec::new();
        wait_for_text(&mut receiver, &mut output, "descendant=");
        let output = String::from_utf8_lossy(&output);
        let descendant = output
            .split("descendant=")
            .nth(1)
            .and_then(|value| value.split(|character: char| !character.is_ascii_digit()).next())
            .and_then(|value| value.parse::<i32>().ok())
            .expect("descendant PID in child output");
        process.terminate();

        let descendant = Pid::from_raw(descendant);
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_is_running(descendant) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!process_is_running(descendant));
    }
}
