// ─── common/system_field_tokens.rs ───────────────────────────────────────────
//
// System field tokenization for v1 storage format.
//
// System field keys are stored as single-byte negative FixInt integers in
// MsgPack instead of strings. This reduces per-document overhead to 1 byte
// per system key and enables O(1) integer comparison on the read path.
//
// Token dictionary (static, globally uniform):
//   _v          → -1
//   _key        → -2  (not stored in documents; injected at API boundary)
//   _seq        → -3
//   _createdAt  → -4
//   _modifiedAt → -5
//   _expiresAt  → -6
//
// MsgPack negative FixInt encoding: a single byte in range 0xe0..=0xff
//   -1  → 0xff
//   -2  → 0xfe
//   -3  → 0xfd
//   -4  → 0xfc
//   -5  → 0xfb
//   -6  → 0xfa
//
// ─────────────────────────────────────────────────────────────────────────────

use crate::common::system_fields::SystemFields;
use serde_json::{Map, Value};

// ─── Token byte constants ─────────────────────────────────────────────────────

const TOK_VERSION: u8 = 0xff; // -1
const TOK_KEY: u8 = 0xfe; // -2  (reserved, not stored in docs)
const TOK_SEQ: u8 = 0xfd; // -3
const TOK_CREATED_AT: u8 = 0xfc; // -4
const TOK_MODIFIED_AT: u8 = 0xfb; // -5
const TOK_EXPIRES_AT: u8 = 0xfa; // -6

// ─── Encode: Value → MsgPack bytes ───────────────────────────────────────────

/// Serialize a `serde_json::Value` to MsgPack bytes, encoding system field
/// string keys as single-byte negative FixInt tokens.
///
/// All other values are serialized identically to `rmp_serde::to_vec`.
pub fn value_to_msgpack(value: &Value) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let mut buf = Vec::new();
    write_value(&mut buf, value);
    Ok(buf)
}

fn write_value(buf: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => buf.push(0xc0),
        Value::Bool(true) => buf.push(0xc3),
        Value::Bool(false) => buf.push(0xc2),
        Value::Number(n) => write_number(buf, n),
        Value::String(s) => write_str(buf, s),
        Value::Array(arr) => {
            write_array_header(buf, arr.len());
            for item in arr {
                write_value(buf, item);
            }
        }
        Value::Object(map) => {
            write_map_header(buf, map.len());
            for (k, v) in map {
                write_map_key(buf, k);
                write_value(buf, v);
            }
        }
    }
}

fn write_map_key(buf: &mut Vec<u8>, key: &str) {
    match key {
        k if k == SystemFields::VERSION => buf.push(TOK_VERSION),
        k if k == SystemFields::SEQ => buf.push(TOK_SEQ),
        k if k == SystemFields::CREATED_AT => buf.push(TOK_CREATED_AT),
        k if k == SystemFields::MODIFIED_AT => buf.push(TOK_MODIFIED_AT),
        k if k == SystemFields::EXPIRES_AT => buf.push(TOK_EXPIRES_AT),
        _ => write_str(buf, key),
    }
}

fn write_number(buf: &mut Vec<u8>, n: &serde_json::Number) {
    if let Some(u) = n.as_u64() {
        if u <= 0x7f {
            buf.push(u as u8);
        } else if u <= 0xff {
            buf.push(0xcc);
            buf.push(u as u8);
        } else if u <= 0xffff {
            buf.push(0xcd);
            buf.extend_from_slice(&(u as u16).to_be_bytes());
        } else if u <= 0xffff_ffff {
            buf.push(0xce);
            buf.extend_from_slice(&(u as u32).to_be_bytes());
        } else {
            buf.push(0xcf);
            buf.extend_from_slice(&u.to_be_bytes());
        }
    } else if let Some(i) = n.as_i64() {
        if i >= -32 {
            buf.push(i as i8 as u8);
        } else if i >= i8::MIN as i64 {
            buf.push(0xd0);
            buf.push(i as i8 as u8);
        } else if i >= i16::MIN as i64 {
            buf.push(0xd1);
            buf.extend_from_slice(&(i as i16).to_be_bytes());
        } else if i >= i32::MIN as i64 {
            buf.push(0xd2);
            buf.extend_from_slice(&(i as i32).to_be_bytes());
        } else {
            buf.push(0xd3);
            buf.extend_from_slice(&i.to_be_bytes());
        }
    } else if let Some(f) = n.as_f64() {
        buf.push(0xcb);
        buf.extend_from_slice(&f.to_be_bytes());
    }
}

fn write_str(buf: &mut Vec<u8>, s: &str) {
    let len = s.len();
    if len <= 31 {
        buf.push(0xa0 | len as u8);
    } else if len <= 0xff {
        buf.push(0xd9);
        buf.push(len as u8);
    } else if len <= 0xffff {
        buf.push(0xda);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0xdb);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
    buf.extend_from_slice(s.as_bytes());
}

fn write_array_header(buf: &mut Vec<u8>, len: usize) {
    if len <= 15 {
        buf.push(0x90 | len as u8);
    } else if len <= 0xffff {
        buf.push(0xdc);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0xdd);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
}

fn write_map_header(buf: &mut Vec<u8>, len: usize) {
    if len <= 15 {
        buf.push(0x80 | len as u8);
    } else if len <= 0xffff {
        buf.push(0xde);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(0xdf);
        buf.extend_from_slice(&(len as u32).to_be_bytes());
    }
}

// ─── Decode: MsgPack bytes → Value ───────────────────────────────────────────

/// Deserialize MsgPack bytes to a `serde_json::Value`, expanding single-byte
/// negative FixInt map keys back to their public system field string names.
pub fn msgpack_to_value(bytes: &[u8]) -> Option<Value> {
    let mut pos = 0;
    read_value(bytes, &mut pos)
}

fn read_value(buf: &[u8], pos: &mut usize) -> Option<Value> {
    if *pos >= buf.len() {
        return None;
    }
    let byte = buf[*pos];
    *pos += 1;

    match byte {
        // nil
        0xc0 => Some(Value::Null),
        // false / true
        0xc2 => Some(Value::Bool(false)),
        0xc3 => Some(Value::Bool(true)),
        // positive FixInt (0x00..=0x7f)
        0x00..=0x7f => Some(Value::Number((byte as u64).into())),
        // negative FixInt (0xe0..=0xff) — these are i8 values -32..=-1
        0xe0..=0xff => Some(Value::Number((byte as i8 as i64).into())),
        // uint 8
        0xcc => {
            let v = *buf.get(*pos)?;
            *pos += 1;
            Some(Value::Number((v as u64).into()))
        }
        // uint 16
        0xcd => {
            if *pos + 2 > buf.len() {
                return None;
            }
            let v = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]);
            *pos += 2;
            Some(Value::Number((v as u64).into()))
        }
        // uint 32
        0xce => {
            if *pos + 4 > buf.len() {
                return None;
            }
            let v = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
            *pos += 4;
            Some(Value::Number((v as u64).into()))
        }
        // uint 64
        0xcf => {
            if *pos + 8 > buf.len() {
                return None;
            }
            let v = u64::from_be_bytes(buf[*pos..*pos + 8].try_into().ok()?);
            *pos += 8;
            Some(Value::Number(v.into()))
        }
        // int 8
        0xd0 => {
            let v = *buf.get(*pos)? as i8;
            *pos += 1;
            Some(Value::Number((v as i64).into()))
        }
        // int 16
        0xd1 => {
            if *pos + 2 > buf.len() {
                return None;
            }
            let v = i16::from_be_bytes([buf[*pos], buf[*pos + 1]]);
            *pos += 2;
            Some(Value::Number((v as i64).into()))
        }
        // int 32
        0xd2 => {
            if *pos + 4 > buf.len() {
                return None;
            }
            let v = i32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
            *pos += 4;
            Some(Value::Number((v as i64).into()))
        }
        // int 64
        0xd3 => {
            if *pos + 8 > buf.len() {
                return None;
            }
            let v = i64::from_be_bytes(buf[*pos..*pos + 8].try_into().ok()?);
            *pos += 8;
            Some(Value::Number(v.into()))
        }
        // float 32
        0xca => {
            if *pos + 4 > buf.len() {
                return None;
            }
            let v = f32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
            *pos += 4;
            serde_json::Number::from_f64(v as f64).map(Value::Number)
        }
        // float 64
        0xcb => {
            if *pos + 8 > buf.len() {
                return None;
            }
            let v = f64::from_be_bytes(buf[*pos..*pos + 8].try_into().ok()?);
            *pos += 8;
            serde_json::Number::from_f64(v).map(Value::Number)
        }
        // FixStr (0xa0..=0xbf)
        0xa0..=0xbf => {
            let len = (byte & 0x1f) as usize;
            read_str_value(buf, pos, len)
        }
        // str 8
        0xd9 => {
            let len = *buf.get(*pos)? as usize;
            *pos += 1;
            read_str_value(buf, pos, len)
        }
        // str 16
        0xda => {
            if *pos + 2 > buf.len() {
                return None;
            }
            let len = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]) as usize;
            *pos += 2;
            read_str_value(buf, pos, len)
        }
        // str 32
        0xdb => {
            if *pos + 4 > buf.len() {
                return None;
            }
            let len = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]])
                as usize;
            *pos += 4;
            read_str_value(buf, pos, len)
        }
        // FixArray (0x90..=0x9f)
        0x90..=0x9f => {
            let len = (byte & 0x0f) as usize;
            read_array(buf, pos, len)
        }
        // array 16
        0xdc => {
            if *pos + 2 > buf.len() {
                return None;
            }
            let len = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]) as usize;
            *pos += 2;
            read_array(buf, pos, len)
        }
        // array 32
        0xdd => {
            if *pos + 4 > buf.len() {
                return None;
            }
            let len = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]])
                as usize;
            *pos += 4;
            read_array(buf, pos, len)
        }
        // FixMap (0x80..=0x8f)
        0x80..=0x8f => {
            let len = (byte & 0x0f) as usize;
            read_map(buf, pos, len)
        }
        // map 16
        0xde => {
            if *pos + 2 > buf.len() {
                return None;
            }
            let len = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]) as usize;
            *pos += 2;
            read_map(buf, pos, len)
        }
        // map 32
        0xdf => {
            if *pos + 4 > buf.len() {
                return None;
            }
            let len = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]])
                as usize;
            *pos += 4;
            read_map(buf, pos, len)
        }
        // bin 8 / bin 16 / bin 32 — not used by this engine, skip
        0xc4 => {
            let len = *buf.get(*pos)? as usize;
            *pos += 1 + len;
            Some(Value::Null)
        }
        0xc5 => {
            if *pos + 2 > buf.len() {
                return None;
            }
            let len = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]) as usize;
            *pos += 2 + len;
            Some(Value::Null)
        }
        0xc6 => {
            if *pos + 4 > buf.len() {
                return None;
            }
            let len = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]])
                as usize;
            *pos += 4 + len;
            Some(Value::Null)
        }
        _ => None,
    }
}

fn read_str_value(buf: &[u8], pos: &mut usize, len: usize) -> Option<Value> {
    if *pos + len > buf.len() {
        return None;
    }
    let s = std::str::from_utf8(&buf[*pos..*pos + len])
        .ok()?
        .to_string();
    *pos += len;
    Some(Value::String(s))
}

fn read_array(buf: &[u8], pos: &mut usize, len: usize) -> Option<Value> {
    let mut arr = Vec::with_capacity(len);
    for _ in 0..len {
        arr.push(read_value(buf, pos)?);
    }
    Some(Value::Array(arr))
}

fn read_map(buf: &[u8], pos: &mut usize, len: usize) -> Option<Value> {
    let mut map = Map::with_capacity(len);
    for _ in 0..len {
        let key = read_map_key(buf, pos)?;
        let val = read_value(buf, pos)?;
        map.insert(key, val);
    }
    Some(Value::Object(map))
}

/// Read a map key. If the key byte is a negative FixInt token, expand it to
/// the corresponding public system field name. Otherwise read it as a string.
fn read_map_key(buf: &[u8], pos: &mut usize) -> Option<String> {
    if *pos >= buf.len() {
        return None;
    }
    let byte = buf[*pos];
    match byte {
        TOK_VERSION => {
            *pos += 1;
            Some(SystemFields::VERSION.to_string())
        }
        TOK_KEY => {
            *pos += 1;
            Some(SystemFields::KEY.to_string())
        }
        TOK_SEQ => {
            *pos += 1;
            Some(SystemFields::SEQ.to_string())
        }
        TOK_CREATED_AT => {
            *pos += 1;
            Some(SystemFields::CREATED_AT.to_string())
        }
        TOK_MODIFIED_AT => {
            *pos += 1;
            Some(SystemFields::MODIFIED_AT.to_string())
        }
        TOK_EXPIRES_AT => {
            *pos += 1;
            Some(SystemFields::EXPIRES_AT.to_string())
        }
        // Any other negative FixInt (0xe0..=0xf9) — unknown token, skip as string
        0xe0..=0xf9 => {
            *pos += 1;
            Some(format!("__tok_{}", byte as i8))
        }
        // Normal string key
        _ => {
            *pos += 1; // re-consume the byte we peeked
            // Reconstruct the full value read by prepending the byte back
            let mut tmp_pos = *pos - 1;
            match read_value(buf, &mut tmp_pos) {
                Some(Value::String(s)) => {
                    *pos = tmp_pos;
                    Some(s)
                }
                _ => None,
            }
        }
    }
}

// ─── Raw MsgPack seq reader ───────────────────────────────────────────────────

/// Extract the `_seq` token (-3 / 0xfd) value from raw MsgPack document bytes
/// without full deserialization. Returns `u64::MAX` if not found.
///
/// This is the O(N) fast-path used by the sort/pagination scanner.
pub fn read_msgpack_seq_token(bytes: &[u8]) -> u64 {
    read_token_u64(bytes, TOK_SEQ).unwrap_or(u64::MAX)
}

/// Scan a MsgPack map for a single-byte negative FixInt key and return its
/// value as u64. Returns None if the key is not found or the value is not an
/// integer.
fn read_token_u64(bytes: &[u8], token: u8) -> Option<u64> {
    let mut pos = 0usize;
    let byte = *bytes.get(pos)?;
    pos += 1;

    // Parse map header
    let map_len = match byte {
        0x80..=0x8f => (byte & 0x0f) as u32,
        0xde => {
            if pos + 2 > bytes.len() {
                return None;
            }
            let l = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as u32;
            pos += 2;
            l
        }
        0xdf => {
            if pos + 4 > bytes.len() {
                return None;
            }
            let l =
                u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
            pos += 4;
            l
        }
        _ => return None,
    };

    for _ in 0..map_len {
        if pos >= bytes.len() {
            return None;
        }
        let key_byte = bytes[pos];
        pos += 1;

        if key_byte == token {
            // Found — read the value as u64
            return read_uint_at(bytes, &mut pos);
        }

        // Skip this key's string bytes if it's a string key
        let key_skip = match key_byte {
            0xa0..=0xbf => (key_byte & 0x1f) as usize,
            0xd9 => {
                let l = *bytes.get(pos)? as usize;
                pos += 1;
                l
            }
            0xda => {
                if pos + 2 > bytes.len() {
                    return None;
                }
                let l = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
                pos += 2;
                l
            }
            0xdb => {
                if pos + 4 > bytes.len() {
                    return None;
                }
                let l = u32::from_be_bytes([
                    bytes[pos],
                    bytes[pos + 1],
                    bytes[pos + 2],
                    bytes[pos + 3],
                ]) as usize;
                pos += 4;
                l
            }
            // Other negative FixInt token key — no extra bytes
            0xe0..=0xff => 0,
            // Positive FixInt or other — no extra bytes for the key itself
            _ => 0,
        };
        pos += key_skip;

        // Skip the value
        let mut tmp = pos;
        skip_msgpack_value(bytes, &mut tmp)?;
        pos = tmp;
    }
    None
}

fn read_uint_at(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let byte = *bytes.get(*pos)?;
    *pos += 1;
    match byte {
        0x00..=0x7f => Some(byte as u64),
        0xcc => {
            let v = *bytes.get(*pos)?;
            *pos += 1;
            Some(v as u64)
        }
        0xcd => {
            if *pos + 2 > bytes.len() {
                return None;
            }
            let v = u16::from_be_bytes([bytes[*pos], bytes[*pos + 1]]);
            *pos += 2;
            Some(v as u64)
        }
        0xce => {
            if *pos + 4 > bytes.len() {
                return None;
            }
            let v = u32::from_be_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
            ]);
            *pos += 4;
            Some(v as u64)
        }
        0xcf => {
            if *pos + 8 > bytes.len() {
                return None;
            }
            let v = u64::from_be_bytes(bytes[*pos..*pos + 8].try_into().ok()?);
            *pos += 8;
            Some(v)
        }
        _ => None,
    }
}

fn skip_msgpack_value(bytes: &[u8], pos: &mut usize) -> Option<()> {
    use serde::de::Deserialize;
    let slice = &bytes[*pos..];
    let mut de = rmp_serde::Deserializer::new(slice);
    serde::de::IgnoredAny::deserialize(&mut de).ok()?;
    let remaining = de.into_inner().len();
    *pos = bytes.len() - remaining;
    Some(())
}

// ─── LogEntry serialization ───────────────────────────────────────────────────
//
// Serialize/deserialize a LogEntry with the `value` field tokenized.
// The other fields (cmd, collection, key, _t) are plain strings/u64.
//
// Wire format: MsgPack map with 5 entries:
//   "cmd"        → str
//   "collection" → str
//   "key"        → str
//   "value"      → tokenized MsgPack value (system field keys as negative FixInt)
//   "_t"         → u64

use crate::engine::LogEntry;

/// Serialize a `LogEntry` to MsgPack bytes, tokenizing system field keys in `value`.
pub fn log_entry_to_msgpack(entry: &LogEntry) -> Result<Vec<u8>, rmp_serde::encode::Error> {
    let mut buf = Vec::new();
    // 5-element map
    write_map_header(&mut buf, 5);
    write_str(&mut buf, "cmd");
    write_str(&mut buf, &entry.cmd);
    write_str(&mut buf, "collection");
    write_str(&mut buf, &entry.collection);
    write_str(&mut buf, "key");
    write_str(&mut buf, &entry.key);
    write_str(&mut buf, "value");
    write_value(&mut buf, &entry.value);
    write_str(&mut buf, "_t");
    write_number(&mut buf, &serde_json::Number::from(entry._t));
    Ok(buf)
}

/// Deserialize a `LogEntry` from MsgPack bytes, expanding tokenized system field keys in `value`.
pub fn log_entry_from_msgpack(bytes: &[u8]) -> Option<LogEntry> {
    let mut pos = 0usize;
    // Read map header
    let map_len = match read_map_header(bytes, &mut pos)? {
        n => n,
    };
    let mut cmd = String::new();
    let mut collection = String::new();
    let mut key = String::new();
    let mut value = serde_json::Value::Null;
    let mut _t: u64 = 0;

    for _ in 0..map_len {
        let field_name = read_string(bytes, &mut pos)?;
        match field_name.as_str() {
            "cmd" => cmd = read_string(bytes, &mut pos)?,
            "collection" => collection = read_string(bytes, &mut pos)?,
            "key" => key = read_string(bytes, &mut pos)?,
            "value" => value = read_value(bytes, &mut pos)?,
            "_t" => _t = read_uint_at(bytes, &mut pos)?,
            _ => {
                skip_msgpack_value(bytes, &mut pos)?;
            }
        }
    }

    Some(LogEntry {
        cmd,
        collection,
        key,
        value,
        _t,
    })
}

fn read_map_header(bytes: &[u8], pos: &mut usize) -> Option<usize> {
    if *pos >= bytes.len() {
        return None;
    }
    let byte = bytes[*pos];
    *pos += 1;
    match byte {
        0x80..=0x8f => Some((byte & 0x0f) as usize),
        0xde => {
            if *pos + 2 > bytes.len() {
                return None;
            }
            let l = u16::from_be_bytes([bytes[*pos], bytes[*pos + 1]]) as usize;
            *pos += 2;
            Some(l)
        }
        0xdf => {
            if *pos + 4 > bytes.len() {
                return None;
            }
            let l = u32::from_be_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
            ]) as usize;
            *pos += 4;
            Some(l)
        }
        _ => None,
    }
}

fn read_string(bytes: &[u8], pos: &mut usize) -> Option<String> {
    if *pos >= bytes.len() {
        return None;
    }
    let byte = bytes[*pos];
    *pos += 1;
    let len = match byte {
        0xa0..=0xbf => (byte & 0x1f) as usize,
        0xd9 => {
            let l = *bytes.get(*pos)? as usize;
            *pos += 1;
            l
        }
        0xda => {
            if *pos + 2 > bytes.len() {
                return None;
            }
            let l = u16::from_be_bytes([bytes[*pos], bytes[*pos + 1]]) as usize;
            *pos += 2;
            l
        }
        0xdb => {
            if *pos + 4 > bytes.len() {
                return None;
            }
            let l = u32::from_be_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
            ]) as usize;
            *pos += 4;
            l
        }
        _ => return None,
    };
    if *pos + len > bytes.len() {
        return None;
    }
    let s = std::str::from_utf8(&bytes[*pos..*pos + len])
        .ok()?
        .to_string();
    *pos += len;
    Some(s)
}
