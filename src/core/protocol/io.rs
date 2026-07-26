use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    decode_message, encode_message, validate_content_block_size, validate_data_message, Message,
    TYPE_DATA,
};

pub async fn write_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &Message,
) -> io::Result<()> {
    validate_data_message(msg)?;
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
    if payload_len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty protocol frame",
        ));
    }

    let mut msg_type = [0u8; 1];
    reader.read_exact(&mut msg_type).await?;
    if msg_type[0] == TYPE_DATA {
        if payload_len < 5 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Data frame is shorter than its inner length header",
            ));
        }
        let mut data_len_buf = [0u8; 4];
        reader.read_exact(&mut data_len_buf).await?;
        let data_len = u32::from_le_bytes(data_len_buf) as usize;
        validate_content_block_size(data_len)?;
        if payload_len != 5 + data_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Data frame length does not match its inner payload length",
            ));
        }
        let mut data = vec![0u8; data_len];
        reader.read_exact(&mut data).await?;
        return Ok(Some(Message::Data { payload: data }));
    }

    let mut payload = vec![0u8; payload_len];
    payload[0] = msg_type[0];
    reader.read_exact(&mut payload[1..]).await?;

    let mut frame = Vec::with_capacity(4 + payload_len);
    frame.extend_from_slice(&len_buf);
    frame.extend_from_slice(&payload);

    decode_message(&frame)
        .map(Some)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed protocol message"))
}
