use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::io as durable_io;

const SESSION_MAGIC: &[u8; 4] = b"BCMR";
const SESSION_VERSION: u8 = 1;
const BLOCK_SIZE: u64 = 4 * 1024 * 1024;
const SESSION_MAX_AGE_SECS: u64 = 7 * 24 * 3600;

/// Persistent session state for crash-safe resume. `block_hashes` enables
/// tail-block verification; `src_{size,mtime,inode}` validate source identity.
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
        let key = format!("{}:{}", src.display(), dst.display());
        let hash = blake3::hash(key.as_bytes());
        let hex = &hash.to_hex()[..16];
        session_dir().join(format!("{}.session", hex))
    }

    pub fn load(src: &Path, dst: &Path) -> Option<Self> {
        let path = Self::session_path(src, dst);
        let data = fs::read(&path).ok()?;
        let session = Self::deserialize(&data)?;

        let age = now_secs().saturating_sub(session.updated_at);
        if age > SESSION_MAX_AGE_SECS {
            let _ = fs::remove_file(&path);
            return None;
        }

        if session.src_path != src || session.dst_path != dst {
            return None;
        }

        Some(session)
    }

    pub fn source_matches(&self, src_size: u64, src_mtime: u64, src_inode: u64) -> bool {
        self.src_size == src_size && self.src_mtime == src_mtime && self.src_inode == src_inode
    }

    /// Atomic write → fsync → rename.
    pub fn save(&self) -> io::Result<()> {
        let dir = session_dir();
        fs::create_dir_all(&dir)?;

        let path = Self::session_path(&self.src_path, &self.dst_path);
        let tmp_path = path.with_extension("tmp");

        let data = self.serialize();
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(&data)?;
        durable_io::durable_sync(&f)?;
        drop(f);

        fs::rename(&tmp_path, &path)?;
        durable_io::fsync_dir(&dir);

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

    /// Walks backward from the last recorded block, returning the offset
    /// immediately after the last block whose hash still matches the dst.
    /// O(1) in the common case (tail block only).
    pub fn find_resume_offset(&self, dst: &Path) -> u64 {
        use std::io::Read;

        let mut file = match fs::File::open(dst) {
            Ok(f) => f,
            Err(_) => return 0,
        };

        let dst_len = match file.metadata() {
            Ok(m) => m.len(),
            Err(_) => return 0,
        };

        let mut buf = vec![0u8; BLOCK_SIZE as usize];
        for i in (0..self.block_hashes.len()).rev() {
            let block_offset = i as u64 * BLOCK_SIZE;

            let block_end = block_offset + BLOCK_SIZE;
            if block_end > dst_len {
                continue;
            }

            use std::io::Seek;
            if file.seek(std::io::SeekFrom::Start(block_offset)).is_err() {
                continue;
            }
            let mut read = 0;
            while read < BLOCK_SIZE as usize {
                match file.read(&mut buf[read..BLOCK_SIZE as usize]) {
                    Ok(0) => break,
                    Ok(n) => read += n,
                    Err(_) => return 0,
                }
            }
            if read != BLOCK_SIZE as usize {
                continue;
            }

            let hash = blake3::hash(&buf[..read]);
            if hash.as_bytes() == &self.block_hashes[i] {
                return block_end;
            }
        }

        0
    }

    fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        buf.extend_from_slice(SESSION_MAGIC);
        buf.push(SESSION_VERSION);

        let src_bytes = self.src_path.to_string_lossy().into_owned().into_bytes();
        buf.extend_from_slice(&(src_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&src_bytes);

        let dst_bytes = self.dst_path.to_string_lossy().into_owned().into_bytes();
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

        // Trailing BLAKE3[..8] detects bad sectors / partial writes.
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
        let src_path = PathBuf::from(String::from_utf8_lossy(src_bytes).into_owned());

        let dst_len = r.read_u32()? as usize;
        let dst_bytes = r.read_bytes(dst_len)?;
        let dst_path = PathBuf::from(String::from_utf8_lossy(dst_bytes).into_owned());

        let src_size = r.read_u64()?;
        let src_mtime = r.read_u64()?;
        let src_inode = r.read_u64()?;

        let bytes_written = r.read_u64()?;

        let block_count = r.read_u32()? as usize;
        let mut block_hashes = Vec::with_capacity(block_count);
        for _ in 0..block_count {
            let hash_bytes = r.read_bytes(32)?;
            let mut hash = [0u8; 32];
            hash.copy_from_slice(hash_bytes);
            block_hashes.push(hash);
        }

        let has_src_hash = r.read_u8()?;
        let src_hash = if has_src_hash == 1 {
            let h = r.read_bytes(32)?;
            let mut hash = [0u8; 32];
            hash.copy_from_slice(h);
            Some(hash)
        } else {
            None
        };

        let created_at = r.read_u64()?;
        let updated_at = r.read_u64()?;

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

fn session_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "bcmr")
        .map(|d| d.data_local_dir().join("sessions"))
        .unwrap_or_else(|| PathBuf::from("/tmp/bcmr-sessions"))
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
        if self.pos + n > self.data.len() {
            return None;
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Some(slice)
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

/// 16 blocks = 64 MiB. Ablation: ~4% overhead on Linux, ~16% on macOS.
pub const CHECKPOINT_INTERVAL_BLOCKS: u32 = 16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_roundtrip() {
        let src = Path::new("/tmp/test_src.bin");
        let dst = Path::new("/tmp/test_dst.bin");

        let mut session = Session::new(src, dst, 1024 * 1024, 1700000000, 12345);
        session.add_block([0xAA; 32], BLOCK_SIZE);
        session.add_block([0xBB; 32], BLOCK_SIZE);
        session.set_src_hash([0xCC; 32]);

        let data = session.serialize();
        let restored = Session::deserialize(&data).unwrap();

        assert_eq!(restored.src_path, src);
        assert_eq!(restored.dst_path, dst);
        assert_eq!(restored.src_size, 1024 * 1024);
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

        let loaded = Session::load(&src, &dst).unwrap();
        assert_eq!(loaded.src_size, 5);
        assert_eq!(loaded.src_inode, 99);
        assert_eq!(loaded.block_hashes.len(), 1);

        Session::remove(&src, &dst);
    }
}
