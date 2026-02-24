use std::time::Instant;
use mio::net::TcpStream;

#[derive(Debug, PartialEq)]
pub enum ConnectionState {
    ReadRequest,
    CgiPending,
    WriteResponse,
    // Closing,
}

pub struct Connection {
    pub stream: TcpStream,

    pub state: ConnectionState,
    pub read_buffer: Vec<u8>,
    pub write_buffer: Vec<u8>,
    pub bytes_written: usize,
    pub last_activity: Instant,
    pub server_idx: usize,
    pub request_complete: bool,
}

impl Connection {
    pub fn new(stream: TcpStream, server_idx: usize) -> Self {
        Self {
            stream,
            state: ConnectionState::ReadRequest,
            read_buffer: Vec::with_capacity(8192),
            write_buffer: Vec::new(),
            bytes_written: 0,
            last_activity: Instant::now(),
            server_idx,
            request_complete: false,
        }
    }


}
