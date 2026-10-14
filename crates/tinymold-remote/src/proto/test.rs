//! Pins the wire encoding of every message against hand-encoded bytes, so a
//! wrong field number cannot slip through a round trip that would agree with
//! itself.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use prost::Message;

use super::*;

fn digest(hash: &str, size: i64) -> Digest {
    Digest {
        hash: hash.to_owned(),
        size_bytes: size,
    }
}

#[test]
fn a_digest_is_hash_one_and_size_two() {
    assert_eq!(digest("ab", 3).encode_to_vec(), [0x0a, 2, b'a', b'b', 0x10, 3]);
}

#[test]
fn an_action_result_lists_files_at_two_and_the_exit_code_at_four() {
    let result = ActionResult {
        output_files: vec![OutputFile {
            path: "p".to_owned(),
            digest: Some(digest("h", 1)),
            is_executable: true,
        }],
        exit_code: 1,
    };
    // OutputFile: path=1, digest=2, is_executable=4.
    let file = [0x0a, 1, b'p', 0x12, 5, 0x0a, 1, b'h', 0x10, 1, 0x20, 1];
    let mut expected = vec![0x12, file.len() as u8];
    expected.extend_from_slice(&file);
    expected.extend_from_slice(&[0x20, 1]);
    assert_eq!(result.encode_to_vec(), expected);
}

#[test]
fn action_cache_requests_put_the_digest_function_where_reapi_does() {
    let get = GetActionResultRequest {
        instance_name: "i".to_owned(),
        action_digest: Some(digest("h", 0)),
        digest_function: DIGEST_SHA256,
    };
    assert_eq!(
        get.encode_to_vec(),
        [0x0a, 1, b'i', 0x12, 3, 0x0a, 1, b'h', 0x30, 1]
    );

    let update = UpdateActionResultRequest {
        instance_name: String::new(),
        action_digest: Some(digest("h", 0)),
        action_result: Some(ActionResult::default()),
        digest_function: DIGEST_SHA256,
    };
    assert_eq!(
        update.encode_to_vec(),
        [0x12, 3, 0x0a, 1, b'h', 0x1a, 0, 0x28, 1]
    );
}

#[test]
fn find_missing_blobs_sends_digests_at_two_and_reads_them_back_at_two() {
    let request = FindMissingBlobsRequest {
        instance_name: String::new(),
        blob_digests: vec![digest("h", 1)],
        digest_function: DIGEST_SHA256,
    };
    assert_eq!(
        request.encode_to_vec(),
        [0x12, 5, 0x0a, 1, b'h', 0x10, 1, 0x18, 1]
    );
    let response =
        FindMissingBlobsResponse::decode(&[0x12, 5, 0x0a, 1, b'h', 0x10, 1][..]).unwrap();
    assert_eq!(response.missing_blob_digests, [digest("h", 1)]);
}

#[test]
fn capabilities_decode_digest_functions_and_whether_updates_are_enabled() {
    // ServerCapabilities { cache_capabilities (1): { digest_functions (1):
    // [SHA256, BLAKE3] packed, action_cache_update_capabilities (2):
    // { update_enabled (1): true } } }, plus an unknown field (3) to skip.
    let bytes = [
        0x0a, 8, 0x0a, 2, 1, 9, 0x12, 2, 0x08, 1, 0x1a, 0,
    ];
    let capabilities = ServerCapabilities::decode(&bytes[..]).unwrap();
    let cache = capabilities.cache_capabilities.unwrap();
    assert_eq!(cache.digest_functions, [DIGEST_SHA256, DIGEST_BLAKE3]);
    assert!(cache.action_cache_update_capabilities.unwrap().update_enabled);
    assert_eq!(
        GetCapabilitiesRequest {
            instance_name: "i".to_owned()
        }
        .encode_to_vec(),
        [0x0a, 1, b'i']
    );
}

#[test]
fn byte_stream_messages_carry_data_at_ten() {
    let read = ReadRequest {
        resource_name: "r".to_owned(),
        read_offset: 1,
        read_limit: 2,
    };
    assert_eq!(read.encode_to_vec(), [0x0a, 1, b'r', 0x10, 1, 0x18, 2]);
    let chunk = ReadResponse::decode(&[0x52, 1, b'x'][..]).unwrap();
    assert_eq!(chunk.data, b"x");

    let write = WriteRequest {
        resource_name: "r".to_owned(),
        write_offset: 1,
        finish_write: true,
        data: b"x".to_vec(),
    };
    assert_eq!(
        write.encode_to_vec(),
        [0x0a, 1, b'r', 0x10, 1, 0x18, 1, 0x52, 1, b'x']
    );
    assert_eq!(
        WriteResponse::decode(&[0x08, 5][..]).unwrap().committed_size,
        5
    );
}
