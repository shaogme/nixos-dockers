//! Four-byte big-endian length-prefixed JSON frames for Unix protocols.

use libc::{
    c_int, poll, pollfd, recv, send, MSG_DONTWAIT, MSG_NOSIGNAL, POLLERR, POLLHUP, POLLIN,
    POLLNVAL, POLLOUT,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{from_slice, to_writer, Error as JsonError};
use std::{
    error::Error,
    fmt,
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    time::Instant,
};

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Json(JsonError),
    Empty,
    TooLarge { length: usize, limit: usize },
    LengthOverflow(usize),
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "frame IO failed: {error}"),
            Self::Json(error) => write!(formatter, "frame JSON failed: {error}"),
            Self::Empty => formatter.write_str("frame length must be nonzero"),
            Self::TooLarge { length, limit } => {
                write!(
                    formatter,
                    "frame length {length} exceeds the {limit} byte limit"
                )
            }
            Self::LengthOverflow(length) => write!(formatter, "frame length {length} exceeds u32"),
        }
    }
}

impl Error for FrameError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Empty | Self::TooLarge { .. } | Self::LengthOverflow(_) => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
pub struct FrameWriteError {
    pub error: FrameError,
    pub bytes_written: usize,
}

struct BoundedVecWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded_length: Option<usize>,
}

/// Serialize a JSON payload and prepend its four-byte big-endian length.
pub fn encode_json_frame<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, FrameError> {
    let payload = serialize_bounded(value, limit)?;
    encode_payload_frame(&payload, limit)
}

/// Add the common length prefix to an already serialized JSON payload.
pub fn encode_payload_frame(payload: &[u8], limit: usize) -> Result<Vec<u8>, FrameError> {
    ensure_payload(payload, limit)?;
    let length =
        u32::try_from(payload.len()).map_err(|_| FrameError::LengthOverflow(payload.len()))?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Write one JSON frame and flush the writer.
pub fn write_json_frame<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
    limit: usize,
) -> Result<(), FrameError> {
    writer.write_all(&encode_json_frame(value, limit)?)?;
    writer.flush()?;
    Ok(())
}

/// Write an already serialized JSON payload as one length-prefixed frame.
pub fn write_payload_frame(
    writer: &mut impl Write,
    payload: &[u8],
    limit: usize,
) -> Result<(), FrameError> {
    writer.write_all(&encode_payload_frame(payload, limit)?)?;
    writer.flush()?;
    Ok(())
}

/// Read and deserialize one JSON frame with an allocation limit.
pub fn read_json_frame<T: DeserializeOwned>(
    reader: &mut impl Read,
    limit: usize,
) -> Result<T, FrameError> {
    let payload = read_payload(reader, limit)?;
    from_slice(&payload).map_err(FrameError::Json)
}

/// Read one length-prefixed JSON payload without deserializing it.
pub fn read_payload_frame(reader: &mut impl Read, limit: usize) -> Result<Vec<u8>, FrameError> {
    read_payload(reader, limit)
}

/// Read and deserialize one frame without allowing the operation to exceed
/// an absolute deadline.
pub fn read_json_frame_until<T: DeserializeOwned>(
    stream: &UnixStream,
    deadline: Instant,
    limit: usize,
) -> Result<T, FrameError> {
    let mut header = [0_u8; 4];
    read_exact_until(stream, &mut header, deadline)?;
    let length = u32::from_be_bytes(header) as usize;
    check_length(length, limit)?;
    let mut payload = vec![0_u8; length];
    read_exact_until(stream, &mut payload, deadline)?;
    from_slice(&payload).map_err(FrameError::Json)
}

/// Write a frame by an absolute deadline. The byte count includes the header
/// and is used by clients to decide whether retrying is safe.
pub fn write_json_frame_until<T: Serialize>(
    stream: &UnixStream,
    value: &T,
    deadline: Instant,
    limit: usize,
) -> Result<usize, FrameWriteError> {
    let frame = encode_json_frame(value, limit).map_err(|error| FrameWriteError {
        error,
        bytes_written: 0,
    })?;
    write_all_until(stream, &frame, deadline).map_err(|(error, bytes_written)| FrameWriteError {
        error,
        bytes_written,
    })
}

fn serialize_bounded<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, FrameError> {
    let mut writer = BoundedVecWriter {
        bytes: Vec::with_capacity(limit.min(64 * 1024)),
        limit,
        exceeded_length: None,
    };
    let result = to_writer(&mut writer, value);
    if let Some(length) = writer.exceeded_length {
        return Err(FrameError::TooLarge { length, limit });
    }
    result.map_err(FrameError::Json)?;
    check_length(writer.bytes.len(), limit)?;
    Ok(writer.bytes)
}

impl Write for BoundedVecWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(next_length) = self.bytes.len().checked_add(buffer.len()) else {
            self.exceeded_length = Some(usize::MAX);
            return Err(io::Error::other("frame length overflow"));
        };
        if next_length > self.limit {
            self.exceeded_length = Some(next_length);
            return Err(io::Error::other("frame exceeds limit"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_payload(reader: &mut impl Read, limit: usize) -> Result<Vec<u8>, FrameError> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    check_length(length, limit)?;
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

fn ensure_payload(payload: &[u8], limit: usize) -> Result<(), FrameError> {
    check_length(payload.len(), limit)
}

fn check_length(length: usize, limit: usize) -> Result<(), FrameError> {
    if length == 0 {
        return Err(FrameError::Empty);
    }
    if length > limit {
        return Err(FrameError::TooLarge { length, limit });
    }
    Ok(())
}

fn read_exact_until(
    stream: &UnixStream,
    buffer: &mut [u8],
    deadline: Instant,
) -> Result<(), FrameError> {
    let fd = stream.as_raw_fd();
    let mut offset = 0;
    while offset < buffer.len() {
        wait_fd(fd, POLLIN, deadline)?;
        let result = unsafe {
            recv(
                fd,
                buffer[offset..].as_mut_ptr().cast(),
                buffer.len() - offset,
                MSG_DONTWAIT,
            )
        };
        if result > 0 {
            offset += result as usize;
            continue;
        }
        if result == 0 {
            return Err(FrameError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "backend closed the connection before the frame was complete",
            )));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted || error.kind() == io::ErrorKind::WouldBlock {
            continue;
        }
        return Err(FrameError::Io(error));
    }
    Ok(())
}

fn write_all_until(
    stream: &UnixStream,
    buffer: &[u8],
    deadline: Instant,
) -> Result<usize, (FrameError, usize)> {
    let fd = stream.as_raw_fd();
    let mut offset = 0;
    while offset < buffer.len() {
        if let Err(error) = wait_fd(fd, POLLOUT, deadline) {
            return Err((error, offset));
        }
        let result = unsafe {
            send(
                fd,
                buffer[offset..].as_ptr().cast(),
                buffer.len() - offset,
                MSG_DONTWAIT | MSG_NOSIGNAL,
            )
        };
        if result > 0 {
            offset += result as usize;
            continue;
        }
        if result == 0 {
            return Err((
                FrameError::Io(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "backend frame write made no progress",
                )),
                offset,
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted || error.kind() == io::ErrorKind::WouldBlock {
            continue;
        }
        return Err((FrameError::Io(error), offset));
    }
    Ok(offset)
}

fn wait_fd(fd: c_int, events: i16, deadline: Instant) -> Result<(), FrameError> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(FrameError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "backend request deadline expired",
            )));
        }
        let millis = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut descriptor = pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { poll(&mut descriptor, 1, millis) };
        if result > 0 {
            if descriptor.revents & POLLNVAL != 0 {
                return Err(FrameError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "backend socket is invalid",
                )));
            }
            if descriptor.revents & POLLERR != 0 {
                return Err(FrameError::Io(io::Error::other(
                    "backend socket poll failed",
                )));
            }
            if descriptor.revents & (events | POLLHUP) != 0 {
                return Ok(());
            }
            continue;
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(FrameError::Io(error));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        read_json_frame, read_json_frame_until, write_json_frame, write_json_frame_until,
        FrameError,
    };
    use serde::{Deserialize, Serialize};
    use std::{io::Cursor, os::unix::net::UnixStream, time::Instant};

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct Message {
        value: String,
    }

    #[test]
    fn round_trips_and_enforces_length_and_truncation() {
        let message = Message {
            value: "hello".to_owned(),
        };
        let mut frame = Vec::new();
        write_json_frame(&mut frame, &message, 64).unwrap();
        assert_eq!(
            read_json_frame::<Message>(&mut Cursor::new(&frame), 64).unwrap(),
            message
        );
        assert!(matches!(
            read_json_frame::<Message>(&mut Cursor::new([0, 0, 0, 0]), 64),
            Err(FrameError::Empty)
        ));
        assert!(matches!(
            read_json_frame::<Message>(&mut Cursor::new([0, 0, 0, 65]), 64),
            Err(FrameError::TooLarge {
                length: 65,
                limit: 64
            })
        ));
        assert!(matches!(
            read_json_frame::<Message>(&mut Cursor::new([0, 0, 0, 2, b'{']), 64),
            Err(FrameError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn deadline_reads_stop_when_the_deadline_expires() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let result = read_json_frame_until::<Message>(&stream, Instant::now(), 64);
        assert!(matches!(
            result,
            Err(FrameError::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn deadline_writes_report_timeout_before_sending_any_bytes() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let failure = write_json_frame_until(
            &stream,
            &Message {
                value: "hello".to_owned(),
            },
            Instant::now(),
            64,
        )
        .unwrap_err();
        assert_eq!(failure.bytes_written, 0);
        assert!(matches!(
            failure.error,
            FrameError::Io(error) if error.kind() == std::io::ErrorKind::TimedOut
        ));
    }
}
