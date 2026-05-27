import socket
import struct
import sys
import time

# New raw wire format (one frame, repeated):
#   [u8 direction][u64 socket_id LE][u32 len LE][len bytes payload]
# direction: 0 = C2S (outgoing), 1 = S2C (incoming)
HEADER = struct.Struct("<BQI")  # 1 + 8 + 4 = 13 bytes


def listen():
    server_address = ("127.0.0.1", 50051)
    print("Frog raw packet tap (binary framing).")
    c2s = s2c = 0
    while True:
        try:
            print(f"Connecting to {server_address[0]}:{server_address[1]} ...")
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.connect(server_address)
            print("Connected. Listening for live frames...\n")
            buf = bytearray()
            while True:
                data = sock.recv(65536)
                if not data:
                    print("Connection closed by agent. Reconnecting...")
                    break
                buf.extend(data)
                # Parse as many complete frames as the buffer holds.
                while len(buf) >= HEADER.size:
                    direction, socket_id, length = HEADER.unpack_from(buf, 0)
                    if len(buf) < HEADER.size + length:
                        break  # wait for the rest of the payload
                    payload = bytes(buf[HEADER.size:HEADER.size + length])
                    del buf[:HEADER.size + length]
                    if direction == 0:
                        c2s += 1
                        tag = "--> [C2S out]"
                    else:
                        s2c += 1
                        tag = "<-- [S2C  in]"
                    preview = payload[:32].hex()
                    print(f"{tag} sock={socket_id:#x} {length:>5}B  {preview}"
                          f"{'...' if length > 32 else ''}   (C2S={c2s} S2C={s2c})")
                    sys.stdout.flush()
        except ConnectionRefusedError:
            print("Refused — is the agent running with hooks installed? Retrying in 2s...")
            time.sleep(2)
        except KeyboardInterrupt:
            print(f"\nDone. Totals: C2S={c2s}  S2C={s2c}")
            sys.exit(0)
        finally:
            try:
                sock.close()
            except Exception:
                pass


if __name__ == "__main__":
    listen()
