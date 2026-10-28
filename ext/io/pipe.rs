// Copyright 2018-2026 the Deno authors. MIT license.
use std::io;
use std::pin::Pin;
use std::process::Stdio;

pub type RawPipeHandle = super::RawIoHandle;

// The synchronous read end of a unidirectional pipe.
pub struct PipeRead {
  file: std::fs::File,
}

// The asynchronous read end of a unidirectional pipe.
pub struct AsyncPipeRead {
  #[cfg(windows)]
  /// The client end of the overlapped named pipe [`pipe`] creates, read
  /// through the runtime's I/O completion port. tokio's `ChildStdout`, used
  /// here before, read it with a blocking `ReadFile` on the runtime's
  /// blocking pool, and a pending read held its pool thread until data or EOF
  /// arrived: a quiet pipe (a test worker's stderr) pinned a thread for as
  /// long as it stayed quiet. The pool is capped (four threads per CPU on
  /// Windows), so enough of them starved every other blocking task (file
  /// system ops, child process I/O). An overlapped read holds no thread.
  read: tokio::net::windows::named_pipe::NamedPipeClient,
  #[cfg(not(windows))]
  read: tokio::net::unix::pipe::Receiver,
}

// The synchronous write end of a unidirectional pipe.
pub struct PipeWrite {
  file: std::fs::File,
}

// The asynchronous write end of a unidirectional pipe.
pub struct AsyncPipeWrite {
  #[cfg(windows)]
  /// The server end of the overlapped named pipe [`pipe`] creates, written
  /// through the runtime's I/O completion port. tokio's `ChildStdin`, used
  /// here before, wrote it with a blocking `WriteFile` on the runtime's
  /// blocking pool, and a write the reader didn't take yet (more than the
  /// pipe's 4 KiB buffer) held its pool thread until it did (DENEXT.md item
  /// 137, the write side of item 119).
  write: tokio::net::windows::named_pipe::NamedPipeServer,
  #[cfg(not(windows))]
  write: tokio::net::unix::pipe::Sender,
}

impl PipeRead {
  /// Converts this sync reader into an async reader. May fail if the Tokio runtime is
  /// unavailable.
  #[cfg(windows)]
  pub fn into_async(self) -> io::Result<AsyncPipeRead> {
    use std::os::windows::io::IntoRawHandle;
    let owned: std::os::windows::io::OwnedHandle = self.file.into();
    // SAFETY: a `PipeRead` holds the client end of the named pipe
    // `pipe_impl` creates, opened with `FILE_FLAG_OVERLAPPED`, which is what
    // `NamedPipeClient` requires; ownership moves to it (it closes the handle,
    // also when registering it fails).
    let read = unsafe {
      tokio::net::windows::named_pipe::NamedPipeClient::from_raw_handle(
        owned.into_raw_handle(),
      )?
    };
    Ok(AsyncPipeRead { read })
  }

  /// Converts this sync reader into an async reader. May fail if the Tokio runtime is
  /// unavailable.
  #[cfg(not(windows))]
  pub fn into_async(self) -> io::Result<AsyncPipeRead> {
    Ok(AsyncPipeRead {
      read: tokio::net::unix::pipe::Receiver::from_file(self.file)?,
    })
  }

  /// Creates a new [`PipeRead`] instance that shares the same underlying file handle
  /// as the existing [`PipeRead`] instance.
  pub fn try_clone(&self) -> io::Result<Self> {
    Ok(Self {
      file: self.file.try_clone()?,
    })
  }
}

impl AsyncPipeRead {
  /// Converts this async reader into an sync reader.
  ///
  /// Not supported on Windows: the handle stays bound to the runtime's I/O
  /// completion port (tokio's `NamedPipeClient` can't hand it back), and the
  /// reader may already hold data it read ahead, which a sync reader would
  /// never see.
  #[cfg(windows)]
  pub fn into_sync(self) -> io::Result<PipeRead> {
    Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "an async pipe reader can't be made synchronous again on Windows",
    ))
  }

  /// Converts this async reader into an sync reader. May fail if the Tokio runtime is
  /// unavailable.
  #[cfg(not(windows))]
  pub fn into_sync(self) -> io::Result<PipeRead> {
    let file = self.read.into_nonblocking_fd()?.into();
    Ok(PipeRead { file })
  }
}

impl std::io::Read for PipeRead {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    self.file.read(buf)
  }

  fn read_vectored(
    &mut self,
    bufs: &mut [io::IoSliceMut<'_>],
  ) -> io::Result<usize> {
    self.file.read_vectored(bufs)
  }
}

impl tokio::io::AsyncRead for AsyncPipeRead {
  fn poll_read(
    self: Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
    buf: &mut tokio::io::ReadBuf<'_>,
  ) -> std::task::Poll<io::Result<()>> {
    Pin::new(&mut self.get_mut().read).poll_read(cx, buf)
  }
}

impl PipeWrite {
  /// Converts this sync writer into an async writer. May fail if the Tokio runtime is
  /// unavailable.
  #[cfg(windows)]
  pub fn into_async(self) -> io::Result<AsyncPipeWrite> {
    use std::os::windows::io::IntoRawHandle;
    let owned: std::os::windows::io::OwnedHandle = self.file.into();
    // SAFETY: a `PipeWrite` holds the server end of the named pipe
    // `pipe_impl` creates, opened with `FILE_FLAG_OVERLAPPED`, which is what
    // `NamedPipeServer` requires; ownership moves to it (it closes the handle,
    // also when registering it fails).
    let write = unsafe {
      tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle(
        owned.into_raw_handle(),
      )?
    };
    Ok(AsyncPipeWrite { write })
  }

  /// Converts this sync writer into an async writer. May fail if the Tokio runtime is
  /// unavailable.
  #[cfg(not(windows))]
  pub fn into_async(self) -> io::Result<AsyncPipeWrite> {
    Ok(AsyncPipeWrite {
      write: tokio::net::unix::pipe::Sender::from_file(self.file)?,
    })
  }

  /// Creates a new [`PipeWrite`] instance that shares the same underlying file handle
  /// as the existing [`PipeWrite`] instance.
  pub fn try_clone(&self) -> io::Result<Self> {
    Ok(Self {
      file: self.file.try_clone()?,
    })
  }
}

impl AsyncPipeWrite {
  /// Converts this async writer into an sync writer.
  ///
  /// Not supported on Windows: the handle stays bound to the runtime's I/O
  /// completion port (tokio's `NamedPipeServer` can't hand it back), and a
  /// write may still be in flight on it. Nothing in the tree calls it.
  #[cfg(windows)]
  pub fn into_sync(self) -> io::Result<PipeWrite> {
    Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "an async pipe writer can't be made synchronous again on Windows",
    ))
  }

  /// Converts this async writer into an sync writer. May fail if the Tokio runtime is
  /// unavailable.
  #[cfg(not(windows))]
  pub fn into_sync(self) -> io::Result<PipeWrite> {
    let file = self.write.into_nonblocking_fd()?.into();
    Ok(PipeWrite { file })
  }
}

impl std::io::Write for PipeWrite {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.file.write(buf)
  }

  fn flush(&mut self) -> io::Result<()> {
    self.file.flush()
  }

  fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
    self.file.write_vectored(bufs)
  }
}

impl tokio::io::AsyncWrite for AsyncPipeWrite {
  #[inline(always)]
  fn poll_write(
    self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
    buf: &[u8],
  ) -> std::task::Poll<Result<usize, io::Error>> {
    Pin::new(&mut self.get_mut().write).poll_write(cx, buf)
  }

  #[inline(always)]
  fn poll_flush(
    self: Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Result<(), io::Error>> {
    Pin::new(&mut self.get_mut().write).poll_flush(cx)
  }

  #[inline(always)]
  fn poll_shutdown(
    self: Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Result<(), io::Error>> {
    Pin::new(&mut self.get_mut().write).poll_shutdown(cx)
  }

  #[inline(always)]
  fn is_write_vectored(&self) -> bool {
    self.write.is_write_vectored()
  }

  #[inline(always)]
  fn poll_write_vectored(
    self: Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
    bufs: &[io::IoSlice<'_>],
  ) -> std::task::Poll<Result<usize, io::Error>> {
    Pin::new(&mut self.get_mut().write).poll_write_vectored(cx, bufs)
  }
}

impl From<PipeRead> for Stdio {
  fn from(val: PipeRead) -> Self {
    Stdio::from(val.file)
  }
}

impl From<PipeWrite> for Stdio {
  fn from(val: PipeWrite) -> Self {
    Stdio::from(val.file)
  }
}

impl From<PipeRead> for std::fs::File {
  fn from(val: PipeRead) -> Self {
    val.file
  }
}

impl From<PipeWrite> for std::fs::File {
  fn from(val: PipeWrite) -> Self {
    val.file
  }
}

#[cfg(not(windows))]
impl From<PipeRead> for std::os::unix::io::OwnedFd {
  fn from(val: PipeRead) -> Self {
    val.file.into()
  }
}

#[cfg(not(windows))]
impl From<PipeWrite> for std::os::unix::io::OwnedFd {
  fn from(val: PipeWrite) -> Self {
    val.file.into()
  }
}

#[cfg(windows)]
impl From<PipeRead> for std::os::windows::io::OwnedHandle {
  fn from(val: PipeRead) -> Self {
    val.file.into()
  }
}

#[cfg(windows)]
impl From<PipeWrite> for std::os::windows::io::OwnedHandle {
  fn from(val: PipeWrite) -> Self {
    val.file.into()
  }
}

/// Create a unidirectional pipe pair that starts off as a pair of synchronous file handles,
/// but either side may be promoted to an async-capable reader/writer.
///
/// On Windows, we use a named pipe because that's the only way to get reliable async I/O
/// support. On Unix platforms, we use the `os_pipe` library, which uses `pipe2` under the hood
/// (or `pipe` on OSX).
pub fn pipe() -> io::Result<(PipeRead, PipeWrite)> {
  pipe_impl()
}

/// Creates a unidirectional pipe on top of a named pipe (which is technically bidirectional).
#[cfg(windows)]
pub fn pipe_impl() -> io::Result<(PipeRead, PipeWrite)> {
  // SAFETY: We're careful with handles here
  unsafe {
    use std::os::windows::io::FromRawHandle;
    use std::os::windows::io::OwnedHandle;
    let (server, client) = crate::winpipe::create_named_pipe()?;
    let read = std::fs::File::from(OwnedHandle::from_raw_handle(client));
    let write = std::fs::File::from(OwnedHandle::from_raw_handle(server));
    Ok((PipeRead { file: read }, PipeWrite { file: write }))
  }
}

/// Creates a unidirectional pipe for unix platforms.
#[cfg(not(windows))]
pub fn pipe_impl() -> io::Result<(PipeRead, PipeWrite)> {
  use std::os::unix::io::OwnedFd;
  let (read, write) = os_pipe::pipe()?;
  let read = std::fs::File::from(Into::<OwnedFd>::into(read));
  let write = std::fs::File::from(Into::<OwnedFd>::into(write));
  Ok((PipeRead { file: read }, PipeWrite { file: write }))
}

#[cfg(test)]
mod tests {
  use std::io::Read;
  use std::io::Write;

  use tokio::io::AsyncReadExt;
  use tokio::io::AsyncWriteExt;

  use super::*;

  #[test]
  fn test_pipe() {
    let (mut read, mut write) = pipe().unwrap();
    // Write to the server and read from the client
    write.write_all(b"hello").unwrap();
    let mut buf: [u8; 5] = Default::default();
    read.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hello");
  }

  #[tokio::test]
  async fn test_async_pipe() {
    let (read, write) = pipe().unwrap();
    let mut read = read.into_async().unwrap();
    let mut write = write.into_async().unwrap();

    write.write_all(b"hello").await.unwrap();
    let mut buf: [u8; 5] = Default::default();
    read.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"hello");
  }

  /// Test a round-trip through async mode and back.
  #[tokio::test]
  async fn test_pipe_transmute() {
    let (mut read, mut write) = pipe().unwrap();

    // Sync
    write.write_all(b"hello").unwrap();
    let mut buf: [u8; 5] = Default::default();
    read.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hello");

    let mut read = read.into_async().unwrap();
    let mut write = write.into_async().unwrap();

    // Async
    write.write_all(b"hello").await.unwrap();
    let mut buf: [u8; 5] = Default::default();
    read.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"hello");

    // Windows can't detach a handle from the I/O completion port (see
    // `AsyncPipeRead::into_sync`, `AsyncPipeWrite::into_sync`).
    #[cfg(windows)]
    {
      assert_eq!(
        read.into_sync().err().map(|e| e.kind()),
        Some(io::ErrorKind::Unsupported)
      );
      assert_eq!(
        write.into_sync().err().map(|e| e.kind()),
        Some(io::ErrorKind::Unsupported)
      );
    }
    #[cfg(not(windows))]
    {
      let mut read = read.into_sync().unwrap();
      let mut write = write.into_sync().unwrap();

      // Sync
      write.write_all(b"hello").unwrap();
      let mut buf: [u8; 5] = Default::default();
      read.read_exact(&mut buf).unwrap();
      assert_eq!(&buf, b"hello");
    }
  }

  #[tokio::test]
  async fn test_async_pipe_is_nonblocking() {
    let (read, write) = pipe().unwrap();
    let mut read = read.into_async().unwrap();
    let mut write = write.into_async().unwrap();

    let a = tokio::spawn(async move {
      let mut buf: [u8; 5] = Default::default();
      read.read_exact(&mut buf).await.unwrap();
      assert_eq!(&buf, b"hello");
    });
    let b = tokio::spawn(async move {
      write.write_all(b"hello").await.unwrap();
    });

    a.await.unwrap();
    b.await.unwrap();
  }

  /// A pending read of a quiet pipe holds no thread of the runtime's blocking
  /// pool (DENEXT.md item 119). On Windows the read end used to be read with
  /// a blocking `ReadFile` on that pool, so more quiet pipes than the pool
  /// has threads (a `deno test --parallel` worker's stdout and stderr each
  /// held one) left no thread for anything else: here the `spawn_blocking`
  /// task never ran, and neither did the read of the pipe that has data.
  #[test]
  fn quiet_async_pipes_hold_no_blocking_thread() {
    const POOL: usize = 2;
    let rt = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .max_blocking_threads(POOL)
      .build()
      .unwrap();
    rt.block_on(async {
      let mut writers = Vec::new();
      let mut quiet = Vec::new();
      for _ in 0..4 * POOL {
        let (read, write) = pipe().unwrap();
        let mut read = read.into_async().unwrap();
        writers.push(write);
        quiet.push(tokio::spawn(async move {
          let mut buf = [0_u8; 1];
          read.read(&mut buf).await
        }));
      }
      // Let every quiet read start.
      tokio::time::sleep(std::time::Duration::from_millis(200)).await;

      let (read, write) = pipe().unwrap();
      let mut read = read.into_async().unwrap();
      let mut write = write.into_async().unwrap();
      let busy = async move {
        write.write_all(b"hello").await.unwrap();
        let mut buf: [u8; 5] = Default::default();
        read.read_exact(&mut buf).await.unwrap();
        buf
      };
      let outcome =
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
          tokio::join!(tokio::task::spawn_blocking(|| 42), busy)
        })
        .await;
      // Closing the writers ends the quiet reads (at EOF) either way, so a
      // failure doesn't leave the pool's threads blocked behind it.
      drop(writers);
      let (blocking, busy) =
        outcome.expect("quiet pipe reads starved the blocking pool");
      assert_eq!(blocking.unwrap(), 42);
      assert_eq!(&busy, b"hello");
      for read in quiet {
        assert_eq!(read.await.unwrap().unwrap(), 0);
      }
    });
  }
  /// A write the reader doesn't take yet holds no thread of the runtime's
  /// blocking pool either (DENEXT.md item 137). On Windows the write end used
  /// to be written with a blocking `WriteFile` on that pool, which waited
  /// there for the reader once the pipe's 4 KiB buffer was full.
  #[test]
  fn pending_async_pipe_writes_hold_no_blocking_thread() {
    const POOL: usize = 2;
    let rt = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .max_blocking_threads(POOL)
      .build()
      .unwrap();
    rt.block_on(async {
      let mut readers = Vec::new();
      let mut writes = Vec::new();
      for _ in 0..4 * POOL {
        let (read, write) = pipe().unwrap();
        let mut write = write.into_async().unwrap();
        readers.push(read);
        writes.push(tokio::spawn(async move {
          // Well past the pipe's buffer, and nobody reads it yet.
          write.write_all(&[97; 256 * 1024]).await?;
          write.shutdown().await?;
          Ok::<_, io::Error>(write)
        }));
      }
      // Let every write start.
      tokio::time::sleep(std::time::Duration::from_millis(200)).await;

      let blocking = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::task::spawn_blocking(|| 42),
      )
      .await;
      // Reading the pipes (through the completion port, item 119) ends the
      // writes either way, so a failure doesn't leave the pool's threads
      // blocked behind them.
      let reads = readers
        .into_iter()
        .map(|read| {
          let mut read = read.into_async().unwrap();
          tokio::spawn(async move {
            let mut buf = vec![0_u8; 256 * 1024];
            read.read_exact(&mut buf).await.map(|_| buf)
          })
        })
        .collect::<Vec<_>>();
      for read in reads {
        assert!(read.await.unwrap().unwrap().iter().all(|b| *b == 97));
      }
      for write in writes {
        write.await.unwrap().unwrap();
      }
      assert_eq!(
        blocking
          .expect("pending pipe writes starved the blocking pool")
          .unwrap(),
        42
      );
    });
  }
}
