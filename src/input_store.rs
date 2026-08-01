use crate::{event::AppEvent, template::ViewId};
use anyhow::{Context, Result};
use std::{
    fs::File,
    io::{ErrorKind, Read, Seek, SeekFrom, Write},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use tempfile::NamedTempFile;
use tokio::sync::mpsc::UnboundedSender;

const BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug, Default)]
struct CaptureState {
    published_len: u64,
    eof: bool,
    failed: bool,
}

#[derive(Debug)]
struct Inner {
    tempfile: NamedTempFile,
    state: Mutex<CaptureState>,
    advanced: Condvar,
}

#[derive(Debug, Clone)]
pub struct InputStore {
    inner: Arc<Inner>,
}

impl InputStore {
    pub fn start(source: Box<dyn Read + Send>, events: UnboundedSender<AppEvent>) -> Result<Self> {
        let tempfile = NamedTempFile::new().context("failed to create input backing file")?;
        let writer = tempfile
            .reopen()
            .context("failed to open input backing file for capture")?;
        let inner = Arc::new(Inner {
            tempfile,
            state: Mutex::new(CaptureState::default()),
            advanced: Condvar::new(),
        });
        let store = Self {
            inner: Arc::clone(&inner),
        };
        thread::Builder::new()
            .name("prism-input-capture".to_owned())
            .spawn(move || capture(source, writer, &inner, &events))
            .context("failed to start input capture thread")?;
        Ok(store)
    }

    pub fn spawn_pump(
        &self,
        writer: File,
        cancelled: Arc<AtomicBool>,
        events: UnboundedSender<AppEvent>,
        view: ViewId,
        generation: u64,
    ) -> Result<()> {
        let reader = File::open(self.inner.tempfile.path()).context("failed to open input replay file")?;
        let inner = Arc::clone(&self.inner);
        thread::Builder::new()
            .name(format!("prism-input-pump-{view}-{generation}"))
            .spawn(move || {
                pump(reader, writer, &inner, &cancelled);
                let _ = events.send(AppEvent::PumpEnded { view, generation });
            })
            .context("failed to start input pump")?;
        Ok(())
    }

    pub fn wake_pumps(&self) {
        self.inner.advanced.notify_all();
    }

    #[cfg(test)]
    fn snapshot(&self) -> (u64, bool, bool) {
        let state = self.inner.state.lock().unwrap();
        (state.published_len, state.eof, state.failed)
    }
}

fn capture(mut source: Box<dyn Read + Send>, mut writer: File, inner: &Inner, events: &UnboundedSender<AppEvent>) {
    let mut buffer = vec![0; BUFFER_SIZE];
    loop {
        match source.read(&mut buffer) {
            Ok(0) => {
                let mut state = inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                state.eof = true;
                inner.advanced.notify_all();
                drop(state);
                let _ = events.send(AppEvent::InputEof);
                return;
            }
            Ok(count) => {
                if let Err(error) = writer.write_all(&buffer[..count]) {
                    fail_capture(inner, events, format!("failed to store input: {error}"));
                    return;
                }
                let mut state = inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                state.published_len += u64::try_from(count).expect("input read size fits in u64");
                let len = state.published_len;
                inner.advanced.notify_all();
                drop(state);
                let _ = events.send(AppEvent::InputAdvanced { len });
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                fail_capture(inner, events, format!("failed to read input: {error}"));
                return;
            }
        }
    }
}

fn fail_capture(inner: &Inner, events: &UnboundedSender<AppEvent>, error: String) {
    let mut state = inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    state.failed = true;
    inner.advanced.notify_all();
    drop(state);
    let _ = events.send(AppEvent::InputFailed { error });
}

fn pump(mut reader: File, mut writer: File, inner: &Inner, cancelled: &AtomicBool) {
    let mut offset = 0_u64;
    let mut buffer = vec![0; BUFFER_SIZE];
    loop {
        let (published_len, finished) = {
            let mut state = inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            while offset == state.published_len && !state.eof && !state.failed && !cancelled.load(Ordering::Acquire) {
                state = inner
                    .advanced
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            (
                state.published_len,
                state.eof || state.failed || cancelled.load(Ordering::Acquire),
            )
        };

        if cancelled.load(Ordering::Acquire) {
            return;
        }
        if offset == published_len {
            if finished {
                return;
            }
            continue;
        }

        let available = published_len - offset;
        let count = usize::try_from(available.min(BUFFER_SIZE as u64)).expect("bounded pump read size fits usize");
        if reader.seek(SeekFrom::Start(offset)).is_err() {
            return;
        }
        if reader.read_exact(&mut buffer[..count]).is_err() {
            return;
        }
        if writer.write_all(&buffer[..count]).is_err() {
            return;
        }
        offset += u64::try_from(count).expect("pump write size fits u64");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::pipe;
    use std::{
        os::fd::OwnedFd,
        time::{Duration, Instant},
    };
    use tokio::sync::mpsc;

    fn wait_for_eof(store: &InputStore) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !store.snapshot().1 {
            assert!(Instant::now() < deadline, "input capture timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn file(fd: OwnedFd) -> File {
        File::from(fd)
    }

    struct GatedReader {
        stage: u8,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl Read for GatedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let bytes = match self.stage {
                0 => b"prefix".as_slice(),
                1 => {
                    self.release.recv().unwrap();
                    b"-suffix".as_slice()
                }
                _ => return Ok(0),
            };
            self.stage += 1;
            buffer[..bytes.len()].copy_from_slice(bytes);
            Ok(bytes.len())
        }
    }

    #[test]
    fn preserves_binary_bytes_and_publishes_after_storage() {
        let bytes = vec![0, 255, 1, b'\n', 128];
        let (events, _receiver) = mpsc::unbounded_channel();
        let store = InputStore::start(Box::new(std::io::Cursor::new(bytes.clone())), events).unwrap();
        wait_for_eof(&store);
        let (published, eof, failed) = store.snapshot();
        assert_eq!(published, bytes.len() as u64);
        assert!(eof);
        assert!(!failed);
        assert_eq!(std::fs::read(store.inner.tempfile.path()).unwrap(), bytes);
    }

    #[test]
    fn independent_pumps_receive_identical_input_and_eof() {
        let bytes = (0..=255).cycle().take(BUFFER_SIZE * 2 + 17).collect::<Vec<_>>();
        let (events, _receiver) = mpsc::unbounded_channel();
        let store = InputStore::start(Box::new(std::io::Cursor::new(bytes.clone())), events.clone()).unwrap();
        let (read_one, write_one) = pipe().unwrap();
        let (read_two, write_two) = pipe().unwrap();
        store
            .spawn_pump(file(write_one), Arc::new(AtomicBool::new(false)), events.clone(), 0, 1)
            .unwrap();
        store
            .spawn_pump(file(write_two), Arc::new(AtomicBool::new(false)), events, 1, 1)
            .unwrap();
        let first = thread::spawn(move || {
            let mut output = Vec::new();
            file(read_one).read_to_end(&mut output).unwrap();
            output
        });
        let second = thread::spawn(move || {
            let mut output = Vec::new();
            file(read_two).read_to_end(&mut output).unwrap();
            output
        });
        assert_eq!(first.join().unwrap(), bytes);
        assert_eq!(second.join().unwrap(), bytes);
    }

    #[test]
    fn blocked_pump_does_not_block_capture_or_another_pump() {
        let bytes = vec![42; BUFFER_SIZE * 32];
        let (events, _receiver) = mpsc::unbounded_channel();
        let store = InputStore::start(Box::new(std::io::Cursor::new(bytes.clone())), events.clone()).unwrap();
        let (blocked_reader, blocked_writer) = pipe().unwrap();
        let (fast_reader, fast_writer) = pipe().unwrap();
        store
            .spawn_pump(
                file(blocked_writer),
                Arc::new(AtomicBool::new(false)),
                events.clone(),
                0,
                1,
            )
            .unwrap();
        store
            .spawn_pump(file(fast_writer), Arc::new(AtomicBool::new(false)), events, 1, 1)
            .unwrap();
        let (complete, received) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut output = Vec::new();
            file(fast_reader).read_to_end(&mut output).unwrap();
            complete.send(output).unwrap();
        });
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), bytes);
        assert!(store.snapshot().1);
        drop(blocked_reader);
    }

    #[test]
    fn pump_started_after_eof_replays_then_closes() {
        let bytes = b"complete input".to_vec();
        let (events, _receiver) = mpsc::unbounded_channel();
        let store = InputStore::start(Box::new(std::io::Cursor::new(bytes.clone())), events.clone()).unwrap();
        wait_for_eof(&store);
        let (reader, writer) = pipe().unwrap();
        store
            .spawn_pump(file(writer), Arc::new(AtomicBool::new(false)), events, 0, 1)
            .unwrap();
        let mut output = Vec::new();
        file(reader).read_to_end(&mut output).unwrap();
        assert_eq!(output, bytes);
    }

    #[test]
    fn restart_during_capture_gets_prefix_and_later_bytes_once() {
        let (release, wait_for_release) = std::sync::mpsc::channel();
        let source = GatedReader {
            stage: 0,
            release: wait_for_release,
        };
        let (events, _receiver) = mpsc::unbounded_channel();
        let store = InputStore::start(Box::new(source), events.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while store.snapshot().0 < 6 {
            assert!(Instant::now() < deadline, "input prefix was not published");
            thread::sleep(Duration::from_millis(5));
        }

        let (first_reader, first_writer) = pipe().unwrap();
        let (restart_reader, restart_writer) = pipe().unwrap();
        store
            .spawn_pump(
                file(first_writer),
                Arc::new(AtomicBool::new(false)),
                events.clone(),
                0,
                1,
            )
            .unwrap();
        store
            .spawn_pump(file(restart_writer), Arc::new(AtomicBool::new(false)), events, 0, 2)
            .unwrap();
        let first = thread::spawn(move || {
            let mut bytes = Vec::new();
            file(first_reader).read_to_end(&mut bytes).unwrap();
            bytes
        });
        let restarted = thread::spawn(move || {
            let mut bytes = Vec::new();
            file(restart_reader).read_to_end(&mut bytes).unwrap();
            bytes
        });
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap(), b"prefix-suffix");
        assert_eq!(restarted.join().unwrap(), b"prefix-suffix");
    }
}
