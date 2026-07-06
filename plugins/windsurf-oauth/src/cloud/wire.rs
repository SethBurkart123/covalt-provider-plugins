use std::io::{Read, Write};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

pub fn encode_varint(value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value;
    loop {
        if v <= 0x7f {
            out.push(v as u8);
            break;
        }
        out.push(((v & 0x7f) as u8) | 0x80);
        v >>= 7;
    }
    out
}

pub fn encode_tag(field_num: u32, wire: u32) -> Vec<u8> {
    encode_varint(((field_num as u64) << 3) | wire as u64)
}

pub fn encode_string(field_num: u32, value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = encode_tag(field_num, 2);
    out.extend(encode_varint(bytes.len() as u64));
    out.extend_from_slice(bytes);
    out
}

pub fn encode_message(field_num: u32, body: &[u8]) -> Vec<u8> {
    let mut out = encode_tag(field_num, 2);
    out.extend(encode_varint(body.len() as u64));
    out.extend_from_slice(body);
    out
}

pub fn encode_varint_field(field_num: u32, value: u64) -> Vec<u8> {
    let mut out = encode_tag(field_num, 0);
    out.extend(encode_varint(value));
    out
}

pub fn encode_fixed64_field(field_num: u32, value: f64) -> Vec<u8> {
    let mut out = encode_tag(field_num, 1);
    out.extend(value.to_le_bytes());
    out
}

pub fn encode_timestamp_body() -> Vec<u8> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs();
    let nanos = now.subsec_nanos();
    let mut out = encode_varint_field(1, seconds);
    if nanos > 0 {
        out.extend(encode_varint_field(2, nanos as u64));
    }
    out
}

pub fn decode_varint(buf: &[u8], offset: usize) -> Option<(u64, usize)> {
    let mut res = 0u64;
    let mut shift = 0u32;
    let mut i = offset;
    while i < buf.len() {
        let b = buf[i];
        i += 1;
        res |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some((res, i));
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct ProtoField {
    pub num: u32,
    pub wire: u32,
    pub value: FieldValue,
}

#[derive(Debug, Clone)]
pub enum FieldValue {
    Varint(u64),
    Fixed64([u8; 8]),
    Fixed32([u8; 4]),
    Bytes(Vec<u8>),
}

pub fn iter_fields(buf: &[u8]) -> impl Iterator<Item = ProtoField> + '_ {
    let mut index = 0usize;
    std::iter::from_fn(move || {
        let (tag, next) = decode_varint(buf, index)?;
        index = next;
        let num = (tag >> 3) as u32;
        let wire = (tag & 0x7) as u32;
        match wire {
            0 => {
                let (value, next) = decode_varint(buf, index)?;
                index = next;
                Some(ProtoField {
                    num,
                    wire,
                    value: FieldValue::Varint(value),
                })
            }
            1 => {
                if index + 8 > buf.len() {
                    return None;
                }
                let mut fixed = [0u8; 8];
                fixed.copy_from_slice(&buf[index..index + 8]);
                index += 8;
                Some(ProtoField {
                    num,
                    wire,
                    value: FieldValue::Fixed64(fixed),
                })
            }
            2 => {
                let (len, next) = decode_varint(buf, index)?;
                index = next;
                let len = len as usize;
                if index + len > buf.len() {
                    return None;
                }
                let bytes = buf[index..index + len].to_vec();
                index += len;
                Some(ProtoField {
                    num,
                    wire,
                    value: FieldValue::Bytes(bytes),
                })
            }
            5 => {
                if index + 4 > buf.len() {
                    return None;
                }
                let mut fixed = [0u8; 4];
                fixed.copy_from_slice(&buf[index..index + 4]);
                index += 4;
                Some(ProtoField {
                    num,
                    wire,
                    value: FieldValue::Fixed32(fixed),
                })
            }
            _ => None,
        }
    })
}

pub fn frame_connect_stream(body: &[u8], compress: bool) -> Vec<u8> {
    let (payload, flags) = if compress {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body).expect("gzip encode");
        (encoder.finish().expect("gzip finish"), 0x01u8)
    } else {
        (body.to_vec(), 0u8)
    };
    let mut out = vec![flags];
    out.extend((payload.len() as u32).to_be_bytes());
    out.extend(payload);
    out
}

#[derive(Debug, Clone)]
pub struct ConnectFrame {
    pub flags: u8,
    pub payload: Vec<u8>,
    pub eos: bool,
}

pub fn parse_connect_frames(buf: &[u8]) -> Vec<ConnectFrame> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 5 <= buf.len() {
        let flags = buf[i];
        let len = u32::from_be_bytes([buf[i + 1], buf[i + 2], buf[i + 3], buf[i + 4]]) as usize;
        if i + 5 + len > buf.len() {
            break;
        }
        let mut payload = buf[i + 5..i + 5 + len].to_vec();
        if flags & 0x01 != 0 {
            let mut decoder = GzDecoder::new(payload.as_slice());
            let mut decoded = Vec::new();
            if decoder.read_to_end(&mut decoded).is_ok() {
                payload = decoded;
            }
        }
        out.push(ConnectFrame {
            flags,
            payload,
            eos: flags & 0x02 != 0,
        });
        i += 5 + len;
    }
    out
}
