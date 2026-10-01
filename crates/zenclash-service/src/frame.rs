use std::io::{self, Write};
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum serialized message size, including JSON punctuation, in bytes.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Failure to exchange a bounded JSON message.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The declared or serialized body exceeded the message budget.
    #[error("message exceeds the service frame budget")]
    TooLarge,
    /// The peer did not complete the whole frame before the deadline.
    #[error("service frame timed out")]
    Timeout,
    /// The stream ended or failed while exchanging a frame.
    #[error("service frame transport failed")]
    Io(#[from] io::Error),
    /// The body could not be represented by the expected JSON type.
    #[error("invalid service frame JSON")]
    InvalidJson(#[source] serde_json::Error),
}

/// Reads a length-prefixed message with a deadline for the entire exchange.
///
/// An oversized header is rejected before allocating or reading its body.
/// After any error callers must discard the stream, because a partial frame
/// may have been consumed.
pub async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
    timeout: Duration,
) -> Result<T, FrameError> {
    tokio::time::timeout(timeout, async {
        let length = reader.read_u32_le().await? as usize;
        if length > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge);
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await?;
        serde_json::from_slice(&body).map_err(FrameError::InvalidJson)
    })
    .await
    .map_err(|_| FrameError::Timeout)?
}

struct BoundedBody(Vec<u8>);

impl Write for BoundedBody {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("frame size limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Writes a bounded length-prefixed JSON message.
///
/// Serialization is bounded before any header is written. After a timeout or
/// transport error callers must discard the partially written stream.
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
    timeout: Duration,
) -> Result<(), FrameError> {
    let mut body = BoundedBody(Vec::new());
    serde_json::to_writer(&mut body, value).map_err(|error| {
        if error.is_io() {
            FrameError::TooLarge
        } else {
            FrameError::InvalidJson(error)
        }
    })?;
    tokio::time::timeout(timeout, async {
        writer.write_u32_le(body.0.len() as u32).await?;
        writer.write_all(&body.0).await?;
        writer.flush().await?;
        Ok(())
    })
    .await
    .map_err(|_| FrameError::Timeout)?
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncWriteExt, duplex};

    use super::*;

    #[tokio::test]
    async fn a_declared_oversize_frame_is_rejected_without_reading_its_body() {
        let (mut sender, mut receiver) = duplex(8);
        sender
            .write_all(&((MAX_FRAME_BYTES + 1) as u32).to_le_bytes())
            .await
            .unwrap();
        assert!(matches!(
            read_frame::<_, serde_json::Value>(&mut receiver, Duration::from_secs(1)).await,
            Err(FrameError::TooLarge)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_peer_that_never_finishes_a_body_does_not_hold_a_reader_forever() {
        let (mut sender, mut receiver) = duplex(8);
        sender.write_all(&8_u32.to_le_bytes()).await.unwrap();
        sender.write_all(b"{").await.unwrap();
        assert!(matches!(
            read_frame::<_, serde_json::Value>(&mut receiver, Duration::from_secs(1)).await,
            Err(FrameError::Timeout)
        ));
    }

    #[tokio::test]
    async fn consecutive_messages_preserve_their_boundaries() {
        let (mut sender, mut receiver) = duplex(128);
        let timeout = Duration::from_secs(1);
        write_frame(&mut sender, &vec![1_u32, 2], timeout)
            .await
            .unwrap();
        write_frame(&mut sender, &vec![3_u32], timeout)
            .await
            .unwrap();
        assert_eq!(
            read_frame::<_, Vec<u32>>(&mut receiver, timeout)
                .await
                .unwrap(),
            [1, 2]
        );
        assert_eq!(
            read_frame::<_, Vec<u32>>(&mut receiver, timeout)
                .await
                .unwrap(),
            [3]
        );
    }

    #[tokio::test]
    async fn truncated_and_invalid_messages_are_not_accepted() {
        let timeout = Duration::from_secs(1);
        let (mut sender, mut receiver) = duplex(32);
        sender.write_all(&5_u32.to_le_bytes()).await.unwrap();
        sender.write_all(b"{}").await.unwrap();
        drop(sender);
        assert!(matches!(
            read_frame::<_, serde_json::Value>(&mut receiver, timeout).await,
            Err(FrameError::Io(_))
        ));

        let (mut sender, mut receiver) = duplex(32);
        sender.write_all(&1_u32.to_le_bytes()).await.unwrap();
        sender.write_all(b"!").await.unwrap();
        assert!(matches!(
            read_frame::<_, serde_json::Value>(&mut receiver, timeout).await,
            Err(FrameError::InvalidJson(_))
        ));
    }

    #[tokio::test]
    async fn an_oversize_outgoing_message_writes_no_header() {
        let (mut sender, mut receiver) = duplex(64);
        let value = "a".repeat(MAX_FRAME_BYTES);
        assert!(matches!(
            write_frame(&mut sender, &value, Duration::from_secs(1)).await,
            Err(FrameError::TooLarge)
        ));
        drop(sender);
        assert!(matches!(
            read_frame::<_, serde_json::Value>(&mut receiver, Duration::from_secs(1)).await,
            Err(FrameError::Io(_))
        ));
    }
}
