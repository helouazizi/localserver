// src/server/mod.rs
pub mod connection;
use crate::config::models::Config;
use crate::handlers::cgi::execute_cgi;
use crate::http::request;
use crate::network::poll::Poller;
use crate::server::connection::{Connection, ConnectionState};
use libc::{EPOLLET, EPOLLIN, EPOLLOUT};
use std::collections::HashMap;

use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::fd::RawFd;

pub struct Server {
    poller: Poller,
    listeners: HashMap<RawFd, ListenerEntry>,
    connections: HashMap<RawFd, Connection>,
    config: Config,
}

struct ListenerEntry {
    listener: TcpListener,
    server_idx: usize,
}

impl Server {
    pub fn new(config: Config) -> Self {
        Self {
            poller: Poller::new().expect("Failed to create epoll"),
            listeners: HashMap::new(),
            connections: HashMap::new(),
            config,
        }
    }

    pub fn bind(&mut self) -> Result<(), String> {
        for (idx, s_cfg) in self.config.servers.iter().enumerate() {
            let addr = format!("{}:{}", s_cfg.host, s_cfg.port);

            match TcpListener::bind(&addr) {
                Ok(listener) => {
                    listener.set_nonblocking(true).map_err(|e| e.to_string())?;

                    let fd = listener.as_raw_fd();

                    println!("[Setup] Bound to http://{}", addr);

                    self.poller
                        .add(fd, libc::EPOLLIN as u32)
                        .map_err(|e| e.to_string())?;

                    self.listeners.insert(
                        fd,
                        ListenerEntry {
                            listener,
                            server_idx: idx,
                        },
                    );
                }
                Err(e) => {
                    eprintln!("[Setup] Failed to bind {}: {}", addr, e);
                }
            }
        }

        if self.listeners.is_empty() {
            return Err("No ports could be bound".into());
        }

        Ok(())
    }

    pub fn run(&mut self) {
        let mut events = vec![libc::epoll_event { events: 0, u64: 0 }; 1024];
        println!("\n[Reactor] Event loop started. Waiting for connections...");
        loop {
            // Wait for events (timeout 1 second to allow for cleanup logic)
            let n = match self.poller.wait(&mut events, 1000) {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("Epoll wait error: {}", e);
                    continue;
                }
            };

            for i in 0..n {
                let fd = events[i].u64 as i32;
                let ev = events[i].events;

                if self.listeners.contains_key(&fd) {
                    // This is a NEW connection on a listening port
                    self.accept_connection(fd);
                } else {
                    self.handle_client_event(fd, ev);
                }
            }
            self.check_timeouts();
        }
    }

    fn handle_client_event(&mut self, fd: i32, flags: u32) {
        // 1. Handle Errors
        if (flags & ((libc::EPOLLERR | libc::EPOLLHUP) as u32)) != 0 {
            println!("[Network] Closing connection on FD {} (Error/HUP)", fd);
            self.close_connection(fd);
            return;
        }

        // 2. Handle Reading (Client sent data)
        if (flags & (libc::EPOLLIN as u32)) != 0 {
            self.read_from_client(fd);
        }

        // 3. Handle Writing (Socket is ready for us to send data)
        if (flags & (libc::EPOLLOUT as u32)) != 0 {
            self.write_to_client(fd);
        }
    }

    fn read_from_client(&mut self, fd: i32) {
        let conn = match self.connections.get_mut(&fd) {
            Some(c) => c,
            None => {
                return;
            }
        };

        let mut temp_buf = [0u8; 4096];

        loop {
            let bytes_read = unsafe {
                libc::read(
                    fd,
                    temp_buf.as_mut_ptr() as *mut libc::c_void,
                    temp_buf.len(),
                )
            };

            if bytes_read > 0 {
                conn.read_buffer
                    .extend_from_slice(&temp_buf[..bytes_read as usize]);

                if let Some(header_end) = Self::find_header_end(&conn.read_buffer) {
                    let content_length = Self::get_content_length(&conn.read_buffer[..header_end]);
                    let body_len = conn.read_buffer.len().saturating_sub(header_end);

                    if content_length.map_or(true, |len| body_len >= len) {
                        conn.request_complete = true;
                        break;
                    }
                }
            } else if bytes_read == 0 {
                // Client closed connection
                self.close_connection(fd);
                return;
            } else {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    break; // No more data to read for now
                } else {
                    self.close_connection(fd);
                    return;
                }
            }
        }
        if conn.request_complete {
            self.process_request(fd);
        }
    }

    fn write_to_client(&mut self, fd: i32) {
        let conn = match self.connections.get_mut(&fd) {
            Some(c) => c,
            None => {
                return;
            }
        };

        while conn.bytes_written < conn.write_buffer.len() {
            let to_write = &conn.write_buffer[conn.bytes_written..];
            let n = unsafe {
                libc::write(fd, to_write.as_ptr() as *const libc::c_void, to_write.len())
            };

            if n > 0 {
                conn.bytes_written += n as usize;
            } else {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    return; // Socket buffer full, wait for next EPOLLOUT
                } else {
                    self.close_connection(fd);
                    return;
                }
            }
        }

        // Finished writing
        println!("[Network] Response sent to FD {}", fd);
        self.close_connection(fd);
    }

    fn process_request(&mut self, fd: i32) {
        // --- 1. DATA EXTRACTION ---
        // We get the server_idx directly from the connection state we saved during 'accept'
        let (method, uri, headers, body, server_idx) = {
            let conn = match self.connections.get(&fd) {
                Some(c) => c,
                None => {
                    return;
                }
            };

            let idx = conn.server_idx; // Use the saved index!

            match crate::http::request::HttpRequest::parse(&conn.read_buffer) {
                Some(req) => (req.method, req.uri, req.headers, req.body, idx),
                None => (
                    "".to_string(),
                    "".to_string(),
                    std::collections::HashMap::new(),
                    Vec::new(),
                    999,
                ),
            }
        };

        if server_idx == 999 {
            self.send_error(fd, 400, "Bad Request");
            return;
        }

        println!("[HTTP] {} requested: {}", method, uri);

        let (path_only, query_string) = match uri.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (uri.clone(), String::new()),
        };

        // --- 2. CONFIG LOOKUP ---
        let server_cfg = &self.config.servers[server_idx];

        let mut matched_route = None;
        for route in &server_cfg.routes {
            if path_only.starts_with(&route.path) {
                // Longest prefix match logic
                if matched_route.map_or(true, |r: &crate::config::models::RouteConfig| {
                    route.path.len() > r.path.len()
                }) {
                    matched_route = Some(route);
                }
            }
        }

        let route = match matched_route {
            Some(r) => r,
            None => {
                self.send_error(fd, 404, "Not Found");
                return;
            }
        };

        // --- 3. METHOD VALIDATION ---
        if !route.methods.contains(&method) && !route.methods.is_empty() {
            self.send_error(fd, 405, "Method Not Allowed");
            return;
        }

        // --- 4. FILE SYSTEM LOGIC ---
        let relative_path = path_only.strip_prefix(&route.path).unwrap_or("");
        let mut full_path = std::path::PathBuf::from(&route.root);
        full_path.push(relative_path.trim_start_matches('/'));

        println!("------------> {:?}", full_path);
        if method == "POST" {
            if let Some(form) = crate::http::request::HttpRequest::parse_multipart(&headers, &body) {
              self.handle_multipart_upload(form, &full_path);
            }
        }
        if full_path.is_dir() {
            if let Some(index_file) = &route.index {
                full_path.push(index_file);
            } else if route.autoindex {
                self.send_error(fd, 501, "Not Implemented (Autoindex)");
                return;
            } else {
                self.send_error(fd, 400, "Bad Request");
                return;
            }
        }

        // --- 5. RESPONSE GENERATION ---
        if let Some(cgi_ext) = &route.cgi_extension {
            if path_only.ends_with(cgi_ext) {
                if !full_path.exists() {
                    self.send_error(fd, 404, "Not Found");
                    return;
                }

                let script_path = std::fs::canonicalize(&full_path).unwrap_or(full_path.clone());
                let script_path_str = script_path.to_string_lossy().to_string();

                let mut env_vars = std::collections::HashMap::new();
                env_vars.insert("REQUEST_METHOD".to_string(), method.clone());
                env_vars.insert("SCRIPT_FILENAME".to_string(), script_path_str.clone());
                env_vars.insert("PATH_INFO".to_string(), script_path_str.clone());
                env_vars.insert("QUERY_STRING".to_string(), query_string);
                env_vars.insert("SERVER_PROTOCOL".to_string(), "HTTP/1.1".to_string());
                env_vars.insert("GATEWAY_INTERFACE".to_string(), "CGI/1.1".to_string());

                if let Some(content_type) = headers.get("content-type") {
                    env_vars.insert("CONTENT_TYPE".to_string(), content_type.clone());
                }
                if let Some(content_length) = headers.get("content-length") {
                    env_vars.insert("CONTENT_LENGTH".to_string(), content_length.clone());
                } else {
                    env_vars.insert("CONTENT_LENGTH".to_string(), "0".to_string());
                }

                let interpreter = route.cgi_interpreter.as_deref();
                let output = match execute_cgi(&script_path_str, interpreter, &body, env_vars) {
                    Ok(out) => out,
                    Err(_) => {
                        self.send_error(fd, 500, "CGI Execution Failed");
                        return;
                    }
                };

                let response_bytes = Self::build_cgi_response(&output);

                if let Some(conn) = self.connections.get_mut(&fd) {
                    conn.write_buffer = response_bytes;
                    conn.state = ConnectionState::WriteResponse;
                    let _ = self.poller.modify(fd, libc::EPOLLOUT as u32);
                }
                return;
            }
        }

        match std::fs::read(&full_path) {
            Ok(content) => {
                let header = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: {}\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    Self::get_mime_type(full_path.to_str().unwrap_or("")),
                    content.len()
                );

                if let Some(conn) = self.connections.get_mut(&fd) {
                    conn.write_buffer = [header.as_bytes(), &content].concat();
                    conn.state = ConnectionState::WriteResponse;
                    let _ = self.poller.modify(fd, libc::EPOLLOUT as u32);
                }
            }
            Err(_) => {
                // This triggers if the file is missing or permissions are wrong
                self.send_error(fd, 400, "Bad Request");
            }
        }
    }

    fn send_error(&mut self, fd: i32, code: u16, msg: &str) {
        // Get the server_idx from the connection
        let server_idx = self.connections.get(&fd).map(|c| c.server_idx).unwrap_or(0);
        let server_cfg = &self.config.servers[server_idx];

        let mut body = format!("<p>Localhost Server</p>\r\n<h1>{} {}</h1>", code, msg);
        // Default hardcoded body

        // Try to find the custom error page from the YAML
        if let Some(custom_path) = server_cfg.error_pages.get(&code) {
            // IMPORTANT: The path in YAML must be relative to where you run 'cargo run'
            // For example: ./www/errors/404.html
            match std::fs::read_to_string(custom_path.trim_start_matches('/')) {
                Ok(content) => {
                    body = content;
                }
                Err(e) => {
                    eprintln!(
                        "[Config] Error page {} found in YAML but could not be read: {}",
                        custom_path, e
                    );
                }
            }
        }

        let response = format!(
            "HTTP/1.1 {} {}\r\n\
             Content-Type: text/html\r\n\
             Content-Length: {}\r\n\
             Server: Localhost_RS\r\n\
             Connection: close\r\n\r\n{}",
            code,
            msg,
            body.len(),
            body
        );

        if let Some(conn) = self.connections.get_mut(&fd) {
            conn.write_buffer = response.into_bytes();
            conn.state = ConnectionState::WriteResponse;
            // Update last activity so we don't timeout while sending error
            conn.last_activity = std::time::Instant::now();
            let _ = self.poller.modify(fd, libc::EPOLLOUT as u32);
        }
    }

    fn check_timeouts(&mut self) {
        let now = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(self.config.timeout_seconds);

        let to_remove: Vec<i32> = self
            .connections
            .iter()
            .filter(|(_, conn)| now.duration_since(conn.last_activity) > timeout)
            .map(|(&fd, _)| fd)
            .collect();

        for fd in to_remove {
            self.close_connection(fd);
        }
    }

    fn accept_connection(&mut self, listener_fd: RawFd) {
        let server_idx = match self.listeners.get(&listener_fd) {
            Some(entry) => entry.server_idx,
            None => {
                return;
            }
        };

        let client_fd =
            unsafe { libc::accept(listener_fd, std::ptr::null_mut(), std::ptr::null_mut()) };

        if client_fd >= 0 {
            unsafe {
                let flags = libc::fcntl(client_fd, libc::F_GETFL, 0);
                libc::fcntl(client_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }

            self.poller.add(client_fd, libc::EPOLLIN as u32).ok();

            self.connections
                .insert(client_fd, Connection::new(client_fd, server_idx));

            println!("[Network] New client connected on FD {}", client_fd);
        }
    }

    fn close_connection(&mut self, fd: i32) {
        let _ = self.poller.delete(fd);
        unsafe {
            libc::close(fd);
        }
        self.connections.remove(&fd);
    }

    fn get_mime_type(path: &str) -> &str {
        if path.ends_with(".html") {
            "text/html"
        } else if path.ends_with(".css") {
            "text/css"
        } else if path.ends_with(".js") {
            "application/javascript"
        } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
            "image/jpeg"
        } else if path.ends_with(".png") {
            "image/png"
        } else {
            "text/plain"
        }
    }

    fn get_content_length(header_bytes: &[u8]) -> Option<usize> {
        let header_str = std::str::from_utf8(header_bytes).ok()?;
        for line in header_str.lines() {
            if let Some((key, value)) = line.split_once(':') {
                if key.trim().eq_ignore_ascii_case("content-length") {
                    return value.trim().parse::<usize>().ok();
                }
            }
        }
        None
    }

    fn build_cgi_response(output: &[u8]) -> Vec<u8> {
        if output.starts_with(b"HTTP/") {
            return output.to_vec();
        }

        let (header_part, body_part) =
            if let Some(pos) = output.windows(4).position(|w| w == b"\r\n\r\n") {
                (&output[..pos], &output[pos + 4..])
            } else if let Some(pos) = output.windows(2).position(|w| w == b"\n\n") {
                (&output[..pos], &output[pos + 2..])
            } else {
                (&[][..], output)
            };

        let mut status_code = 200u16;
        let mut status_text = "OK".to_string();
        let mut headers: Vec<(String, String)> = Vec::new();

        if !header_part.is_empty() {
            if let Ok(header_str) = std::str::from_utf8(header_part) {
                for line in header_str.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.trim().eq_ignore_ascii_case("status") {
                            let status_val = value.trim();
                            let mut parts = status_val.splitn(2, ' ');
                            if let Some(code_str) = parts.next() {
                                if let Ok(code) = code_str.parse::<u16>() {
                                    status_code = code;
                                }
                            }
                            if let Some(text) = parts.next() {
                                status_text = text.trim().to_string();
                            }
                        } else {
                            headers.push((key.trim().to_string(), value.trim().to_string()));
                        }
                    }
                }
            }
        }

        let mut has_content_type = false;
        let mut has_content_length = false;
        let mut header_lines = String::new();

        for (k, v) in &headers {
            if k.eq_ignore_ascii_case("content-type") {
                has_content_type = true;
            }
            if k.eq_ignore_ascii_case("content-length") {
                has_content_length = true;
            }
            header_lines.push_str(&format!("{}: {}\r\n", k, v));
        }

        if !has_content_type {
            header_lines.push_str("Content-Type: text/plain\r\n");
        }
        if !has_content_length {
            header_lines.push_str(&format!("Content-Length: {}\r\n", body_part.len()));
        }

        let header = format!(
            "HTTP/1.1 {} {}\r\n{}Connection: close\r\n\r\n",
            status_code, status_text, header_lines
        );

        [header.as_bytes(), body_part].concat()
    }
    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|pos| pos + 4)
    }
    fn handle_multipart_upload(
    &self,
    form: crate::http::request::MultipartForm,
    upload_dir: &std::path::Path,
) -> Result<(), String> {
    // 1️⃣ Ensure upload directory exists
    if !upload_dir.exists() {
        std::fs::create_dir_all(upload_dir)
            .map_err(|e| format!("Failed to create upload directory: {}", e))?;
    }

    // 2️⃣ Save each file
    for file in form.files {
        // Prevent directory traversal attacks
        let safe_filename = std::path::Path::new(&file.file_name)
            .file_name()
            .ok_or("Invalid file name")?;

        let mut full_path = upload_dir.to_path_buf();
        full_path.push(safe_filename);

        std::fs::write(&full_path, &file.data)
            .map_err(|e| format!("Failed to write file {:?}: {}", full_path, e))?;

        println!(
            "[UPLOAD] Saved file: {:?} ({} bytes)",
            full_path,
            file.data.len()
        );
    }

    Ok(())
}

}
