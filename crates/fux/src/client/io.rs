//! Owned stdin and SIGWINCH producers for the viewer. Adapted from koh (MIT); see LICENSES/koh.txt.

use anyhow::Context;
use nix::poll::{PollFd, PollFlags, poll};
use std::io::Read;
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

pub struct ClientIo {
    pub input_rx: mpsc::Receiver<Vec<u8>>,
    pub resize_rx: mpsc::Receiver<()>,
    cancel: Arc<AtomicBool>,
    input: Option<JoinHandle<()>>,
    resize: Option<tokio::task::JoinHandle<()>>,
}

impl ClientIo {
    /// Starts byte-exact stdin and SIGWINCH producers. Requires a Tokio runtime.
    pub fn spawn() -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Handle::try_current()
            .context("starting client I/O requires a Tokio runtime")?;
        let mut sigwinch =
            signal(SignalKind::window_change()).context("installing SIGWINCH handler")?;
        let cancel = Arc::new(AtomicBool::new(false));
        let (input_tx, input_rx) = mpsc::channel(64);
        let input_cancel = Arc::clone(&cancel);
        let input = std::thread::Builder::new()
            .name("fux-stdin".into())
            .spawn(move || read_input(std::io::stdin(), &input_tx, &input_cancel))
            .context("spawning stdin producer")?;
        let (resize_tx, resize_rx) = mpsc::channel(8);
        let resize_cancel = Arc::clone(&cancel);
        let resize = runtime.spawn(async move {
            loop {
                if sigwinch.recv().await.is_none() || resize_cancel.load(Ordering::Acquire) {
                    break;
                }
                if resize_tx.try_send(()).is_err() && resize_tx.is_closed() {
                    break;
                }
            }
        });
        Ok(Self {
            input_rx,
            resize_rx,
            cancel,
            input: Some(input),
            resize: Some(resize),
        })
    }

    /// Cancels and joins both producers. The stdin poll checks cancellation every 100 ms.
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.cancel.store(true, Ordering::Release);
        if let Some(resize) = self.resize.take() {
            resize.abort();
            let _ = resize.await;
        }
        let input = self.input.take();
        tokio::task::spawn_blocking(move || {
            if let Some(input) = input {
                input
                    .join()
                    .map_err(|_| anyhow::anyhow!("stdin producer panicked"))?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("joining stdin producer")?
    }
}

impl Drop for ClientIo {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(resize) = self.resize.take() {
            resize.abort();
        }
    }
}

fn read_input<R: Read + AsFd>(mut reader: R, sender: &mpsc::Sender<Vec<u8>>, cancel: &AtomicBool) {
    let mut buffer = [0_u8; 1024];
    while !cancel.load(Ordering::Acquire) {
        let ready = {
            let mut descriptors = [PollFd::new(reader.as_fd(), PollFlags::POLLIN)];
            poll(&mut descriptors, 100_u16)
        };
        match ready {
            Ok(0) | Err(nix::errno::Errno::EINTR) => {}
            Ok(_) => match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    let Some(chunk) = buffer.get(..count) else {
                        break;
                    };
                    if !send_chunk(sender, cancel, chunk.to_vec()) {
                        break;
                    }
                }
            },
            Err(_) => break,
        }
    }
}

fn send_chunk(sender: &mpsc::Sender<Vec<u8>>, cancel: &AtomicBool, mut chunk: Vec<u8>) -> bool {
    loop {
        if cancel.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(chunk) {
            Ok(()) => return true,
            Err(TrySendError::Closed(_)) => return false,
            Err(TrySendError::Full(returned)) => {
                chunk = returned;
                std::thread::park_timeout(std::time::Duration::from_millis(10));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn input_producer_forwards_bytes_exactly_and_cancels_while_idle() {
        let (reader, mut writer) = UnixStream::pair().unwrap_or_else(|_| std::process::abort());
        let (sender, mut receiver) = mpsc::channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let input = std::thread::spawn(move || read_input(reader, &sender, &thread_cancel));
        writer.write_all(b"a\0\x1bZ").unwrap_or_default();
        assert_eq!(
            receiver.recv().await.as_deref(),
            Some(b"a\0\x1bZ".as_slice())
        );
        cancel.store(true, Ordering::Release);
        let start = Instant::now();
        assert!(input.join().is_ok());
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn full_channel_does_not_block_cancellation() {
        let (reader, mut writer) = UnixStream::pair().unwrap_or_else(|_| std::process::abort());
        let (sender, mut receiver) = mpsc::channel(1);
        sender.try_send(vec![0]).unwrap_or_default();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let input = std::thread::spawn(move || read_input(reader, &sender, &thread_cancel));
        writer.write_all(b"blocked").unwrap_or_default();
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.store(true, Ordering::Release);
        assert!(input.join().is_ok());
        assert_eq!(receiver.recv().await, Some(vec![0]));
    }
}
