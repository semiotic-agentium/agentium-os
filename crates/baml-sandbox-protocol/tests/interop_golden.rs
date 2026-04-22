//! Golden-frame interop test.
//!
//! Pins the byte layout of a canonical describe request so that any
//! accidental wire-format drift — renamed serde fields, changed tags,
//! reordered struct members, a different codec length encoding —
//! trips a failure immediately instead of surfacing later as a
//! cross-language interop bug. The check is deliberately narrow:
//! decode through the real [`TsrpcChannel`] receive path, then
//! re-encode the typed [`JsonRpcRequest`] and assert byte identity
//! against both the golden body and the full golden frame (length
//! prefix included).
//!
//! If this test fails after a deliberate wire-format change,
//! regenerate `GOLDEN_BODY` / `GOLDEN_FRAME` like this:
//!
//! ```ignore
//! use baml_sandbox_protocol::{JsonRpcRequest, METHOD_DESCRIBE};
//! use serde_json::json;
//! let body = serde_json::to_vec(
//!     &JsonRpcRequest::new(METHOD_DESCRIBE, 1, json!({}))
//! ).unwrap();
//! println!("body = {:?}", std::str::from_utf8(&body).unwrap());
//! println!("len  = 0x{:08x}", body.len() as u32);
//! ```
//!
//! A regen should never be silent. Document the deliberate change in
//! the commit message so consumers on the other end of the wire know
//! to re-sync.

use baml_sandbox_protocol::{JsonRpcRequest, METHOD_DESCRIBE, TsrpcChannel};
use serde_json::{Map, Value};
use tokio::io::{AsyncWriteExt, duplex};

/// Canonical describe request body.
///
/// Field order mirrors the declaration order of [`JsonRpcRequest`]:
/// `jsonrpc, method, id, params`. `serde_json` preserves insertion
/// order when serializing from a typed struct.
const GOLDEN_BODY: &[u8] =
    b"{\"jsonrpc\":\"2.0\",\"method\":\"tool/describe\",\"id\":1,\"params\":{}}";

/// Full frame: 4-byte big-endian length prefix (= 61) followed by
/// [`GOLDEN_BODY`] verbatim.
const GOLDEN_FRAME: &[u8] = &[
    0x00, 0x00, 0x00, 0x3d, // u32 BE length = 61
    b'{', b'"', b'j', b's', b'o', b'n', b'r', b'p', b'c', b'"', b':', b'"', b'2', b'.', b'0', b'"',
    b',', b'"', b'm', b'e', b't', b'h', b'o', b'd', b'"', b':', b'"', b't', b'o', b'o', b'l', b'/',
    b'd', b'e', b's', b'c', b'r', b'i', b'b', b'e', b'"', b',', b'"', b'i', b'd', b'"', b':', b'1',
    b',', b'"', b'p', b'a', b'r', b'a', b'm', b's', b'"', b':', b'{', b'}', b'}',
];

#[tokio::test]
async fn golden_describe_frame_decodes_and_reencodes_byte_identical() {
    // Self-consistency guard: the constants must agree internally
    // before we even touch the codec. Catches cut/paste drift cheaply.
    let expected_len = u32::try_from(GOLDEN_BODY.len())
        .expect("body fits in u32")
        .to_be_bytes();
    assert_eq!(&GOLDEN_FRAME[..4], &expected_len, "length-prefix drift");
    assert_eq!(&GOLDEN_FRAME[4..], GOLDEN_BODY, "frame body drift");

    // Decode through the real receive path: feed the full framed
    // bytes into one end of a duplex stream, read the other end via
    // TsrpcChannel::recv.
    let (mut writer_half, reader_half) = duplex(4096);
    writer_half
        .write_all(GOLDEN_FRAME)
        .await
        .expect("write golden frame");
    drop(writer_half);

    let mut channel = TsrpcChannel::new(reader_half, tokio::io::sink());
    let decoded: Value = channel.recv().await.expect("recv golden frame");

    // Shape sanity before byte-identity — a mismatch here tells you
    // which field drifted, whereas a raw byte diff just points at the
    // wrong offset.
    let request: JsonRpcRequest =
        serde_json::from_value(decoded).expect("decoded frame deserializes as JsonRpcRequest");
    assert_eq!(request.jsonrpc, "2.0");
    assert_eq!(request.method, METHOD_DESCRIBE);
    assert_eq!(request.id, 1);
    assert_eq!(request.params, Value::Object(Map::new()));

    // Byte-identity on both the body and the full framed form.
    let reencoded_body = serde_json::to_vec(&request).expect("reencode request");
    assert_eq!(
        reencoded_body,
        GOLDEN_BODY,
        "body byte-identity drift\nexpected: {}\n     got: {}",
        String::from_utf8_lossy(GOLDEN_BODY),
        String::from_utf8_lossy(&reencoded_body)
    );

    let mut reencoded_frame = Vec::with_capacity(4 + reencoded_body.len());
    reencoded_frame.extend_from_slice(
        &u32::try_from(reencoded_body.len())
            .expect("body fits in u32")
            .to_be_bytes(),
    );
    reencoded_frame.extend_from_slice(&reencoded_body);
    assert_eq!(
        reencoded_frame, GOLDEN_FRAME,
        "full-frame byte-identity drift"
    );
}
