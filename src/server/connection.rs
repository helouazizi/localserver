use std::time::Instant;

// src/server/connection.rs
pub enum ConnectionState {
    ReadRequest,
    WriteResponse,
    Closing,
    
}

pub struct Connection {
    pub fd: i32,
    pub state: ConnectionState,
    pub read_buffer: Vec<u8>,
    pub write_buffer: Vec<u8>,
    pub bytes_written: usize,
    pub last_activity: Instant,
    pub server_idx: usize,
    pub request_complete: bool,
    
}

impl Connection {
    pub fn new(fd: i32, server_idx: usize) -> Self {
        Self {
            fd,
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
