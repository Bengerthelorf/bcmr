use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{decode_message, encode_message, Message};

pub async fn write_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &Message,
) -> io::Result<()> {
    // DirectChannelReady carries a session key; Zeroizing scrubs the heap.
    if matches!(msg, Message::DirectChannelReady { .. }) {
        let frame = zeroize::Zeroizing::new(encode_message(msg));
        writer.write_all(&frame).await
    } else {
        let frame = encode_message(msg);
        writer.write_all(&frame).await
    }
}

pub async fn read_message<R: AsyncReadExt + Unpin>(reader: &mut R) -> io::Result<Option<Message>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }

    let payload_len = u32::from_le_bytes(len_buf) as usize;

    const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;
    if payload_len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "frame too large: {} bytes (max {})",
                payload_len, MAX_FRAME_SIZE
            ),
        ));
    }

    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload).await?;

    let mut frame = Vec::with_capacity(4 + payload_len);
    frame.extend_from_slice(&len_buf);
    frame.extend_from_slice(&payload);

    decode_message(&frame)
        .map(Some)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed protocol message"))
}
