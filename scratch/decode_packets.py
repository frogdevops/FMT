import json
import struct
import sys
import os

class BSONDecoder:
    def __init__(self, data):
        self.data = data
        self.offset = 0

    def read_bytes(self, n):
        if self.offset + n > len(self.data):
            raise ValueError("Unexpected EOF")
        res = self.data[self.offset:self.offset+n]
        self.offset += n
        return res

    def read_byte(self):
        return self.read_bytes(1)[0]

    def read_int32(self):
        return struct.unpack("<i", self.read_bytes(4))[0]

    def read_int64(self):
        return struct.unpack("<q", self.read_bytes(8))[0]

    def read_double(self):
        return struct.unpack("<d", self.read_bytes(8))[0]

    def read_cstring(self):
        start = self.offset
        while self.data[self.offset] != 0:
            self.offset += 1
        res = self.data[start:self.offset].decode("utf-8", errors="ignore")
        self.offset += 1  # Skip null byte
        return res

    def read_string(self):
        length = self.read_int32()
        if length <= 0:
            return ""
        string_bytes = self.read_bytes(length - 1)
        null_byte = self.read_byte()
        if null_byte != 0:
            raise ValueError("String not null terminated")
        return string_bytes.decode("utf-8", errors="ignore")

    def read_document(self):
        doc_size = self.read_int32()
        res = {}
        while True:
            elem_type = self.read_byte()
            if elem_type == 0:
                break
            name = self.read_cstring()
            res[name] = self.read_value(elem_type)
        return res

    def read_value(self, elem_type):
        if elem_type == 0x01:  # Double
            return self.read_double()
        elif elem_type == 0x02:  # String
            return self.read_string()
        elif elem_type == 0x03:  # Document
            return self.read_document()
        elif elem_type == 0x04:  # Array
            doc = self.read_document()
            try:
                # BSON arrays are stored as docs with string keys representing indices
                return [doc[str(i)] for i in range(len(doc))]
            except KeyError:
                return doc
        elif elem_type == 0x05:  # Binary
            length = self.read_int32()
            subtype = self.read_byte()
            data = self.read_bytes(length)
            return data.hex()
        elif elem_type == 0x08:  # Boolean
            return self.read_byte() != 0
        elif elem_type == 0x0A:  # Null
            return None
        elif elem_type == 0x10:  # Int32
            return self.read_int32()
        elif elem_type == 0x12:  # Int64
            return self.read_int64()
        else:
            raise ValueError(f"Unsupported BSON type {elem_type:#x}")

def decode_payload(payload_bytes):
    # Pixel Worlds game packets have a custom 4-byte header followed by standard BSON.
    # The BSON document length prefix starts at offset 4.
    if len(payload_bytes) <= 8:
        return None
    
    try:
        # Verify if offset 4 looks like a valid BSON size (little-endian int <= remaining size)
        bson_len = struct.unpack("<i", payload_bytes[4:8])[0]
        if 5 <= bson_len <= len(payload_bytes) - 4:
            decoder = BSONDecoder(payload_bytes[4:])
            return decoder.read_document()
    except Exception:
        pass
    
    # Try parsing directly from index 0 just in case
    try:
        decoder = BSONDecoder(payload_bytes)
        return decoder.read_document()
    except Exception:
        pass

    return None

def process_log(log_path):
    if not os.path.exists(log_path):
        print(f"Log file not found: {log_path}")
        return

    print(f"Reading and translating raw packets from: {log_path}\n")
    
    parsed_count = 0
    for line in open(log_path):
        line = line.strip()
        if not line:
            continue
            
        try:
            frame = json.loads(line)
        except Exception:
            continue

        payload_hex = frame.get("payload_hex", "")
        if not payload_hex:
            continue

        try:
            payload_bytes = bytes.fromhex(payload_hex)
        except Exception:
            continue

        # Skip keepalives/short pings
        if len(payload_bytes) <= 4:
            continue

        decoded = decode_payload(payload_bytes)
        if decoded:
            timestamp = frame.get("timestamp_ms", 0)
            direction = frame.get("direction", "???")
            tag = "--> [OUTGOING]" if direction == "C2S" else "<-- [INCOMING]"
            print(f"[{timestamp}ms] {tag} | {len(payload_bytes)} bytes | BSON TRANSLATED:")
            print(json.dumps(decoded, indent=2, ensure_ascii=False))
            print("-" * 60)
            parsed_count += 1
            if parsed_count >= 10:
                print("Showing first 10 translated packets. Complete log contains more.")
                break

if __name__ == "__main__":
    path = sys.argv[1] if len(sys.argv) > 1 else '/home/chef/.local/share/Steam/steamapps/common/Pixel Worlds/packets.log'
    process_log(path)
