//! Protocol domain: universal raw network capture. WinSock/IOCP detours copy raw
//! bytes into a bounded ring (no firehose); a TCP server streams frames out.
//! Decoding (BSON, etc.) is the consumer's job, not the backend's.

pub mod capture;
pub mod hook;

pub use capture::{install_packet_hooks, remove_packet_hooks, start_tcp_server};
