//! Frame codec tests (Appendix A.1): bounded, incremental, pure.

use forgedm_protocol::{
    ErrorCode, FrameDecoder, FrameError, MAX_FRAME, decode_json_object, encode_frame,
};

fn header(len: u32) -> [u8; 4] {
    len.to_le_bytes()
}

fn drain(decoder: &mut FrameDecoder) -> Vec<Vec<u8>> {
    std::iter::from_fn(|| decoder.next_frame()).collect()
}

#[test]
fn frame_roundtrip_min_and_max() {
    let min = b"{".to_vec();
    let mut max = vec![b' '; MAX_FRAME as usize];
    max[0] = b'{';

    for body in [min, max] {
        let wire = encode_frame(&body).unwrap();
        assert_eq!(wire.len(), body.len() + 4);
        let mut decoder = FrameDecoder::new();
        decoder.feed(&wire).unwrap();
        assert_eq!(drain(&mut decoder), vec![body]);
        decoder.finish().unwrap();
    }
}

#[test]
fn frame_roundtrip_survives_byte_at_a_time_and_back_to_back_frames() {
    let a = encode_frame(b"{\"a\":1}").unwrap();
    let b = encode_frame(b"{}").unwrap();
    let wire = [a, b].concat();
    let mut decoder = FrameDecoder::new();
    for byte in &wire {
        decoder.feed(std::slice::from_ref(byte)).unwrap();
    }
    assert_eq!(
        drain(&mut decoder),
        vec![b"{\"a\":1}".to_vec(), b"{}".to_vec()]
    );
    decoder.finish().unwrap();
}

#[test]
fn oversized_length_rejected_before_reading_body() {
    let mut decoder = FrameDecoder::new();
    let err = decoder.feed(&header(MAX_FRAME + 1)).unwrap_err();
    assert_eq!(err, FrameError::TooLarge { len: MAX_FRAME + 1 });
    assert_eq!(err.error_code(), ErrorCode::FrameTooLarge);
    assert_eq!(decoder.buffered_bytes(), 0);

    // Header split across chunks, followed by body bytes in the same chunk: still
    // rejected, and none of the trailing body is buffered.
    let mut decoder = FrameDecoder::new();
    decoder.feed(&header(u32::MAX)[..2]).unwrap();
    let mut rest = header(u32::MAX)[2..].to_vec();
    rest.extend_from_slice(&[0x41; 4096]);
    assert_eq!(
        decoder.feed(&rest),
        Err(FrameError::TooLarge { len: u32::MAX })
    );
    assert_eq!(decoder.buffered_bytes(), 0);

    // The decoder stays poisoned.
    assert!(decoder.feed(b"{}").is_err());
}

#[test]
fn zero_length_rejected() {
    let mut decoder = FrameDecoder::new();
    let err = decoder.feed(&header(0)).unwrap_err();
    assert_eq!(err, FrameError::ZeroLength);
    assert_eq!(err.error_code(), ErrorCode::MalformedFrame);
    assert_eq!(encode_frame(b""), Err(FrameError::ZeroLength));
    assert!(matches!(
        encode_frame(&vec![b'{'; MAX_FRAME as usize + 1]),
        Err(FrameError::TooLarge { .. })
    ));
}

#[test]
fn truncated_header_and_body_unexpected_eof() {
    let mut decoder = FrameDecoder::new();
    decoder.feed(&[5, 0]).unwrap();
    assert_eq!(decoder.finish(), Err(FrameError::UnexpectedEof));

    let mut decoder = FrameDecoder::new();
    decoder.feed(&header(10)).unwrap();
    decoder.feed(b"{\"a\"").unwrap();
    assert_eq!(decoder.finish(), Err(FrameError::UnexpectedEof));
    assert_eq!(
        FrameError::UnexpectedEof.error_code(),
        ErrorCode::MalformedFrame
    );

    // A clean boundary is fine.
    let decoder = FrameDecoder::new();
    assert_eq!(decoder.finish(), Ok(()));
}

#[test]
fn invalid_utf8_or_json_is_malformed_frame_not_panic() {
    let cases: [&[u8]; 8] = [
        &[0xff, 0xfe, 0xfd],
        b"{\"a\":",
        b"not json",
        b"[1,2,3]",
        b"\"string\"",
        b"42",
        b"null",
        &[b'{', 0xc3, b'}'],
    ];
    for body in cases {
        assert_eq!(
            decode_json_object(body).unwrap_err().error_code(),
            ErrorCode::MalformedFrame,
            "{body:?}"
        );
    }
    assert!(decode_json_object(b"{\"a\":1}").is_ok());
}
