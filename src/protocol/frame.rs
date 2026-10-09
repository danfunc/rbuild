use std::io::{Read, Write};
use thiserror::Error;

/// Maximum allowed frame size: 64 MiB to prevent memory exhaustion / DoS attacks.
pub const MAX_FRAME_SIZE: u32 = 64 * 1024 * 1024;

#[derive(Error, Debug)]
pub enum FrameError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Frame size {0} exceeds maximum limit of {1} bytes")]
    FrameTooLarge(u32, u32),

    #[error("Unexpected end of stream while reading frame")]
    UnexpectedEof,

    #[error("Serialization / deserialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Reads a length-prefixed raw frame from the reader.
///
/// Prefix format: 4-byte big-endian `u32` length followed by payload bytes.
/// Returns `Ok(None)` on clean EOF at frame boundary.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            // Clean EOF when exactly 0 bytes read at the start of a frame
            return Ok(None);
        }
        Err(e) => return Err(FrameError::Io(e)),
    }

    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_SIZE {
        return Err(FrameError::FrameTooLarge(len, MAX_FRAME_SIZE));
    }

    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}

/// Writes a length-prefixed raw frame to the writer and flushes it.
pub fn write_frame<W: Write>(writer: &mut W, data: &[u8]) -> Result<(), FrameError> {
    let len = data.len();
    if len as u64 > MAX_FRAME_SIZE as u64 {
        return Err(FrameError::FrameTooLarge(len as u32, MAX_FRAME_SIZE));
    }

    let len_u32 = len as u32;
    writer.write_all(&len_u32.to_be_bytes())?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
}

/// Serializes a value to JSON and sends it as a length-prefixed frame.
pub fn write_message<W: Write, T: serde::Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), FrameError> {
    let bytes = serde_json::to_vec(message)?;
    write_frame(writer, &bytes)
}

/// Reads a length-prefixed frame and deserializes it from JSON.
/// Returns `Ok(None)` on clean EOF at message boundary.
pub fn read_message<R: Read, T: serde::de::DeserializeOwned>(
    reader: &mut R,
) -> Result<Option<T>, FrameError> {
    let frame = match read_frame(reader)? {
        Some(f) => f,
        None => return Ok(None),
    };
    let msg: T = serde_json::from_slice(&frame)?;
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_frame_roundtrip() {
        let mut buffer = Vec::new();
        let payload = b"hello rbuild protocol framing test";

        write_frame(&mut buffer, payload).unwrap();
        assert_eq!(buffer.len(), 4 + payload.len());

        let mut cursor = Cursor::new(buffer);
        let read_back = read_frame(&mut cursor).unwrap().expect("should read frame");
        assert_eq!(read_back, payload);

        // Subsequent read should hit clean EOF
        assert_eq!(read_frame(&mut cursor).unwrap(), None);
    }

    #[test]
    fn test_frame_size_limit_rejection() {
        let mut buffer = Vec::new();
        let oversized = MAX_FRAME_SIZE + 1;
        buffer.extend_from_slice(&oversized.to_be_bytes());

        let mut cursor = Cursor::new(buffer);
        let result = read_frame(&mut cursor);
        match result {
            Err(FrameError::FrameTooLarge(size, max)) => {
                assert_eq!(size, oversized);
                assert_eq!(max, MAX_FRAME_SIZE);
            }
            other => panic!("Expected FrameTooLarge error, got {:?}", other),
        }
    }

    #[test]
    fn test_message_roundtrip() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct TestMsg {
            foo: String,
            bar: u64,
        }

        let msg = TestMsg {
            foo: "test string".to_string(),
            bar: 123456,
        };

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();

        let mut cursor = Cursor::new(buf);
        let decoded: TestMsg = read_message(&mut cursor).unwrap().expect("decoded msg");
        assert_eq!(decoded, msg);
    }
}
