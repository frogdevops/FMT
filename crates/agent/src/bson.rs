#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum BsonValue {
    Double(f64),
    String(String),
    Document(Vec<(String, BsonValue)>),  // ordered key-value pairs
    Array(Vec<BsonValue>),
    Binary(Vec<u8>),
    Boolean(bool),
    Null,
    Int32(i32),
    Int64(i64),
}

fn escape_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => out.push(c),
        }
    }
    out.push('"');
}

impl BsonValue {
    #[allow(dead_code)]
    pub fn to_json_string(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            BsonValue::Double(d) => {
                if d.is_finite() {
                    out.push_str(&d.to_string());
                } else {
                    out.push_str("null");
                }
            }
            BsonValue::String(s) => {
                escape_str(s, out);
            }
            BsonValue::Document(doc) => {
                out.push('{');
                for (i, (key, val)) in doc.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    escape_str(key, out);
                    out.push(':');
                    val.write_json(out);
                }
                out.push('}');
            }
            BsonValue::Array(arr) => {
                out.push('[');
                for (i, val) in arr.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    val.write_json(out);
                }
                out.push(']');
            }
            BsonValue::Binary(bin) => {
                out.push('"');
                for b in bin {
                    out.push_str(&format!("{:02x}", b));
                }
                out.push('"');
            }
            BsonValue::Boolean(b) => {
                if *b {
                    out.push_str("true");
                } else {
                    out.push_str("false");
                }
            }
            BsonValue::Null => {
                out.push_str("null");
            }
            BsonValue::Int32(i) => {
                out.push_str(&i.to_string());
            }
            BsonValue::Int64(i) => {
                out.push_str(&i.to_string());
            }
        }
    }
}

struct Parser<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    fn read_byte(&mut self) -> Option<u8> {
        let b = *self.data.get(self.offset)?;
        self.offset += 1;
        Some(b)
    }

    fn read_bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.offset + len > self.data.len() {
            return None;
        }
        let slice = &self.data[self.offset..self.offset + len];
        self.offset += len;
        Some(slice)
    }

    fn read_i32(&mut self) -> Option<i32> {
        let bytes = self.read_bytes(4)?;
        Some(i32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn read_i64(&mut self) -> Option<i64> {
        let bytes = self.read_bytes(8)?;
        Some(i64::from_le_bytes(bytes.try_into().ok()?))
    }

    fn read_f64(&mut self) -> Option<f64> {
        let bytes = self.read_bytes(8)?;
        Some(f64::from_le_bytes(bytes.try_into().ok()?))
    }

    fn read_cstring(&mut self) -> Option<String> {
        let start = self.offset;
        while *self.data.get(self.offset)? != 0 {
            self.offset += 1;
        }
        let end = self.offset;
        self.offset += 1; // consume null byte
        String::from_utf8(self.data[start..end].to_vec()).ok()
    }

    fn read_string(&mut self) -> Option<String> {
        let len = self.read_i32()?; // includes null terminator
        if len <= 0 {
            return None;
        }
        let string_len = (len - 1) as usize;
        let bytes = self.read_bytes(string_len)?;
        let terminator = self.read_byte()?;
        if terminator != 0 {
            return None;
        }
        String::from_utf8(bytes.to_vec()).ok()
    }

    fn read_document(&mut self) -> Option<Vec<(String, BsonValue)>> {
        let doc_start = self.offset;
        let doc_size = self.read_i32()?;
        if doc_size < 5 || doc_start + (doc_size as usize) > self.data.len() {
            return None;
        }
        
        let mut pairs = Vec::new();
        while self.offset < doc_start + (doc_size as usize) - 1 {
            let type_byte = self.read_byte()?;
            if type_byte == 0 {
                break;
            }
            let key = self.read_cstring()?;
            let val = self.read_value(type_byte)?;
            pairs.push((key, val));
        }
        
        let term = self.read_byte()?;
        if term != 0 {
            return None;
        }
        Some(pairs)
    }

    fn read_value(&mut self, type_byte: u8) -> Option<BsonValue> {
        match type_byte {
            0x01 => {
                let d = self.read_f64()?;
                Some(BsonValue::Double(d))
            }
            0x02 => {
                let s = self.read_string()?;
                Some(BsonValue::String(s))
            }
            0x03 => {
                let doc = self.read_document()?;
                Some(BsonValue::Document(doc))
            }
            0x04 => {
                let doc = self.read_document()?;
                let mut vals = Vec::new();
                for (_key, val) in doc {
                    vals.push(val);
                }
                Some(BsonValue::Array(vals))
            }
            0x05 => {
                let len = self.read_i32()?;
                if len < 0 {
                    return None;
                }
                let _subtype = self.read_byte()?;
                let bytes = self.read_bytes(len as usize)?;
                Some(BsonValue::Binary(bytes.to_vec()))
            }
            0x08 => {
                let b = self.read_byte()?;
                Some(BsonValue::Boolean(b != 0))
            }
            0x0A => Some(BsonValue::Null),
            0x10 => {
                let val = self.read_i32()?;
                Some(BsonValue::Int32(val))
            }
            0x12 => {
                let val = self.read_i64()?;
                Some(BsonValue::Int64(val))
            }
            _ => None,
        }
    }
}

#[allow(dead_code)]
pub fn parse_document(data: &[u8]) -> Option<Vec<(String, BsonValue)>> {
    let mut parser = Parser::new(data);
    parser.read_document()
}

#[allow(dead_code)]
pub fn try_parse_bson(data: &[u8]) -> Option<String> {
    // Try parsing starting at index 0
    if let Some(doc) = parse_document(data) {
        return Some(BsonValue::Document(doc).to_json_string());
    }

    // Try parsing starting at index 4 (assuming 4-byte header length prefix)
    if data.len() > 4 {
        if let Some(doc) = parse_document(&data[4..]) {
            return Some(BsonValue::Document(doc).to_json_string());
        }
    }

    // Try parsing starting at index 5 (assuming 4-byte length + 1-byte message type)
    if data.len() > 5 {
        if let Some(doc) = parse_document(&data[5..]) {
            return Some(BsonValue::Document(doc).to_json_string());
        }
    }

    None
}
