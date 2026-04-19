use bcmr::core::protocol::{decode_message, encode_message, ListEntry, Message, PROTOCOL_VERSION};
use proptest::prelude::*;

fn arb_string() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..128).prop_map(|v| v.into_iter().collect())
}

fn arb_hash() -> impl Strategy<Value = [u8; 32]> {
    prop::array::uniform32(any::<u8>())
}

fn arb_list_entry() -> impl Strategy<Value = ListEntry> {
    (arb_string(), any::<u64>(), any::<bool>()).prop_map(|(path, size, is_dir)| ListEntry {
        path,
        size,
        is_dir,
    })
}

fn arb_message() -> impl Strategy<Value = Message> {
    prop_oneof![
        (any::<u8>(), any::<u8>()).prop_map(|(version, caps)| Message::Hello { version, caps }),
        arb_string().prop_map(|path| Message::List { path }),
        arb_string().prop_map(|path| Message::Stat { path }),
        (arb_string(), any::<u64>(), prop::option::of(any::<u64>())).prop_map(
            |(path, offset, limit)| Message::Hash {
                path,
                offset,
                limit
            }
        ),
        (arb_string(), any::<u64>()).prop_map(|(path, offset)| Message::Get { path, offset }),
        (arb_string(), any::<u64>()).prop_map(|(path, size)| Message::Put { path, size }),
        arb_string().prop_map(|path| Message::Mkdir { path }),
        arb_string().prop_map(|path| Message::Resume { path }),
        Just(Message::Done),
        Just(Message::OpenDirectChannel),
        arb_hash().prop_map(|mac| Message::AuthHello { mac }),
        arb_hash().prop_map(|nonce| Message::AuthChallenge { nonce }),
        (arb_string(), any::<u64>(), any::<u64>()).prop_map(|(path, offset, length)| {
            Message::PutChunked {
                path,
                offset,
                length,
            }
        }),
        (arb_string(), any::<u64>(), any::<u64>()).prop_map(|(path, offset, length)| {
            Message::GetChunked {
                path,
                offset,
                length,
            }
        }),
        (arb_string(), any::<u64>()).prop_map(|(path, size)| Message::Truncate { path, size }),
        (any::<u8>(), any::<u8>()).prop_map(|(version, caps)| Message::Welcome { version, caps }),
        prop::option::of(arb_string()).prop_map(|hash| {
            let hash = hash.map(|s| format!("{:0<64.64}", s));
            Message::Ok { hash }
        }),
        arb_string().prop_map(|message| Message::Error { message }),
        prop::collection::vec(any::<u8>(), 0..256).prop_map(|payload| Message::Data { payload }),
        (
            any::<u8>(),
            any::<u32>(),
            prop::collection::vec(any::<u8>(), 0..256)
        )
            .prop_map(|(algo, original_size, payload)| Message::DataCompressed {
                algo,
                original_size,
                payload,
            }),
        (any::<u32>(), prop::collection::vec(arb_hash(), 0..16))
            .prop_map(|(block_size, hashes)| { Message::HaveBlocks { block_size, hashes } }),
        prop::collection::vec(any::<u8>(), 0..32).prop_map(|bits| Message::MissingBlocks { bits }),
        (any::<u64>(), any::<i64>(), any::<bool>()).prop_map(|(size, mtime, is_dir)| {
            Message::StatResponse {
                size,
                mtime,
                is_dir,
            }
        }),
        arb_hash().prop_map(|h| Message::HashResponse {
            hash: h.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
        }),
        prop::collection::vec(arb_list_entry(), 0..16)
            .prop_map(|entries| Message::ListResponse { entries }),
        (any::<u64>(), prop::option::of(arb_string())).prop_map(|(size, block_hash)| {
            let block_hash = block_hash.map(|s| format!("{:0<64.64}", s));
            Message::ResumeResponse { size, block_hash }
        }),
        (arb_string(), arb_hash())
            .prop_map(|(addr, session_key)| { Message::DirectChannelReady { addr, session_key } }),
    ]
}

proptest! {
    #[test]
    fn encode_decode_roundtrip(msg in arb_message()) {
        let bytes = encode_message(&msg);
        let back = decode_message(&bytes).expect("well-formed frame must decode");
        prop_assert_eq!(msg, back);
    }

    #[test]
    fn decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        let _ = decode_message(&bytes);
    }

    #[test]
    fn decode_handles_length_prefix_without_payload(len in 0u32..1024) {
        let mut buf = len.to_le_bytes().to_vec();
        let _ = decode_message(&buf);
        buf.extend_from_slice(&[0u8; 4]);
        let _ = decode_message(&buf);
    }

    #[test]
    fn protocol_version_fixed(_ in Just(())) {
        prop_assert_eq!(PROTOCOL_VERSION, 1);
    }
}
