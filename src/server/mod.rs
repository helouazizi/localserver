pub mod connection;
use crate::config::models::Config;
use crate::handlers::cgi::spawn_cgi_process;
use crate::server::connection::{ Connection, ConnectionState };

use mio::net::{ TcpListener };
use mio::unix::SourceFd;
use mio::{ Interest, Poll, Token };
use std::collections::HashMap;
use std::io::{ self, Read, Write };
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

const SERVER_TOKEN_MAX: usize = 100; // Assume max 100 server blocks

pub struct Server {
    poll: Poll,
    listeners: HashMap<Token, ListenerEntry>,
    connections: HashMap<Token, Connection>,
    pending_cgi: HashMap<Token, PendingCgi>,
    cgi_token_to_client: HashMap<Token, Token>,
    config: Config,
    next_token: usize,
}

struct ListenerEntry {
    listener: TcpListener,
    server_idx: usize,
}

struct PendingCgi {
    child: std::process::Child,
    stdout: std::process::ChildStdout,
    output: Vec<u8>,
    io_token: Token,
    started_at: Instant,
}

impl Server {
    pub fn new(config: Config) -> Self {
        Self {
            poll: Poll::new().expect("Failed to create mio poll"),
            listeners: HashMap::new(),
            connections: HashMap::new(),
            pending_cgi: HashMap::new(),
            cgi_token_to_client: HashMap::new(),
            config,
            next_token: SERVER_TOKEN_MAX,
        }
    }

    pub fn bind(&mut self) -> Result<(), String> {
        for (idx, s_cfg) in self.config.servers.iter().enumerate() {
            let addr = format!("{}:{}", s_cfg.host, s_cfg.port)
                .parse()
                .map_err(|e| format!("Invalid address: {}", e))?;

            match TcpListener::bind(addr) {
                Ok(mut listener) => {
                    let token = Token(idx);

                    self.poll
                        .registry()
                        .register(&mut listener, token, Interest::READABLE)
                        .map_err(|e| e.to_string())?;

                    self.listeners.insert(token, ListenerEntry {
                        listener,
                        server_idx: idx,
                    });
                    println!("[Setup] Bound to http://{}", addr);
                }
                Err(e) => eprintln!("[Setup] Failed to bind {}: {}", addr, e),
            }
        }

        if self.listeners.is_empty() {
            return Err("No ports could be bound".into());
        }
        Ok(())
    }

    pub fn run(&mut self) {
        let mut events = mio::Events::with_capacity(1024);

        println!("\n[Reactor] Mio event loop started...");
        loop {
            if
                let Err(e) = self.poll.poll(
                    &mut events,
                    Some(std::time::Duration::from_millis(1000))
                )
            {
                eprintln!("Mio poll error: {}", e);
                continue;
            }

            for event in events.iter() {
                let token = event.token();

                if self.listeners.contains_key(&token) {
                    self.accept_connection(token);
                } else if self.cgi_token_to_client.contains_key(&token) {
                    self.handle_cgi_event(token, event);
                } else {
                    self.handle_client_event(token, event);
                }
            }
            self.check_cgi_progress();
            self.check_cgi_timeouts();
            self.check_timeouts();
        }
    }

    fn handle_client_event(&mut self, token: Token, event: &mio::event::Event) {
        if let Some(conn) = self.connections.get(&token) {
            if conn.state == ConnectionState::CgiPending {
                if event.is_read_closed() || event.is_write_closed() {
                    self.close_connection(token);
                }
                return;
            }
        }

        // Handle Reading
        if event.is_readable() {
            self.read_from_client(token);
        }

        // Handle Writing
        if event.is_writable() {
            self.write_to_client(token);
        }

        // Handle Errors/Hangup
        if event.is_read_closed() || event.is_write_closed() {
            self.close_connection(token);
        }
    }

    fn read_from_client(&mut self, token: Token) {
        let server_idx = match self.connections.get(&token) {
            Some(c) => c.server_idx,
            None => {
                return;
            }
        };

        let effective_body_limit = self.config.servers
            .get(server_idx)
            .map(|s| s.max_body_size.min(self.config.max_server_size))
            .unwrap_or(self.config.max_server_size);

        let conn = match self.connections.get_mut(&token) {
            Some(c) => c,
            None => {
                return;
            }
        };

        let mut buf = [0u8; 4096];
        let mut oversized = false;
        let mut should_process = false;

        loop {
            match conn.stream.read(&mut buf) {
                Ok(0) => {
                    self.close_connection(token);
                    return;
                }
                Ok(n) => {
                    conn.read_buffer.extend_from_slice(&buf[..n]);
                    conn.last_activity = std::time::Instant::now();

                    if conn.read_buffer.len() > self.config.max_server_size {
                        oversized = true;
                        break;
                    }

                    if let Some(header_end) = Self::find_header_end(&conn.read_buffer) {
                        if
                            let Some(content_length) = Self::extract_content_length(
                                &conn.read_buffer[..header_end]
                            )
                        {
                            if content_length > effective_body_limit {
                                oversized = true;
                                break;
                            }
                        }

                        let current_body_len = conn.read_buffer.len().saturating_sub(header_end);
                        if current_body_len > effective_body_limit {
                            oversized = true;
                            break;
                        }
                    }

                    // Use our refactored helper to check if the full request (Header + Body) is here
                    if crate::http::request::HttpRequest::is_complete(&conn.read_buffer) {
                        conn.request_complete = true;
                        should_process = true;
                        break; // Exit the read loop to start processing
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // OS buffer is empty for now, wait for the next EPOLLIN notification
                    break;
                }
                Err(_) => {
                    // Real socket error
                    self.close_connection(token);
                    return;
                }
            }
        }

        if oversized {
            self.send_error(token, 413);
            return;
        }

        if should_process {
            self.process_request(token);
        }
    }

    fn write_to_client(&mut self, token: Token) {
        let conn = match self.connections.get_mut(&token) {
            Some(c) => c,
            None => {
                return;
            }
        };

        while conn.bytes_written < conn.write_buffer.len() {
            let to_write = &conn.write_buffer[conn.bytes_written..];

            match conn.stream.write(to_write) {
                Ok(n) => {
                    conn.bytes_written += n;
                    conn.last_activity = std::time::Instant::now();
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return;
                }
                Err(_) => {
                    self.close_connection(token);
                    return;
                }
            }
        }

        println!("[Network] Response sent to Token {:?}", token);

        self.close_connection(token);
    }

    fn process_request(&mut self, token: Token) {
        // --- 1. DATA EXTRACTION ---
        let (method, uri, headers, body, server_idx) = {
            let conn = match self.connections.get(&token) {
                Some(c) => c,
                None => {
                    return;
                }
            };
            let idx = conn.server_idx;
            match crate::http::request::HttpRequest::parse(&conn.read_buffer) {
                Some(req) => (req.method, req.uri, req.headers, req.body, idx),
                None =>
                    (
                        "".to_string(),
                        "".to_string(),
                        std::collections::HashMap::new(),
                        Vec::new(),
                        999,
                    ),
            }
        };

        if server_idx == 999 {
            self.send_error(token, 400);
            return;
        }

        let (path_only, query_string) = match uri.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (uri.clone(), String::new()),
        };

        if method == "GET" {
            if self.try_serve_upload_file(token, server_idx, &path_only) {
                return;
            }
        }

        // --- 2. CONFIG & ROUTE LOOKUP ---
        let server_cfg = &self.config.servers[server_idx];
        let route = match
            server_cfg.routes
                .iter()
                .filter(|r| Self::path_matches_route(&path_only, &r.path))
                .max_by_key(|r| r.path.len())
        {
            Some(r) => r.clone(),
            None => {
                self.send_error(token, 404);
                return;
            }
        };

        // --- 3. METHOD VALIDATION ---
        if !route.methods.contains(&method) && !route.methods.is_empty() {
            self.send_error(token, 405);
            return;
        }

        // --- 4. CONVENTION-BASED UPLOAD LOGIC ---
        // Rule: If it's POST/PUT and NOT a CGI script, treat it as an upload
        let is_cgi = route.cgi_extension.as_ref().map_or(false, |ext| path_only.ends_with(ext));

        if (method == "POST" || method == "PUT") && !is_cgi {
            let upload_path = route.upload_dir
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    let mut fallback = std::path::PathBuf::from(&route.root);
                    fallback.push("uploads");
                    fallback
                });

            let mut upload_performed = false;

            if let Some(form) = crate::http::request::HttpRequest::parse_multipart(&headers, &body) {
                if self.handle_multipart_upload(form, &upload_path).is_ok() {
                    upload_performed = true;
                }
            } else if !body.is_empty() {
                let filename = Self::extract_raw_upload_filename(&path_only, &route.path, &headers);
                if self.handle_raw_upload(&body, &upload_path, &filename).is_ok() {
                    upload_performed = true;
                }
            }

            if upload_performed {
                self.send_text_response(token, 201, "Upload Successful", "text/plain");
                return;
            }
        }

        // --- 5. PATH RESOLUTION ---
        let relative_path = path_only.strip_prefix(&route.path).unwrap_or("");
        let mut full_path = std::path::PathBuf::from(&route.root);
        full_path.push(relative_path.trim_start_matches('/'));

        // --- 6. DIRECTORY HANDLING ---
        if full_path.is_dir() {
            if let Some(index_file) = &route.index {
                full_path.push(index_file);
            } else if route.autoindex {
                self.send_error(token, 501);
                return;
            } else {
                self.send_error(token, 403);
                return;
            }
        }

        // --- 7. CGI EXECUTION ---
        if is_cgi {
            if !full_path.exists() {
                self.send_error(token, 404);
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

            // Pass headers to CGI
            if let Some(ct) = headers.get("content-type") {
                env_vars.insert("CONTENT_TYPE".to_string(), ct.clone());
            }
            if let Some(cl) = headers.get("content-length") {
                env_vars.insert("CONTENT_LENGTH".to_string(), cl.clone());
            } else {
                env_vars.insert("CONTENT_LENGTH".to_string(), body.len().to_string());
            }

            if
                let Err(e) = self.start_cgi_process(
                    token,
                    &script_path_str,
                    route.cgi_interpreter.as_deref(),
                    &body,
                    env_vars
                )
            {
                eprintln!("[CGI Error] {}", e);
                self.send_error(token, 500);
            }
            return;
        }

        // --- 8. STATIC FILE SERVING ---
        match std::fs::read(&full_path) {
            Ok(content) => {
                let mime = Self::get_mime_type(full_path.to_str().unwrap_or(""));
                self.send_bytes_response(token, 200, content, mime);
            }
            Err(_) => self.send_error(token, 404),
        }
    }
    fn send_error(&mut self, token: Token, code: u16) {
        let status_text = Self::reason_phrase(code);

        // 1. Determine which server config we are using
        let server_idx = self.connections
            .get(&token)
            .map(|c| c.server_idx)
            .unwrap_or(0);
        let server_cfg = &self.config.servers[server_idx];

        // 2. Default Fallback Body
        let mut body = format!(
            "<html><head><title>{} {}</title></head>\
        <body style='font-family:sans-serif; text-align:center; padding-top:50px;'>\
        <h1>{} {}</h1><hr><p>Localhost_RS Server</p></body></html>",
            code,
            status_text,
            code,
            status_text
        );

        // 3. Try to find the custom error page from YAML
        if let Some(custom_path) = server_cfg.error_pages.get(&code) {
            let candidate_paths = [
                custom_path.clone(),
                custom_path.trim_start_matches('/').to_string(),
                format!("./{}", custom_path.trim_start_matches('/')),
            ];

            let mut loaded = false;
            let mut last_error = None;
            for candidate in candidate_paths {
                match std::fs::read_to_string(&candidate) {
                    Ok(content) => {
                        body = content;
                        loaded = true;
                        break;
                    }
                    Err(e) => {
                        last_error = Some(e);
                    }
                }
            }

            if !loaded {
                if let Some(e) = last_error {
                    eprintln!(
                        "[Config] Custom error page {} defined but could not be read: {}",
                        custom_path,
                        e
                    );
                }
            }
        }

        self.send_text_response(token, code, &body, "text/html");
    }

    fn send_text_response(
        &mut self,
        token: Token,
        status_code: u16,
        body: &str,
        content_type: &str
    ) {
        self.send_bytes_response(token, status_code, body.as_bytes().to_vec(), content_type);
    }

    fn send_bytes_response(
        &mut self,
        token: Token,
        status_code: u16,
        body: Vec<u8>,
        content_type: &str
    ) {
        let headers = vec![("Content-Type".to_string(), content_type.to_string())];
        let response = Self::build_http_response(
            status_code,
            Self::reason_phrase(status_code),
            headers,
            &body,
            true
        );
        self.finalize_response(token, response);
    }

    fn finalize_response(&mut self, token: Token, response_bytes: Vec<u8>) {
        if let Some(conn) = self.connections.get_mut(&token) {
            conn.write_buffer = response_bytes;
            conn.state = ConnectionState::WriteResponse;
            conn.last_activity = std::time::Instant::now();

            // Switch MIO from waiting for READ to waiting for WRITE
            if
                let Err(e) = self.poll
                    .registry()
                    .reregister(&mut conn.stream, token, mio::Interest::WRITABLE)
            {
                eprintln!("[Mio] Failed to reregister token {:?}: {}", token, e);
                self.close_connection(token);
            }
        }
    }

    fn check_timeouts(&mut self) {
        let now = Instant::now();
        let timeout = std::time::Duration::from_secs(self.config.timeout_seconds);
        let to_remove: Vec<Token> = self.connections
            .iter()
            .filter(|(_, conn)| {
                conn.state != ConnectionState::CgiPending &&
                    now.duration_since(conn.last_activity) > timeout
            })
            .map(|(&t, _)| t)
            .collect();

        for t in to_remove {
            self.close_connection(t);
        }
    }

    fn check_cgi_timeouts(&mut self) {
        let timeout = Duration::from_secs(self.config.timeout_seconds);
        let now = Instant::now();

        let timed_out: Vec<Token> = self.pending_cgi
            .iter()
            .filter(|(_, pending)| now.duration_since(pending.started_at) > timeout)
            .map(|(&client_token, _)| client_token)
            .collect();

        for client_token in timed_out {
            if let Some(mut pending) = self.remove_pending_cgi(client_token) {
                let _ = pending.child.kill();
                let _ = pending.child.wait();
            }
            if self.connections.contains_key(&client_token) {
                self.send_error(client_token, 504);
            }
        }
    }

    fn accept_connection(&mut self, server_token: Token) {
        let server_idx = self.listeners.get(&server_token).unwrap().server_idx;

        loop {
            match self.listeners.get_mut(&server_token).unwrap().listener.accept() {
                Ok((mut stream, _)) => {
                    let token = Token(self.next_token);
                    self.next_token += 1;

                    self.poll.registry().register(&mut stream, token, Interest::READABLE).ok();

                    self.connections.insert(token, Connection::new(stream, server_idx));
                    println!("[Network] New client Token {:?}", token);
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    break;
                }
                Err(_) => {
                    break;
                }
            }
        }
    }

    fn close_connection(&mut self, token: Token) {
        if let Some(mut pending) = self.remove_pending_cgi(token) {
            let _ = pending.child.kill();
            let _ = pending.child.wait();
        }
        if let Some(mut conn) = self.connections.remove(&token) {
            let _ = self.poll.registry().deregister(&mut conn.stream);
        }
    }

    fn handle_cgi_event(&mut self, cgi_token: Token, event: &mio::event::Event) {
        let client_token = match self.cgi_token_to_client.get(&cgi_token) {
            Some(token) => *token,
            None => {
                return;
            }
        };

        if event.is_readable() || event.is_read_closed() {
            self.poll_cgi_process(client_token);
        }
    }

    fn start_cgi_process(
        &mut self,
        client_token: Token,
        script_path: &str,
        interpreter: Option<&str>,
        body: &[u8],
        env_vars: std::collections::HashMap<String, String>
    ) -> Result<(), String> {
        let mut child = spawn_cgi_process(script_path, interpreter, body, env_vars)?;
        let stdout = child.stdout.take().ok_or("CGI stdout not available")?;

        Self::set_nonblocking_fd(stdout.as_raw_fd())?;

        let io_token = Token(self.next_token);
        self.next_token += 1;

        self.register_raw_fd(stdout.as_raw_fd(), io_token, Interest::READABLE)?;

        self.pending_cgi.insert(client_token, PendingCgi {
            child,
            stdout,
            output: Vec::new(),
            io_token,
            started_at: Instant::now(),
        });
        self.cgi_token_to_client.insert(io_token, client_token);

        if let Some(conn) = self.connections.get_mut(&client_token) {
            conn.state = ConnectionState::CgiPending;
            conn.last_activity = Instant::now();
        }

        Ok(())
    }

    fn poll_cgi_process(&mut self, client_token: Token) {
        let mut should_finalize = false;
        let mut process_error = None;

        {
            let pending = match self.pending_cgi.get_mut(&client_token) {
                Some(p) => p,
                None => {
                    return;
                }
            };

            let mut buf = [0u8; 8192];
            loop {
                match pending.stdout.read(&mut buf) {
                    Ok(0) => {
                        break;
                    }
                    Ok(n) => {
                        pending.output.extend_from_slice(&buf[..n]);
                        if let Some(conn) = self.connections.get_mut(&client_token) {
                            conn.last_activity = Instant::now();
                        }
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        break;
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {
                        continue;
                    }
                    Err(e) => {
                        process_error = Some(format!("CGI stdout read failed: {}", e));
                        break;
                    }
                }
            }

            if process_error.is_none() {
                match pending.child.try_wait() {
                    Ok(Some(_)) => {
                        should_finalize = true;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        process_error = Some(format!("CGI process check failed: {}", e));
                    }
                }
            }
        }

        if let Some(err) = process_error {
            eprintln!("[CGI Error] {}", err);
            let _ = self.remove_pending_cgi(client_token);
            if self.connections.contains_key(&client_token) {
                self.send_error(client_token, 500);
            }
            return;
        }

        if should_finalize {
            if let Some(mut pending) = self.remove_pending_cgi(client_token) {
                let _ = pending.child.wait();
                if self.connections.contains_key(&client_token) {
                    let response_bytes = Self::build_cgi_response(&pending.output);
                    self.finalize_response(client_token, response_bytes);
                }
            }
        }
    }

    fn check_cgi_progress(&mut self) {
        let pending_tokens: Vec<Token> = self.pending_cgi.keys().copied().collect();
        for client_token in pending_tokens {
            self.poll_cgi_process(client_token);
        }
    }

    fn remove_pending_cgi(&mut self, client_token: Token) -> Option<PendingCgi> {
        let pending = self.pending_cgi.remove(&client_token)?;
        self.cgi_token_to_client.remove(&pending.io_token);
        let _ = self.deregister_raw_fd(pending.stdout.as_raw_fd());
        Some(pending)
    }

    fn register_raw_fd(
        &self,
        raw_fd: std::os::fd::RawFd,
        token: Token,
        interest: Interest
    ) -> Result<(), String> {
        let mut source = SourceFd(&raw_fd);
        self.poll
            .registry()
            .register(&mut source, token, interest)
            .map_err(|e| e.to_string())
    }

    fn deregister_raw_fd(&self, raw_fd: std::os::fd::RawFd) -> Result<(), String> {
        let mut source = SourceFd(&raw_fd);
        self.poll
            .registry()
            .deregister(&mut source)
            .map_err(|e| e.to_string())
    }

    fn set_nonblocking_fd(raw_fd: std::os::fd::RawFd) -> Result<(), String> {
        let flags = unsafe { libc::fcntl(raw_fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(format!("fcntl(F_GETFL) failed: {}", io::Error::last_os_error()));
        }

        if (unsafe { libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) }) < 0 {
            return Err(format!("fcntl(F_SETFL) failed: {}", io::Error::last_os_error()));
        }

        Ok(())
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|pos| pos + 4)
    }

    fn extract_content_length(header_bytes: &[u8]) -> Option<usize> {
        let header = std::str::from_utf8(header_bytes).ok()?;
        for line in header.lines() {
            if let Some((key, value)) = line.split_once(':') {
                if key.trim().eq_ignore_ascii_case("content-length") {
                    return value.trim().parse::<usize>().ok();
                }
            }
        }
        None
    }

    fn extract_raw_upload_filename(
        request_path: &str,
        route_path: &str,
        headers: &std::collections::HashMap<String, String>
    ) -> String {
        if let Some(disposition) = headers.get("content-disposition") {
            if let Some(filename) = Self::extract_filename_from_disposition(disposition) {
                return filename;
            }
        }

        let request_name = Path::new(request_path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        let route_name = Path::new(route_path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        if let Some(name) = request_name {
            if route_name.map_or(true, |route_segment| route_segment != name) {
                return name.to_string();
            }
        }

        "upload.bin".to_string()
    }

    fn extract_filename_from_disposition(disposition: &str) -> Option<String> {
        for part in disposition.split(';') {
            let trimmed = part.trim();
            if let Some(value) = trimmed.strip_prefix("filename=") {
                let unquoted = value.trim().trim_matches('"').trim_matches('\'');
                let safe_name = Path::new(unquoted).file_name()?.to_str()?.trim();
                if !safe_name.is_empty() {
                    return Some(safe_name.to_string());
                }
            }
        }
        None
    }

    fn path_matches_route(path: &str, route_path: &str) -> bool {
        if route_path == "/" {
            return path.starts_with('/');
        }

        path == route_path || path.starts_with(&format!("{}/", route_path.trim_end_matches('/')))
    }

    fn try_serve_upload_file(&mut self, token: Token, server_idx: usize, path_only: &str) -> bool {
        let server_cfg = &self.config.servers[server_idx];

        for route in &server_cfg.routes {
            let upload_dir = match &route.upload_dir {
                Some(path) => path,
                None => {
                    continue;
                }
            };

            let can_get = route.methods.is_empty() || route.methods.iter().any(|m| m == "GET");
            if !can_get {
                continue;
            }

            let upload_prefix = match
                Path::new(upload_dir)
                    .file_name()
                    .and_then(|n| n.to_str())
            {
                Some(name) if !name.is_empty() => format!("/{}", name),
                _ => {
                    continue;
                }
            };

            let mut matched_prefix: Option<&str> = None;
            for prefix in [upload_prefix.as_str(), route.path.as_str()] {
                if Self::path_matches_route(path_only, prefix) {
                    matched_prefix = Some(prefix);
                    break;
                }
            }

            let Some(prefix) = matched_prefix else {
                continue;
            };

            let relative = path_only.strip_prefix(prefix).unwrap_or("").trim_start_matches('/');
            if relative.is_empty() {
                continue;
            }

            let mut full_path = std::path::PathBuf::from(upload_dir);
            full_path.push(relative);

            if full_path.is_dir() {
                self.send_error(token, 403);
                return true;
            }

            match std::fs::read(&full_path) {
                Ok(content) => {
                    let mime = Self::get_mime_type(full_path.to_str().unwrap_or(""));
                    self.send_bytes_response(token, 200, content, mime);
                }
                Err(_) => {
                    self.send_error(token, 404);
                }
            }
            return true;
        }

        false
    }

    fn get_mime_type(path: &str) -> &str {
        if path.ends_with(".html") {
            "text/html"
        } else if path.ends_with(".css") {
            "text/css"
        } else if path.ends_with(".js") {
            "application/javascript"
        } else {
            "text/plain"
        }
    }

    fn build_cgi_response(output: &[u8]) -> Vec<u8> {
        if output.starts_with(b"HTTP/") {
            return output.to_vec();
        }

        let (header_part, body_part) = if
            let Some(pos) = output.windows(4).position(|w| w == b"\r\n\r\n")
        {
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

        for (k, _) in &headers {
            if k.eq_ignore_ascii_case("content-type") {
                has_content_type = true;
            }
            if k.eq_ignore_ascii_case("content-length") {
                has_content_length = true;
            }
        }

        if !has_content_type {
            headers.push(("Content-Type".to_string(), "text/plain".to_string()));
        }
        if !has_content_length {
            headers.push(("Content-Length".to_string(), body_part.len().to_string()));
        }

        Self::build_http_response(status_code, &status_text, headers, body_part, true)
    }

    fn build_http_response(
        status_code: u16,
        status_text: &str,
        headers: Vec<(String, String)>,
        body: &[u8],
        close_connection: bool
    ) -> Vec<u8> {
        let mut has_content_length = false;
        let mut header_lines = String::new();

        for (key, value) in headers {
            if key.eq_ignore_ascii_case("content-length") {
                has_content_length = true;
            }
            header_lines.push_str(&format!("{}: {}\r\n", key, value));
        }

        if !has_content_length {
            header_lines.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }

        header_lines.push_str("Server: Localhost_RS\r\n");
        header_lines.push_str(
            if close_connection {
                "Connection: close\r\n"
            } else {
                "Connection: keep-alive\r\n"
            }
        );

        let mut response = format!(
            "HTTP/1.1 {} {}\r\n{}\r\n",
            status_code,
            status_text,
            header_lines
        ).into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn reason_phrase(status_code: u16) -> &'static str {
        match status_code {
            200 => "OK",
            201 => "Created",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            413 => "Payload Too Large",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            504 => "Gateway Timeout",
            _ => "Internal Server Error",
        }
    }

    fn handle_multipart_upload(
        &self,
        form: crate::http::request::MultipartForm,
        upload_dir: &std::path::Path
    ) -> Result<(), String> {
        // Convention: Create the "uploads" folder if it doesn't exist inside the root
        if !upload_dir.exists() {
            std::fs::create_dir_all(upload_dir).map_err(|e| e.to_string())?;
        }

        for file in form.files {
            let safe_name = std::path::Path
                ::new(&file.file_name)
                .file_name()
                .ok_or("Invalid filename")?;
            let mut dest = upload_dir.to_path_buf();
            dest.push(safe_name);

            std::fs::write(&dest, &file.data).map_err(|e| e.to_string())?;
            println!("[Upload] Multipart saved to: {:?}", dest);
        }
        Ok(())
    }

    fn handle_raw_upload(
        &self,
        body: &[u8],
        upload_dir: &std::path::Path,
        filename: &str
    ) -> Result<(), String> {
        if !upload_dir.exists() {
            std::fs::create_dir_all(upload_dir).map_err(|e| e.to_string())?;
        }

        let mut dest = upload_dir.to_path_buf();
        dest.push(filename);

        std::fs::write(&dest, body).map_err(|e| e.to_string())?;
        println!("[Upload] Raw Body saved to: {:?}", dest);
        Ok(())
    }
}
