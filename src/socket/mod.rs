/// Unix socket server for shell communication.
///
/// The desktop shell connects via a Unix socket and receives real-time
/// notification events. The protocol uses length-prefixed protobuf
/// messages (4-byte big-endian length + protobuf body).

pub mod protocol;
pub mod server;

pub use server::SocketServer;
