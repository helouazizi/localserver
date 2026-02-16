To code this effectively, you should follow a **bottom-up approach**. Start with the OS-level abstractions (epoll), then the data models, then the protocol, and finally the loop.

Here is the implementation for each file in your structure.

---

### Phase 1: The Network Layer
This handles the raw interaction with the Linux Kernel.

**File: `src/network/poll.rs`**
```rust
use libc::{epoll_create1, epoll_ctl, epoll_wait, epoll_event, EPOLL_CTL_ADD, EPOLL_CTL_DEL, EPOLL_CTL_MOD};
use std::io;
use std::os::unix::io::RawFd;

pub struct Poller {
    epoll_fd: RawFd,
}

impl Poller {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { epoll_create1(0) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        Ok(Self { epoll_fd: fd })
    }

    pub fn add(&self, fd: RawFd, events: u32) -> io::Result<()> {
        let mut event = epoll_event { events, u64: fd as u64 };
        let res = unsafe { epoll_ctl(self.epoll_fd, EPOLL_CTL_ADD, fd, &mut event) };
        if res < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    pub fn modify(&self, fd: RawFd, events: u32) -> io::Result<()> {
        let mut event = epoll_event { events, u64: fd as u64 };
        let res = unsafe { epoll_ctl(self.epoll_fd, EPOLL_CTL_MOD, fd, &mut event) };
        if res < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    pub fn delete(&self, fd: RawFd) -> io::Result<()> {
        let res = unsafe { epoll_ctl(self.epoll_fd, EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
        if res < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    pub fn wait(&self, events: &mut [epoll_event], timeout: i32) -> io::Result<usize> {
        let res = unsafe {
            epoll_wait(self.epoll_fd, events.as_mut_ptr(), events.len() as i32, timeout)
        };
        if res < 0 { Err(io::Error::last_os_error()) } else { Ok(res as usize) }
    }
}
```

---

### Phase 2: Configuration & State Models
Define how our server looks and how it remembers client progress.

**File: `src/config/models.rs`**
```rust
use std::collections::HashMap;

pub struct Route {
    pub path: String,
    pub root: String,
    pub methods: Vec<String>,
    pub index: Option<String>,
    pub redirect: Option<String>,
    pub cgi_ext: Option<String>,
}

pub struct ServerConfig {
    pub host: String,
    pub ports: Vec<u16>,
    pub server_names: Vec<String>,
    pub error_pages: HashMap<u16, String>,
    pub client_max_body_size: usize,
    pub routes: Vec<Route>,
}
```

**File: `src/server/connection.rs`**
```rust
use std::time::Instant;

pub enum ConnectionState {
    ReadRequest,
    WriteResponse,
    Closing,
}

pub struct Connection {
    pub fd: i32,
    pub last_activity: Instant,
    pub state: ConnectionState,
    pub read_buffer: Vec<u8>,
    pub write_buffer: Vec<u8>,
    pub bytes_written: usize,
}

impl Connection {
    pub fn new(fd: i32) -> Self {
        Self {
            fd,
            last_activity: Instant::now(),
            state: ConnectionState::ReadRequest,
            read_buffer: Vec::with_capacity(4096),
            write_buffer: Vec::new(),
            bytes_written: 0,
        }
    }
}
```

---

### Phase 3: The HTTP Protocol
A simple parser to turn bytes into a request.

**File: `src/http/request.rs`**
```rust
use std::collections::HashMap;

pub struct HttpRequest {
    pub method: String,
    pub uri: String,
    pub version: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    // Basic parser for demonstration; in reality, you'd handle partial reads
    pub fn parse(raw: &[u8]) -> Option<Self> {
        let mut headers = [0u8; 4096]; // Simplified
        let header_str = std::str::from_utf8(raw).ok()?;
        let mut lines = header_str.lines();
        
        let first_line = lines.next()?;
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 3 { return None; }

        Some(HttpRequest {
            method: parts[0].to_string(),
            uri: parts[1].to_string(),
            version: parts[2].to_string(),
            headers: HashMap::new(), // Add header parsing logic here
            body: Vec::new(),
        })
    }
}
```

---

### Phase 4: The Server Core (The Reactor)
This ties the network and the logic together.

**File: `src/server/mod.rs`**
```rust
pub mod connection;
use crate::network::poll::Poller;
use crate::server::connection::{Connection, ConnectionState};
use std::collections::HashMap;
use std::os::unix::io::RawFd;

pub struct Server {
    poller: Poller,
    listeners: Vec<RawFd>,
    connections: HashMap<RawFd, Connection>,
}

impl Server {
    pub fn new() -> Self {
        Self {
            poller: Poller::new().expect("Failed to create epoll"),
            listeners: Vec::new(),
            connections: HashMap::new(),
        }
    }

    pub fn add_listener(&mut self, fd: RawFd) {
        self.listeners.push(fd);
        self.poller.add(fd, libc::EPOLLIN as u32).expect("Epoll add failed");
    }

    pub fn run(&mut self) {
        let mut events = vec![libc::epoll_event { events: 0, u64: 0 }; 1024];
        loop {
            let n = self.poller.wait(&mut events, 1000).unwrap_or(0);
            for i in 0..n {
                let fd = events[i].u64 as i32;
                if self.listeners.contains(&fd) {
                    self.accept_new_client(fd);
                } else {
                    self.handle_client_event(fd, events[i].events);
                }
            }
        }
    }

    fn accept_new_client(&mut self, listen_fd: i32) {
        let client_fd = unsafe { libc::accept(listen_fd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if client_fd > 0 {
            // Set Non-blocking
            unsafe {
                let flags = libc::fcntl(client_fd, libc::F_GETFL, 0);
                libc::fcntl(client_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            self.poller.add(client_fd, libc::EPOLLIN as u32).unwrap();
            self.connections.insert(client_fd, Connection::new(client_fd));
        }
    }

    fn handle_client_event(&mut self, fd: i32, ev: u32) {
        // Logic for reading/writing using libc::read and libc::write
    }
}
```

---

### Phase 5: Main Entry Point
**File: `src/main.rs`**
```rust
mod network;
mod config;
mod server;
mod http;
mod handlers;

use crate::server::Server;
use std::net::TcpListener;
use std::os::unix::io::AsRawFd;

fn main() {
    println!("Starting Localhost Server...");

    let mut web_server = Server::new();

    // Setup a dummy listener for testing (Port 8080)
    // In final version, this will be driven by your YAML parser
    let listener = TcpListener::bind("127.0.0.1:8080").expect("Bind failed");
    listener.set_nonblocking(true).expect("Nonblocking failed");
    
    web_server.add_listener(listener.as_raw_fd());

    // Keep the listener alive by leaking it or storing it
    std::mem::forget(listener); 

    println!("Listening on http://127.0.0.1:8080");
    web_server.run();
}
```

---

### Next Steps to finish the project:
1.  **CGI Logic (`src/handlers/cgi.rs`)**: Use `libc::fork` and `libc::execve` to run scripts. Use `libc::pipe` to capture output.
2.  **YAML Parser**: Since you can't use crates, write a simple function in a new file `src/config/parser.rs` that reads `config.yaml` line by line and looks for key-value pairs.
3.  **Non-blocking logic**: In `handle_client_event`, you must use `libc::read` in a loop. If it returns `EAGAIN`, stop and wait for the next epoll event.
4.  **Error Pages**: Create a folder `www/errors/` and put your 404, 500 etc. pages there.

**Why this order?** 
Because `poll.rs` and `connection.rs` are the foundations. Without them, you cannot build the loop. Without the loop, you cannot test the HTTP parser.