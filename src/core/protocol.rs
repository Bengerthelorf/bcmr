//! Wire frame: `[u32 LE payload_len] [u8 type] [payload…]`. Integers are
//! little-endian, strings are `[u32 len][bytes]`, optional fields are
//! tag-prefixed. Type bytes 0x01–0x0f are client→server, 0x81–0x8c are
//! server→client.

mod codec;
mod io;

pub use codec::{decode_message, encode_message};
pub use io::{read_message, write_message};

pub const PROTOCOL_VERSION: u8 = 1;

pub(super) const TYPE_HELLO: u8 = 0x01;
pub(super) const TYPE_LIST: u8 = 0x02;
pub(super) const TYPE_STAT: u8 = 0x03;
pub(super) const TYPE_HASH: u8 = 0x04;
pub(super) const TYPE_GET: u8 = 0x05;
pub(super) const TYPE_PUT: u8 = 0x06;
pub(super) const TYPE_MKDIR: u8 = 0x07;
pub(super) const TYPE_RESUME: u8 = 0x08;
pub(super) const TYPE_DONE: u8 = 0x09;
pub(super) const TYPE_HAVE_BLOCKS: u8 = 0x0a;
pub(super) const TYPE_OPEN_DIRECT: u8 = 0x0b;
pub(super) const TYPE_AUTH_HELLO: u8 = 0x0c;
pub(super) const TYPE_PUT_CHUNKED: u8 = 0x0d;
pub(super) const TYPE_GET_CHUNKED: u8 = 0x0e;
pub(super) const TYPE_TRUNCATE: u8 = 0x0f;

pub(super) const TYPE_WELCOME: u8 = 0x81;
pub(super) const TYPE_OK: u8 = 0x82;
pub(super) const TYPE_ERROR: u8 = 0x83;
pub(super) const TYPE_DATA: u8 = 0x84;
pub(super) const TYPE_STAT_RESPONSE: u8 = 0x85;
pub(super) const TYPE_HASH_RESPONSE: u8 = 0x86;
pub(super) const TYPE_LIST_RESPONSE: u8 = 0x87;
pub(super) const TYPE_RESUME_RESPONSE: u8 = 0x88;
pub(super) const TYPE_DATA_COMPRESSED: u8 = 0x89;
pub(super) const TYPE_MISSING_BLOCKS: u8 = 0x8a;
pub(super) const TYPE_DIRECT_READY: u8 = 0x8b;
pub(super) const TYPE_AUTH_CHALLENGE: u8 = 0x8c;

pub const CAP_DEDUP: u8 = 0x04;

pub const CAP_FAST: u8 = 0x08;

pub const CAP_SYNC: u8 = 0x10;

pub const CAP_DIRECT_TCP: u8 = 0x20;

pub const CAP_AEAD: u8 = 0x40;

pub const CAP_PUT_OFFSET: u8 = 0x80;

pub const CAP_LZ4: u8 = 0x01;
pub const CAP_ZSTD: u8 = 0x02;

pub const AUTH_HELLO_TAG: &[u8] = b"bcmr-direct-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionAlgo {
    None,
    Lz4,
    Zstd,
}

impl CompressionAlgo {
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => CompressionAlgo::Lz4,
            2 => CompressionAlgo::Zstd,
            _ => CompressionAlgo::None,
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            CompressionAlgo::None => 0,
            CompressionAlgo::Lz4 => 1,
            CompressionAlgo::Zstd => 2,
        }
    }

    pub fn negotiate(local: u8, remote: u8) -> Self {
        let both = local & remote;
        if both & CAP_ZSTD != 0 {
            CompressionAlgo::Zstd
        } else if both & CAP_LZ4 != 0 {
            CompressionAlgo::Lz4
        } else {
            CompressionAlgo::None
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct ListEntry {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub is_dir: bool,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Message {
    Hello {
        version: u8,
        caps: u8,
    },
    List {
        path: String,
    },
    Stat {
        path: String,
    },
    Hash {
        path: String,
        offset: u64,
        limit: Option<u64>,
    },
    Get {
        path: String,
        offset: u64,
    },
    Put {
        path: String,
        size: u64,
        offset: u64,
    },
    Mkdir {
        path: String,
    },
    Resume {
        path: String,
    },
    Done,
    OpenDirectChannel,
    AuthHello {
        mac: [u8; 32],
    },
    PutChunked {
        path: String,
        offset: u64,
        length: u64,
    },
    GetChunked {
        path: String,
        offset: u64,
        length: u64,
    },
    Truncate {
        path: String,
        size: u64,
    },

    Welcome {
        version: u8,
        caps: u8,
    },
    Ok {
        hash: Option<String>,
    },
    Error {
        message: String,
    },
    Data {
        payload: Vec<u8>,
    },
    DataCompressed {
        algo: u8,
        original_size: u32,
        payload: Vec<u8>,
    },
    HaveBlocks {
        block_size: u32,
        hashes: Vec<[u8; 32]>,
    },
    /// Bit i (LSB-first in byte i/8) set iff the server lacks hashes[i].
    MissingBlocks {
        bits: Vec<u8>,
    },
    StatResponse {
        size: u64,
        mtime: i64,
        is_dir: bool,
    },
    HashResponse {
        hash: String,
    },
    ListResponse {
        entries: Vec<ListEntry>,
    },
    ResumeResponse {
        size: u64,
        block_hash: Option<String>,
    },
    DirectChannelReady {
        addr: String,
        session_key: [u8; 32],
    },
    AuthChallenge {
        nonce: [u8; 32],
    },
}
