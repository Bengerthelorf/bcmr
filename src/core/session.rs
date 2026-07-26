use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::io as durable_io;

const SESSION_MAGIC: &[u8; 4] = b"BCMR";
const SESSION_VERSION: u8 = 2;
const BLOCK_SIZE: u64 = 4 * 1024 * 1024;
const SESSION_MAX_AGE_SECS: u64 = 7 * 24 * 3600;
const HASH_LEN: usize = 32;
const SERIALIZE_FIXED_OVERHEAD: usize = 256;

#[derive(Debug)]
pub struct Session {
    pub src_path: PathBuf,
    pub dst_path: PathBuf,
    pub src_size: u64,
    pub src_mtime: u64,
    pub src_inode: u64,
    pub bytes_written: u64,
    pub block_hashes: Vec<[u8; 32]>,
    pub src_hash: Option<[u8; 32]>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl Session {
    // The library exposes this constructor for resume/session tooling and the
    // integration suite. The binary crate shares this module but deliberately
    // no longer constructs final-key sessions while writing private stages.
    #[allow(dead_code)]
    pub fn new(src: &Path, dst: &Path, src_size: u64, src_mtime: u64, src_inode: u64) -> Self {
        let now = now_secs();
        Self {
            src_path: src.to_path_buf(),
            dst_path: dst.to_path_buf(),
            src_size,
            src_mtime,
            src_inode,
            bytes_written: 0,
            block_hashes: Vec::new(),
            src_hash: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn add_block(&mut self, hash: [u8; 32], block_bytes: u64) {
        self.block_hashes.push(hash);
        self.bytes_written += block_bytes;
        self.updated_at = now_secs();
    }

    pub fn set_src_hash(&mut self, hash: [u8; 32]) {
        self.src_hash = Some(hash);
    }

    pub fn session_path(src: &Path, dst: &Path) -> PathBuf {
        let src_bytes = path_to_raw_bytes(src);
        let dst_bytes = path_to_raw_bytes(dst);
        let mut key = Vec::with_capacity(8 + src_bytes.len() + dst_bytes.len());
        key.extend_from_slice(&(src_bytes.len() as u32).to_le_bytes());
        key.extend_from_slice(&src_bytes);
        key.extend_from_slice(&(dst_bytes.len() as u32).to_le_bytes());
        key.extend_from_slice(&dst_bytes);
        let hash = blake3::hash(&key);
        let hex = &hash.to_hex()[..16];
        session_dir().join(format!("{}.session", hex))
    }

    pub fn load(src: &Path, dst: &Path) -> io::Result<Option<Self>> {
        Self::load_impl(src, dst, true)
    }

    pub fn try_load_read_only(src: &Path, dst: &Path) -> io::Result<Option<Self>> {
        Self::load_impl(src, dst, false)
    }

    fn load_impl(src: &Path, dst: &Path, remove_expired: bool) -> io::Result<Option<Self>> {
        let path = Self::session_path(src, dst);
        let data = match fs::read(&path) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let Some(session) = Self::deserialize(&data) else {
            return Ok(None);
        };

        let age = now_secs().saturating_sub(session.updated_at);
        if age > SESSION_MAX_AGE_SECS {
            if remove_expired {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            return Ok(None);
        }

        if session.src_path != src || session.dst_path != dst {
            return Ok(None);
        }

        Ok(Some(session))
    }

    pub fn source_matches(&self, src_size: u64, src_mtime: u64, src_inode: u64) -> bool {
        self.src_size == src_size && self.src_mtime == src_mtime && self.src_inode == src_inode
    }

    pub fn has_valid_resume_structure(&self) -> bool {
        if self.bytes_written > self.src_size {
            return false;
        }

        let partial_bytes = self.bytes_written % BLOCK_SIZE;
        if partial_bytes != 0 && self.bytes_written != self.src_size {
            return false;
        }

        let expected_hashes = self.bytes_written / BLOCK_SIZE + u64::from(partial_bytes != 0);
        usize::try_from(expected_hashes)
            .map(|expected| expected == self.block_hashes.len())
            .unwrap_or(false)
    }

    pub fn save(&self) -> io::Result<()> {
        let dir = session_dir();
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }

        let path = Self::session_path(&self.src_path, &self.dst_path);
        let tmp_path = path.with_extension("tmp");

        let data = self.serialize();
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(&data)?;
        durable_io::durable_sync(&f)?;
        drop(f);

        fs::rename(&tmp_path, &path)?;
        durable_io::durable_sync_dir(&dir)?;

        Ok(())
    }

    pub fn remove(src: &Path, dst: &Path) {
        let path = Self::session_path(src, dst);
        let _ = fs::remove_file(path);
    }

    #[cfg(test)]
    pub fn last_block_hash(&self) -> Option<&[u8; 32]> {
        self.block_hashes.last()
    }

    #[cfg(test)]
    pub fn last_block_offset(&self) -> u64 {
        if self.block_hashes.is_empty() {
            0
        } else {
            (self.block_hashes.len() as u64 - 1) * BLOCK_SIZE
        }
    }

    pub fn find_verified_resume_offset(&self, src: &Path, dst: &Path) -> io::Result<u64> {
        self.find_verified_resume_offset_file(src, fs::File::open(dst)?)
    }

    pub fn find_verified_resume_offset_file(
        &self,
        src: &Path,
        mut dst_file: fs::File,
    ) -> io::Result<u64> {
        use std::io::{Read, Seek, SeekFrom};

        let mut src_file = fs::File::open(src)?;
        dst_file.seek(SeekFrom::Start(0))?;
        let src_len = src_file.metadata()?.len();
        let dst_len = dst_file.metadata()?.len();
        if !self.has_valid_resume_structure() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid resume session structure",
            ));
        }
        let proof_limit = self.src_size.min(src_len).min(dst_len);
        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        let mut verified = 0;

        for (i, expected_hash) in self.block_hashes.iter().enumerate() {
            let block_start = (i as u64)
                .checked_mul(BLOCK_SIZE)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "session overflow"))?;
            let block_len = self
                .bytes_written
                .saturating_sub(block_start)
                .min(BLOCK_SIZE);
            if block_len == 0 {
                break;
            }
            let block_end = block_start
                .checked_add(block_len)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "session overflow"))?;
            if block_end > proof_limit {
                break;
            }
            let block_len = block_len as usize;

            src_file.read_exact(&mut buf[..block_len])?;
            if blake3::hash(&buf[..block_len]).as_bytes() != expected_hash {
                break;
            }

            dst_file.read_exact(&mut buf[..block_len])?;
            if blake3::hash(&buf[..block_len]).as_bytes() != expected_hash {
                break;
            }

            verified = block_end;
        }

        Ok(verified)
    }

    fn serialize(&self) -> Vec<u8> {
        let src_bytes = path_to_raw_bytes(&self.src_path);
        let dst_bytes = path_to_raw_bytes(&self.dst_path);
        let capacity = SERIALIZE_FIXED_OVERHEAD
            + src_bytes.len()
            + dst_bytes.len()
            + HASH_LEN * self.block_hashes.len();
        let mut buf = Vec::with_capacity(capacity);

        buf.extend_from_slice(SESSION_MAGIC);
        buf.push(SESSION_VERSION);

        buf.extend_from_slice(&(src_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&src_bytes);

        buf.extend_from_slice(&(dst_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&dst_bytes);

        buf.extend_from_slice(&self.src_size.to_le_bytes());
        buf.extend_from_slice(&self.src_mtime.to_le_bytes());
        buf.extend_from_slice(&self.src_inode.to_le_bytes());

        buf.extend_from_slice(&self.bytes_written.to_le_bytes());

        buf.extend_from_slice(&(self.block_hashes.len() as u32).to_le_bytes());
        for hash in &self.block_hashes {
            buf.extend_from_slice(hash);
        }

        match &self.src_hash {
            Some(h) => {
                buf.push(1);
                buf.extend_from_slice(h);
            }
            None => {
                buf.push(0);
            }
        }

        buf.extend_from_slice(&self.created_at.to_le_bytes());
        buf.extend_from_slice(&self.updated_at.to_le_bytes());

        let checksum = blake3::hash(&buf);
        buf.extend_from_slice(&checksum.as_bytes()[..8]);

        buf
    }

    fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }

        let (payload, stored_checksum) = data.split_at(data.len() - 8);
        let computed = blake3::hash(payload);
        if &computed.as_bytes()[..8] != stored_checksum {
            return None;
        }

        let mut r = Reader::new(payload);

        let magic = r.read_bytes(4)?;
        if magic != SESSION_MAGIC {
            return None;
        }
        let version = r.read_u8()?;
        if version != SESSION_VERSION {
            return None;
        }

        let src_len = r.read_u32()? as usize;
        let src_bytes = r.read_bytes(src_len)?;
        let src_path = raw_bytes_to_path(src_bytes);

        let dst_len = r.read_u32()? as usize;
        let dst_bytes = r.read_bytes(dst_len)?;
        let dst_path = raw_bytes_to_path(dst_bytes);

        let src_size = r.read_u64()?;
        let src_mtime = r.read_u64()?;
        let src_inode = r.read_u64()?;

        let bytes_written = r.read_u64()?;

        let block_count = r.read_u32()? as usize;
        let max_blocks = src_size / BLOCK_SIZE + u64::from(src_size % BLOCK_SIZE != 0);
        if u64::try_from(block_count).ok()? > max_blocks {
            return None;
        }
        let serialized_hash_bytes = block_count.checked_mul(HASH_LEN)?;
        const MIN_TRAILING_FIELDS: usize = 1 + 8 + 8;
        if serialized_hash_bytes > r.remaining().saturating_sub(MIN_TRAILING_FIELDS) {
            return None;
        }
        let mut block_hashes = Vec::with_capacity(block_count);
        for _ in 0..block_count {
            let hash_bytes = r.read_bytes(32)?;
            let mut hash = [0u8; 32];
            hash.copy_from_slice(hash_bytes);
            block_hashes.push(hash);
        }

        let has_src_hash = r.read_u8()?;
        let src_hash = match has_src_hash {
            1 => {
                let h = r.read_bytes(32)?;
                let mut hash = [0u8; 32];
                hash.copy_from_slice(h);
                Some(hash)
            }
            0 => None,
            _ => return None,
        };

        let created_at = r.read_u64()?;
        let updated_at = r.read_u64()?;
        if r.remaining() != 0 {
            return None;
        }

        Some(Self {
            src_path,
            dst_path,
            src_size,
            src_mtime,
            src_inode,
            bytes_written,
            block_hashes,
            src_hash,
            created_at,
            updated_at,
        })
    }
}

#[cfg(unix)]
fn path_to_raw_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn raw_bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn path_to_raw_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().into_owned().into_bytes()
}

#[cfg(not(unix))]
fn raw_bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

fn session_dir() -> PathBuf {
    if let Some(d) = directories::ProjectDirs::from("", "", "bcmr") {
        return d.data_local_dir().join("sessions");
    }
    #[cfg(unix)]
    let suffix = format!("bcmr-sessions-{}", unsafe { libc::getuid() });
    #[cfg(not(unix))]
    let suffix = String::from("bcmr-sessions");
    std::env::temp_dir().join(suffix)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.data.len() {
            return None;
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Some(slice)
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn read_u8(&mut self) -> Option<u8> {
        let b = self.read_bytes(1)?;
        Some(b[0])
    }

    fn read_u32(&mut self) -> Option<u32> {
        let b = self.read_bytes(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_u64(&mut self) -> Option<u64> {
        let b = self.read_bytes(8)?;
        Some(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

pub const COPY_BLOCK_SIZE: u64 = BLOCK_SIZE;

pub const CHECKPOINT_INTERVAL_BLOCKS: u32 = 16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_roundtrip() {
        let src = Path::new("/tmp/test_src.bin");
        let dst = Path::new("/tmp/test_dst.bin");

        let mut session = Session::new(src, dst, 2 * BLOCK_SIZE, 1700000000, 12345);
        session.add_block([0xAA; 32], BLOCK_SIZE);
        session.add_block([0xBB; 32], BLOCK_SIZE);
        session.set_src_hash([0xCC; 32]);

        let data = session.serialize();
        let restored = Session::deserialize(&data).unwrap();

        assert_eq!(restored.src_path, src);
        assert_eq!(restored.dst_path, dst);
        assert_eq!(restored.src_size, 2 * BLOCK_SIZE);
        assert_eq!(restored.src_mtime, 1700000000);
        assert_eq!(restored.src_inode, 12345);
        assert_eq!(restored.bytes_written, BLOCK_SIZE * 2);
        assert_eq!(restored.block_hashes.len(), 2);
        assert_eq!(restored.block_hashes[0], [0xAA; 32]);
        assert_eq!(restored.block_hashes[1], [0xBB; 32]);
        assert_eq!(restored.src_hash.unwrap(), [0xCC; 32]);
    }

    #[test]
    fn test_session_invalid_magic() {
        let data = b"NOPE\x01";
        assert!(Session::deserialize(data).is_none());
    }

    #[test]
    fn test_session_empty_data() {
        assert!(Session::deserialize(&[]).is_none());
    }

    #[test]
    fn test_session_trailing_payload_rejected() {
        let session = Session::new(Path::new("/a"), Path::new("/b"), 0, 0, 0);
        let encoded = session.serialize();
        let mut payload = encoded[..encoded.len() - 8].to_vec();
        payload.push(0xAA);
        let checksum = blake3::hash(&payload);
        payload.extend_from_slice(&checksum.as_bytes()[..8]);

        assert!(
            Session::deserialize(&payload).is_none(),
            "a checksummed session must still consume its payload exactly"
        );
    }

    #[test]
    fn test_session_source_matches() {
        let session = Session::new(Path::new("/a"), Path::new("/b"), 1000, 2000, 3000);
        assert!(session.source_matches(1000, 2000, 3000));
        assert!(!session.source_matches(999, 2000, 3000));
        assert!(!session.source_matches(1000, 2001, 3000));
        assert!(!session.source_matches(1000, 2000, 3001));
    }

    #[test]
    fn test_session_last_block() {
        let mut session = Session::new(Path::new("/a"), Path::new("/b"), 0, 0, 0);
        assert!(session.last_block_hash().is_none());
        assert_eq!(session.last_block_offset(), 0);

        session.add_block([1; 32], BLOCK_SIZE);
        assert_eq!(*session.last_block_hash().unwrap(), [1; 32]);
        assert_eq!(session.last_block_offset(), 0);

        session.add_block([2; 32], BLOCK_SIZE);
        assert_eq!(*session.last_block_hash().unwrap(), [2; 32]);
        assert_eq!(session.last_block_offset(), BLOCK_SIZE);
    }

    #[cfg(unix)]
    #[test]
    fn test_non_utf8_path_roundtrip() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let raw = vec![b'/', 0xff, 0xfe, 0x80, b'a'];
        let path = PathBuf::from(OsString::from_vec(raw.clone()));
        let session = Session::new(&path, &path, 42, 1700000000, 7);
        let data = session.serialize();
        let restored = Session::deserialize(&data).unwrap();
        assert_eq!(path_to_raw_bytes(&restored.src_path), raw);
        assert_eq!(path_to_raw_bytes(&restored.dst_path), raw);
    }

    #[cfg(unix)]
    #[test]
    fn test_session_path_stable_for_non_utf8_inputs() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let src_a = PathBuf::from(OsString::from_vec(vec![b'/', 0xff, 0xfe, b'a']));
        let dst_a = PathBuf::from(OsString::from_vec(vec![b'/', 0xc3, 0x28, b'b']));
        let src_b = PathBuf::from(OsString::from_vec(vec![b'/', 0xff, 0xfe, b'a']));
        let dst_b = PathBuf::from(OsString::from_vec(vec![b'/', 0xc3, 0x28, b'b']));
        assert_eq!(
            Session::session_path(&src_a, &dst_a),
            Session::session_path(&src_b, &dst_b)
        );

        let other = PathBuf::from(OsString::from_vec(vec![b'/', 0xff, 0xff, b'a']));
        assert_ne!(
            Session::session_path(&src_a, &dst_a),
            Session::session_path(&other, &dst_a)
        );
    }

    #[test]
    fn test_session_path_distinct_when_colon_at_boundary() {
        let sp1 = Session::session_path(Path::new("a:"), Path::new("b"));
        let sp2 = Session::session_path(Path::new("a"), Path::new(":b"));
        assert_ne!(sp1, sp2);
    }

    #[test]
    fn test_v1_session_rejected() {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(SESSION_MAGIC);
        buf.push(1);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        for _ in 0..4 {
            buf.extend_from_slice(&0u64.to_le_bytes());
        }
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.push(0);
        for _ in 0..2 {
            buf.extend_from_slice(&0u64.to_le_bytes());
        }
        let cs = blake3::hash(&buf);
        buf.extend_from_slice(&cs.as_bytes()[..8]);
        assert!(Session::deserialize(&buf).is_none());
    }

    // Non-UTF-8 byte roundtrip: only the Unix path_to_raw_bytes path is
    // lossless. The Windows fallback goes through String::from_utf8_lossy
    // and replaces invalid bytes with U+FFFD by design.
    #[cfg(unix)]
    proptest::proptest! {
        #[test]
        fn session_serde_roundtrip_preserves_fields(
            src_raw in proptest::collection::vec(proptest::prelude::any::<u8>(), 1..64),
            dst_raw in proptest::collection::vec(proptest::prelude::any::<u8>(), 1..64),
            size_seed: u64,
            mtime: u64,
            inode: u64,
            written: u64,
            block_hashes in proptest::collection::vec(proptest::array::uniform32(proptest::prelude::any::<u8>()), 0..8),
            src_hash_opt in proptest::option::of(proptest::array::uniform32(proptest::prelude::any::<u8>())),
        ) {
            let src = raw_bytes_to_path(&src_raw);
            let dst = raw_bytes_to_path(&dst_raw);
            let minimum_size = block_hashes.len() as u64 * BLOCK_SIZE;
            let src_size = size_seed.max(minimum_size);
            let mut s = Session::new(&src, &dst, src_size, mtime, inode);
            s.bytes_written = written;
            for h in &block_hashes { s.block_hashes.push(*h); }
            if let Some(h) = src_hash_opt { s.set_src_hash(h); }

            let bytes = s.serialize();
            let back = Session::deserialize(&bytes).expect("self-produced payload must decode");
            proptest::prop_assert_eq!(path_to_raw_bytes(&back.src_path), src_raw);
            proptest::prop_assert_eq!(path_to_raw_bytes(&back.dst_path), dst_raw);
            proptest::prop_assert_eq!(back.src_size, src_size);
            proptest::prop_assert_eq!(back.src_mtime, mtime);
            proptest::prop_assert_eq!(back.src_inode, inode);
            proptest::prop_assert_eq!(back.bytes_written, written);
            proptest::prop_assert_eq!(back.block_hashes, block_hashes);
            proptest::prop_assert_eq!(back.src_hash, src_hash_opt);
        }
    }

    proptest::proptest! {
        #[test]
        fn session_deserialize_never_panics(
            bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512),
        ) {
            let _ = Session::deserialize(&bytes);
        }
    }

    #[test]
    fn test_session_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, b"hello").unwrap();
        std::fs::write(&dst, b"world").unwrap();

        let mut session = Session::new(&src, &dst, 5, 1700000000, 99);
        session.add_block([0xDD; 32], BLOCK_SIZE);
        session.save().unwrap();

        let loaded = Session::load(&src, &dst).unwrap().unwrap();
        assert_eq!(loaded.src_size, 5);
        assert_eq!(loaded.src_inode, 99);
        assert_eq!(loaded.block_hashes.len(), 1);

        Session::remove(&src, &dst);
    }

    #[cfg(unix)]
    #[test]
    fn test_session_read_error_is_not_treated_as_missing() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        std::fs::write(&src, b"hello").unwrap();
        std::fs::write(&dst, b"world").unwrap();
        let session = Session::new(&src, &dst, 5, 0, 0);
        session.save().unwrap();

        let session_path = Session::session_path(&src, &dst);
        std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = Session::load(&src, &dst);
        std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        Session::remove(&src, &dst);

        assert!(
            result.is_err(),
            "session read failures must not become a destructive no-session fallback"
        );
    }
}
