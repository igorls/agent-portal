//! Minimal schema-less protobuf reader for Antigravity's payloads.
//!
//! Antigravity has no published `.proto`, but its string fields are plaintext
//! UTF-8, so a wire-format walker recovers everything we need. Length-delimited
//! fields are kept as raw bytes and interpreted on access (`as_str`/`as_msg`),
//! which avoids guessing string-vs-nested-message at decode time.

/// A decoded wire field. This adapter only ever consumes strings and nested
/// messages, so varint and fixed fields are decoded solely to advance the
/// cursor — their numeric values are never retained.
#[derive(Debug, Clone)]
pub enum Val {
    Varint,
    Fixed,
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, Default)]
pub struct Msg {
    pub fields: Vec<(u32, Val)>,
}

fn read_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut result = 0u64;
    let mut shift = 0;
    while *i < b.len() {
        let byte = b[*i];
        *i += 1;
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

impl Msg {
    /// Best-effort decode: returns whatever parsed before any malformed byte.
    pub fn decode(b: &[u8]) -> Msg {
        let mut fields = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let Some(tag) = read_varint(b, &mut i) else { break };
            let field = (tag >> 3) as u32;
            let wire = tag & 7;
            if field == 0 {
                break;
            }
            let val = match wire {
                0 => match read_varint(b, &mut i) {
                    Some(_) => Val::Varint,
                    None => break,
                },
                1 => {
                    if i + 8 > b.len() {
                        break;
                    }
                    i += 8;
                    Val::Fixed
                }
                5 => {
                    if i + 4 > b.len() {
                        break;
                    }
                    i += 4;
                    Val::Fixed
                }
                2 => {
                    let Some(len) = read_varint(b, &mut i) else { break };
                    let len = len as usize;
                    if i + len > b.len() {
                        break;
                    }
                    let bytes = b[i..i + len].to_vec();
                    i += len;
                    Val::Bytes(bytes)
                }
                _ => break,
            };
            fields.push((field, val));
        }
        Msg { fields }
    }

    pub fn field(&self, n: u32) -> Option<&Val> {
        self.fields.iter().find(|(f, _)| *f == n).map(|(_, v)| v)
    }

    pub fn str(&self, n: u32) -> Option<String> {
        self.field(n).and_then(|v| v.as_str().map(str::to_string))
    }

    pub fn msg(&self, n: u32) -> Option<Msg> {
        self.field(n).map(|v| v.as_msg())
    }

    /// Every readable string anywhere in the tree (used for best-effort scans).
    pub fn strings(&self, out: &mut Vec<String>) {
        for (_, v) in &self.fields {
            if let Val::Bytes(b) = v {
                if let Ok(s) = std::str::from_utf8(b) {
                    let clean = s.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\t' | '\r'));
                    if clean && s.len() >= 2 {
                        out.push(s.to_string());
                        continue;
                    }
                }
                v.as_msg().strings(out);
            }
        }
    }
}

impl Val {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::Bytes(b) => std::str::from_utf8(b).ok().filter(|s| {
                s.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\t' | '\r'))
            }),
            _ => None,
        }
    }

    /// Interpret length-delimited bytes as a nested message (empty on garbage).
    pub fn as_msg(&self) -> Msg {
        match self {
            Val::Bytes(b) => Msg::decode(b),
            _ => Msg::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_nested_and_strings() {
        // field 1 = varint 132; field 5 = msg { field 2 = "hello" }
        // 0x08 0x84 0x01  (field1 varint 132)
        // 0x2a 0x07  (field5 len7) 0x12 0x05 h e l l o
        let bytes = [
            0x08, 0x84, 0x01, 0x2a, 0x07, 0x12, 0x05, b'h', b'e', b'l', b'l', b'o',
        ];
        let m = Msg::decode(&bytes);
        assert!(matches!(m.field(1), Some(Val::Varint)));
        let inner = m.msg(5).unwrap();
        assert_eq!(inner.str(2).as_deref(), Some("hello"));
    }
}
