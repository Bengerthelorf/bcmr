//! Bounded SFTP v3 packet encoding and decoding.
//!
//! Packet/attribute layouts follow draft-ietf-secsh-filexfer-02. OpenSSH
//! extensions are negotiated separately; no longname text is interpreted.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{Attrs, BcmrError, Capabilities, StatusCode, StatusError};

pub(super) const MAX_FRAME_LEN: usize = 1024 * 1024;
pub(super) const MAX_HANDLE_LEN: usize = 256;
pub(super) const MAX_PATH_LEN: usize = 256 * 1024;

pub(super) const INIT: u8 = 1;
const VERSION: u8 = 2;
pub(super) const OPEN: u8 = 3;
pub(super) const CLOSE: u8 = 4;
pub(super) const READ: u8 = 5;
pub(super) const WRITE: u8 = 6;
pub(super) const LSTAT: u8 = 7;
pub(super) const SETSTAT: u8 = 9;
pub(super) const FSETSTAT: u8 = 10;
pub(super) const OPENDIR: u8 = 11;
pub(super) const READDIR: u8 = 12;
pub(super) const REMOVE: u8 = 13;
pub(super) const MKDIR: u8 = 14;
pub(super) const RMDIR: u8 = 15;
pub(super) const REALPATH: u8 = 16;
pub(super) const STAT: u8 = 17;
pub(super) const RENAME: u8 = 18;
pub(super) const READLINK: u8 = 19;
pub(super) const SYMLINK: u8 = 20;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;
pub(super) const EXTENDED: u8 = 200;

const ATTR_SIZE: u32 = 0x0000_0001;
const ATTR_UIDGID: u32 = 0x0000_0002;
const ATTR_PERMISSIONS: u32 = 0x0000_0004;
const ATTR_ACMODTIME: u32 = 0x0000_0008;
const ATTR_EXTENDED: u32 = 0x8000_0000;

#[derive(Clone, Copy)]
pub(super) enum ExpectedReply {
    Status,
    Handle,
    Data { max_len: usize },
    Names,
    Attrs,
}

pub(super) enum Reply {
    Status(StatusError),
    Handle(Vec<u8>),
    Data(Vec<u8>),
    Names(Vec<(Vec<u8>, Attrs)>),
    Attrs(Attrs),
}

pub(super) fn invalid(message: impl Into<String>) -> BcmrError {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("Invalid SFTP reply: {}", message.into()),
    )
    .into()
}

pub(super) async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>, BcmrError> {
    let len = reader.read_u32().await? as usize;
    if len == 0 || len > MAX_FRAME_LEN {
        return Err(invalid(format!(
            "frame length {len} is outside 1..={MAX_FRAME_LEN}"
        )));
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}

pub(super) async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
) -> Result<(), BcmrError> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME_LEN {
        return Err(BcmrError::InvalidInput(
            "SFTP request exceeds the packet limit".into(),
        ));
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub(super) fn put_string(bytes: &[u8], output: &mut Vec<u8>) {
    // Callers validate paths, handles and data before constructing a request.
    debug_assert!(bytes.len() <= MAX_FRAME_LEN);
    output.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    output.extend_from_slice(bytes);
}

pub(super) fn encode_attrs(attrs: &Attrs, output: &mut Vec<u8>) -> Result<(), BcmrError> {
    if attrs.uid.is_some() != attrs.gid.is_some() {
        return Err(BcmrError::InvalidInput(
            "SFTP v3 uid and gid must be supplied together".into(),
        ));
    }
    if attrs.atime.is_some() != attrs.mtime.is_some() {
        return Err(BcmrError::InvalidInput(
            "SFTP v3 access and modification times must be supplied together".into(),
        ));
    }
    let flags = (u32::from(attrs.size.is_some()) * ATTR_SIZE)
        | (u32::from(attrs.uid.is_some()) * ATTR_UIDGID)
        | (u32::from(attrs.permissions.is_some()) * ATTR_PERMISSIONS)
        | (u32::from(attrs.atime.is_some()) * ATTR_ACMODTIME);
    output.extend_from_slice(&flags.to_be_bytes());
    if let Some(size) = attrs.size {
        output.extend_from_slice(&size.to_be_bytes());
    }
    if let (Some(uid), Some(gid)) = (attrs.uid, attrs.gid) {
        output.extend_from_slice(&uid.to_be_bytes());
        output.extend_from_slice(&gid.to_be_bytes());
    }
    if let Some(permissions) = attrs.permissions {
        output.extend_from_slice(&permissions.to_be_bytes());
    }
    if let (Some(atime), Some(mtime)) = (attrs.atime, attrs.mtime) {
        output.extend_from_slice(&atime.to_be_bytes());
        output.extend_from_slice(&mtime.to_be_bytes());
    }
    Ok(())
}

pub(super) fn decode_version(bytes: &[u8]) -> Result<Capabilities, BcmrError> {
    let mut reader = Reader::new(bytes)?;
    if reader.byte()? != VERSION {
        return Err(invalid("expected SSH_FXP_VERSION during initialization"));
    }
    if reader.u32()? != 3 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "SFTP server did not negotiate protocol version 3",
        )
        .into());
    }
    let mut capabilities = Capabilities::default();
    while !reader.remaining().is_empty() {
        let name = reader.string()?;
        let version = reader.string()?;
        match (name, version) {
            (b"posix-rename@openssh.com", b"1") => capabilities.posix_rename = true,
            (b"fsync@openssh.com", b"1") => capabilities.fsync = true,
            (b"hardlink@openssh.com", b"1") => capabilities.hardlink = true,
            _ => {}
        }
    }
    Ok(capabilities)
}

pub(super) fn decode_reply(
    bytes: &[u8],
    expected_id: u32,
    expected: ExpectedReply,
) -> Result<Reply, BcmrError> {
    let mut reader = Reader::new(bytes)?;
    let kind = reader.byte()?;
    let id = reader.u32()?;
    if id != expected_id {
        return Err(invalid(format!(
            "request ID {id} does not match {expected_id}"
        )));
    }
    let reply = match (kind, expected) {
        (STATUS, _) => {
            let code = StatusCode::from_u32(reader.u32()?);
            let message = std::str::from_utf8(reader.string()?)
                .map_err(|_| invalid("STATUS message is not UTF-8"))?;
            std::str::from_utf8(reader.string()?)
                .map_err(|_| invalid("STATUS language tag is not UTF-8"))?;
            Reply::Status(StatusError {
                code,
                message: super::safe_diagnostic(message),
            })
        }
        (HANDLE, ExpectedReply::Handle) => {
            let handle = reader.string()?;
            if handle.len() > MAX_HANDLE_LEN {
                return Err(invalid("handle exceeds the 256-byte protocol limit"));
            }
            Reply::Handle(handle.to_vec())
        }
        (DATA, ExpectedReply::Data { max_len }) => {
            let data = reader.string()?;
            if data.is_empty() || data.len() > max_len {
                return Err(invalid(
                    "DATA length is empty or exceeds the requested read length",
                ));
            }
            Reply::Data(data.to_vec())
        }
        (NAME, ExpectedReply::Names) => {
            let count = reader.u32()? as usize;
            // Every entry needs two string lengths plus an attributes flag word.
            // Check before reserving memory using an untrusted count.
            if count == 0 || count > reader.remaining().len() / 12 {
                return Err(invalid("NAME entry count exceeds the packet contents"));
            }
            let mut names = Vec::with_capacity(count);
            for _ in 0..count {
                let name = reader.string()?.to_vec();
                reader.string()?; // The unspecified, presentation-only longname.
                let attrs = reader.attrs()?;
                names.push((name, attrs));
            }
            Reply::Names(names)
        }
        (ATTRS, ExpectedReply::Attrs) => Reply::Attrs(reader.attrs()?),
        _ => return Err(invalid(format!("unexpected packet type {kind}"))),
    };
    reader.finish()?;
    Ok(reply)
}

pub(super) fn validate_child_name(name: &[u8]) -> Result<(), BcmrError> {
    if name.is_empty() || name.contains(&b'/') || name.contains(&0) {
        return Err(invalid(
            "READDIR returned a name containing a path separator, NUL, or no bytes",
        ));
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, BcmrError> {
        if bytes.len() > MAX_FRAME_LEN {
            return Err(invalid("packet exceeds the frame limit"));
        }
        Ok(Self { bytes, offset: 0 })
    }

    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], BcmrError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| invalid("field length overflow"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| invalid("truncated field"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, BcmrError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, BcmrError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("four-byte field"),
        ))
    }

    fn u64(&mut self) -> Result<u64, BcmrError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("eight-byte field"),
        ))
    }

    fn string(&mut self) -> Result<&'a [u8], BcmrError> {
        let length = self.u32()? as usize;
        self.take(length)
    }

    fn attrs(&mut self) -> Result<Attrs, BcmrError> {
        let flags = self.u32()?;
        if flags & !(ATTR_SIZE | ATTR_UIDGID | ATTR_PERMISSIONS | ATTR_ACMODTIME | ATTR_EXTENDED)
            != 0
        {
            return Err(invalid("unknown version-3 attribute flags"));
        }
        let mut attrs = Attrs::default();
        if flags & ATTR_SIZE != 0 {
            attrs.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            attrs.uid = Some(self.u32()?);
            attrs.gid = Some(self.u32()?);
        }
        if flags & ATTR_PERMISSIONS != 0 {
            attrs.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            attrs.atime = Some(self.u32()?);
            attrs.mtime = Some(self.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            let count = self.u32()? as usize;
            if count > self.remaining().len() / 8 {
                return Err(invalid(
                    "attribute extension count exceeds the packet contents",
                ));
            }
            for _ in 0..count {
                self.string()?;
                self.string()?;
            }
        }
        Ok(attrs)
    }

    fn finish(&self) -> Result<(), BcmrError> {
        if !self.remaining().is_empty() {
            return Err(invalid("trailing packet bytes"));
        }
        Ok(())
    }
}
