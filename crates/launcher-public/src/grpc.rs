//! A small gRPC client for the two calls Zeal makes to a light wallet server: `GetLightdInfo`
//! and `GetLatestBlock` of `cash.z.wallet.sdk.rpc.CompactTxStreamer`. It runs over any
//! stream: plain TCP for zebrad's endpoint (HTTP/2 with prior knowledge), or TLS through the
//! front door. The protobuf messages are decoded by hand, with the field numbers of the
//! lightwalletd protocol's `service.proto`.

use bytes::{Buf, Bytes, BytesMut};
use launcher_types::public::LightdInfo;
use tokio::io::{AsyncRead, AsyncWrite};

/// How long one call may take.
pub const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The service's path prefix.
const SERVICE: &str = "/cash.z.wallet.sdk.rpc.CompactTxStreamer";

/// The largest answer Zeal reads.
const MAX_MESSAGE: usize = 64 * 1024;

/// A gRPC call failed.
#[derive(Debug, thiserror::Error)]
pub enum GrpcError {
    #[error("HTTP/2: {0}")]
    H2(#[from] h2::Error),
    #[error("the server answered HTTP {0}")]
    Status(u16),
    #[error("gRPC status {code}: {message}")]
    Grpc { code: u32, message: String },
    #[error("the answer is not a valid message: {0}")]
    Decode(&'static str),
    #[error("no answer within {} s", CALL_TIMEOUT.as_secs())]
    Timeout,
}

/// The tip, from `GetLatestBlock`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockId {
    pub height: u64,
    pub hash: Vec<u8>,
}

/// `GetLightdInfo(Empty)`.
pub async fn get_lightd_info<S>(io: S, authority: &str, tls: bool) -> Result<LightdInfo, GrpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let answer = unary(io, authority, tls, "GetLightdInfo", &[]).await?;
    decode_lightd_info(&answer)
}

/// `GetLatestBlock(ChainSpec)`.
pub async fn get_latest_block<S>(io: S, authority: &str, tls: bool) -> Result<BlockId, GrpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let answer = unary(io, authority, tls, "GetLatestBlock", &[]).await?;
    decode_block_id(&answer)
}

/// One unary call on a new HTTP/2 connection over `io`.
async fn unary<S>(
    io: S,
    authority: &str,
    tls: bool,
    method: &str,
    message: &[u8],
) -> Result<Bytes, GrpcError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let call = async {
        let (client, connection) = h2::client::handshake(io).await?;
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let result = call(client, authority, tls, method, message).await;
        driver.abort();
        result
    };
    tokio::time::timeout(CALL_TIMEOUT, call)
        .await
        .map_err(|_| GrpcError::Timeout)?
}

async fn call(
    client: h2::client::SendRequest<Bytes>,
    authority: &str,
    tls: bool,
    method: &str,
    message: &[u8],
) -> Result<Bytes, GrpcError> {
    let scheme = if tls { "https" } else { "http" };
    let request = http::Request::post(format!("{scheme}://{authority}{SERVICE}/{method}"))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())
        .map_err(|_| GrpcError::Decode("request"))?;
    let mut client = client.ready().await?;
    let (response, mut body) = client.send_request(request, false)?;
    let mut frame = BytesMut::with_capacity(5 + message.len());
    frame.extend_from_slice(&[0]);
    frame.extend_from_slice(&(message.len() as u32).to_be_bytes());
    frame.extend_from_slice(message);
    body.send_data(frame.freeze(), true)?;

    let response = response.await?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(GrpcError::Status(status));
    }
    // A trailers-only answer carries the status in the headers.
    if let Some(code) = grpc_status(response.headers())
        && code != 0
    {
        return Err(GrpcError::Grpc {
            code,
            message: grpc_message(response.headers()),
        });
    }
    let mut stream = response.into_body();
    let mut data = BytesMut::new();
    while let Some(chunk) = stream.data().await {
        let chunk = chunk?;
        let _ = stream.flow_control().release_capacity(chunk.len());
        data.extend_from_slice(&chunk);
        if data.len() > MAX_MESSAGE + 5 {
            return Err(GrpcError::Decode("the answer is too large"));
        }
    }
    let trailers = stream.trailers().await?.unwrap_or_default();
    match grpc_status(&trailers) {
        Some(0) => {}
        Some(code) => {
            return Err(GrpcError::Grpc {
                code,
                message: grpc_message(&trailers),
            });
        }
        None => return Err(GrpcError::Decode("no grpc-status")),
    }
    let mut data = data.freeze();
    if data.len() < 5 {
        return Err(GrpcError::Decode("no message"));
    }
    if data.get_u8() != 0 {
        return Err(GrpcError::Decode("a compressed message"));
    }
    let length = data.get_u32() as usize;
    if length > data.len() {
        return Err(GrpcError::Decode("a cut-off message"));
    }
    Ok(data.split_to(length))
}

fn grpc_status(headers: &http::HeaderMap) -> Option<u32> {
    headers.get("grpc-status")?.to_str().ok()?.parse().ok()
}

fn grpc_message(headers: &http::HeaderMap) -> String {
    headers
        .get("grpc-message")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// One field of a protobuf message.
enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Other,
}

fn varint(input: &mut &[u8]) -> Result<u64, GrpcError> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = input
            .split_first()
            .ok_or(GrpcError::Decode("a cut-off varint"))?;
        *input = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(GrpcError::Decode("a varint longer than 64 bits"))
}

/// The fields of `message`, as `(number, value)`.
fn fields(mut message: &[u8]) -> Result<Vec<(u64, Field<'_>)>, GrpcError> {
    let mut out = Vec::new();
    while !message.is_empty() {
        let key = varint(&mut message)?;
        let number = key >> 3;
        let field = match key & 7 {
            0 => Field::Varint(varint(&mut message)?),
            1 | 5 => {
                let width = if key & 7 == 1 { 8 } else { 4 };
                if message.len() < width {
                    return Err(GrpcError::Decode("a cut-off fixed field"));
                }
                message = &message[width..];
                Field::Other
            }
            2 => {
                let length = varint(&mut message)? as usize;
                if message.len() < length {
                    return Err(GrpcError::Decode("a cut-off field"));
                }
                let (bytes, rest) = message.split_at(length);
                message = rest;
                Field::Bytes(bytes)
            }
            _ => return Err(GrpcError::Decode("an unknown wire type")),
        };
        out.push((number, field));
    }
    Ok(out)
}

fn text(bytes: &[u8]) -> Result<String, GrpcError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| GrpcError::Decode("a string that is not UTF-8"))
}

/// Decodes a `LightdInfo` (lightwalletd protocol `service.proto`): `version` 1, `vendor` 2,
/// `chainName` 4, `saplingActivationHeight` 5, `consensusBranchId` 6, `blockHeight` 7,
/// `zcashdBuild` 13 and `zcashdSubversion` 14; the rest is skipped.
pub fn decode_lightd_info(message: &[u8]) -> Result<LightdInfo, GrpcError> {
    let mut info = LightdInfo::default();
    for (number, field) in fields(message)? {
        match (number, field) {
            (1, Field::Bytes(bytes)) => info.version = text(bytes)?,
            (2, Field::Bytes(bytes)) => info.vendor = text(bytes)?,
            (4, Field::Bytes(bytes)) => info.chain_name = text(bytes)?,
            (5, Field::Varint(value)) => info.sapling_activation_height = value,
            (6, Field::Bytes(bytes)) => info.consensus_branch_id = text(bytes)?,
            (7, Field::Varint(value)) => info.block_height = value,
            (13, Field::Bytes(bytes)) => info.zcashd_build = text(bytes)?,
            (14, Field::Bytes(bytes)) => info.zcashd_subversion = text(bytes)?,
            _ => {}
        }
    }
    Ok(info)
}

/// Decodes a `BlockID`: `height` 1, `hash` 2.
pub fn decode_block_id(message: &[u8]) -> Result<BlockId, GrpcError> {
    let mut block = BlockId {
        height: 0,
        hash: Vec::new(),
    };
    for (number, field) in fields(message)? {
        match (number, field) {
            (1, Field::Varint(value)) => block.height = value,
            (2, Field::Bytes(bytes)) => block.hash = bytes.to_vec(),
            _ => {}
        }
    }
    Ok(block)
}

/// Encodes a `LightdInfo` the way a server does, for tests and the fake server.
pub fn encode_lightd_info(info: &LightdInfo) -> Vec<u8> {
    fn put_varint(out: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            out.push((value as u8) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }
    let mut out = Vec::new();
    let string = |out: &mut Vec<u8>, number: u64, value: &str| {
        if !value.is_empty() {
            put_varint(out, number << 3 | 2);
            put_varint(out, value.len() as u64);
            out.extend_from_slice(value.as_bytes());
        }
    };
    string(&mut out, 1, &info.version);
    string(&mut out, 2, &info.vendor);
    put_varint(&mut out, 3 << 3);
    put_varint(&mut out, 1);
    string(&mut out, 4, &info.chain_name);
    put_varint(&mut out, 5 << 3);
    put_varint(&mut out, info.sapling_activation_height);
    string(&mut out, 6, &info.consensus_branch_id);
    put_varint(&mut out, 7 << 3);
    put_varint(&mut out, info.block_height);
    string(&mut out, 13, &info.zcashd_build);
    string(&mut out, 14, &info.zcashd_subversion);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lightd_info_round_trips_and_skips_unknown_fields() {
        let info = LightdInfo {
            version: "7.0.0".into(),
            vendor: "ZcashFoundation/zebra".into(),
            chain_name: "main".into(),
            sapling_activation_height: 419_200,
            consensus_branch_id: "77190ad9".into(),
            block_height: 3_544_812,
            zcashd_build: "7.0.0".into(),
            zcashd_subversion: "/Zebra:7.0.0/".into(),
        };
        let mut bytes = encode_lightd_info(&info);
        // An unknown string field (16) and a fixed64 field (17) are skipped; their keys take
        // two bytes.
        bytes.extend_from_slice(&[0x82, 0x01, 3, b'N', b'U', b'7', 0x89, 0x01]);
        bytes.extend_from_slice(&[0; 8]);
        assert_eq!(decode_lightd_info(&bytes).unwrap(), info);
    }

    #[test]
    fn broken_messages_are_refused() {
        assert!(decode_lightd_info(&[1 << 3 | 2, 10, b'a']).is_err());
        assert!(decode_lightd_info(&[5 << 3, 0xff]).is_err());
        assert!(decode_lightd_info(&[1 << 3 | 2, 1, 0xff]).is_err());
        assert!(decode_lightd_info(&[0x0f]).is_err());
    }

    #[test]
    fn a_block_id_decodes() {
        let bytes = [1 << 3, 0xac, 0x02, 2 << 3 | 2, 2, 0xab, 0xcd];
        assert_eq!(
            decode_block_id(&bytes).unwrap(),
            BlockId {
                height: 300,
                hash: vec![0xab, 0xcd]
            }
        );
    }
}
