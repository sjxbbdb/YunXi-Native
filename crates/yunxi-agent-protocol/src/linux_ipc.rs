//! Length-prefixed IPC primitives for the Linux Host.
//!
//! The Linux daemon deliberately uses a small, explicit framing layer instead
//! of newline-delimited JSON. Newlines are valid inside JSON strings and a
//! length prefix lets the host reject oversized frames before allocating the
//! payload buffer.

use serde::{Serialize, de::DeserializeOwned};

/// Version negotiated by a Linux Host client and daemon.
/// Version 2 adds numbered `Event` replay frames and run-based `Follow`.
pub const LINUX_IPC_PROTOCOL_VERSION: u16 = 2;
/// Hard upper bound for a single request or event frame.
pub const LINUX_IPC_MAX_FRAME_BYTES: usize = 24 * 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IpcFrameError {
    #[error("IPC frame exceeds the {max} byte limit: {actual}")]
    Oversized { actual: usize, max: usize },
    #[error("IPC frame length header is invalid: {length}")]
    InvalidLength { length: u32 },
    #[error("IPC frame contains {actual} bytes; expected exactly {expected}")]
    LengthMismatch { actual: usize, expected: usize },
    #[error("IPC JSON encoding failed: {0}")]
    Encode(String),
    #[error("IPC JSON decoding failed: {0}")]
    Decode(String),
}

/// Encode one JSON value as `[u32 big-endian length][UTF-8 JSON payload]`.
pub fn encode_json_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, IpcFrameError> {
    let payload =
        serde_json::to_vec(value).map_err(|error| IpcFrameError::Encode(error.to_string()))?;
    validate_payload_len(payload.len())?;
    let length = u32::try_from(payload.len()).map_err(|_| IpcFrameError::Oversized {
        actual: payload.len(),
        max: LINUX_IPC_MAX_FRAME_BYTES,
    })?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decode the payload portion of a frame after the length prefix was consumed.
pub fn decode_json_frame<T: DeserializeOwned>(payload: &[u8]) -> Result<T, IpcFrameError> {
    validate_payload_len(payload.len())?;
    serde_json::from_slice(payload).map_err(|error| IpcFrameError::Decode(error.to_string()))
}

/// Validate a decoded big-endian length before allocating a payload buffer.
pub fn validate_frame_length(length: u32) -> Result<usize, IpcFrameError> {
    let length = usize::try_from(length).map_err(|_| IpcFrameError::InvalidLength { length })?;
    validate_payload_len(length)?;
    Ok(length)
}

fn validate_payload_len(length: usize) -> Result<(), IpcFrameError> {
    if length == 0 || length > LINUX_IPC_MAX_FRAME_BYTES {
        return Err(IpcFrameError::Oversized {
            actual: length,
            max: LINUX_IPC_MAX_FRAME_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Probe {
        text: String,
    }

    #[test]
    fn length_prefixed_json_round_trips() {
        let value = Probe {
            text: "line one\nline two".to_string(),
        };
        let frame = encode_json_frame(&value).expect("encode");
        let length = u32::from_be_bytes(frame[..4].try_into().expect("header"));
        assert_eq!(
            validate_frame_length(length).expect("length"),
            frame.len() - 4
        );
        assert_eq!(
            decode_json_frame::<Probe>(&frame[4..]).expect("decode"),
            value
        );
    }

    #[test]
    fn zero_and_oversized_frames_are_rejected_before_decode() {
        assert!(validate_frame_length(0).is_err());
        let error = validate_frame_length((LINUX_IPC_MAX_FRAME_BYTES + 1) as u32)
            .expect_err("oversized frame");
        assert!(matches!(error, IpcFrameError::Oversized { .. }));
    }

    #[test]
    fn malformed_json_is_reported_as_decode_error() {
        let error = decode_json_frame::<Probe>(b"not-json").expect_err("decode error");
        assert!(matches!(error, IpcFrameError::Decode(_)));
    }
}
