pub mod connection;
use crate::config::models::Config;
use crate::handlers::cgi::execute_cgi;
use crate::server::connection::{ Connection, ConnectionState };

use mio::net::{ TcpListener };
use mio::{ Interest, Poll, Token };
use std::collections::HashMap;
use std::io::{ self, Read, Write };
use std::time::Instant;

const SERVER_TOKEN_MAX: usize = 100; // Assume max 100 server blocks

pub struct Server {
    poll: Poll,
    listeners: HashMap<Token, ListenerEntry>,
    connections: HashMap<Token, Connection>,
    config: Config,
    next_token: usize,
}

struct ListenerEntry {
    listener: TcpListener,
    server_idx: usize,
}

impl Server {
    pub fn new(config: Config) -> Self {
        Self {
            poll: Poll::new().expect("Failed to create mio poll"),
            listeners: HashMap::new(),
            connections: HashMap::new(),
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
                } else {
                    // This works now!
                    self.handle_client_event(token, event);
                }
            }
            self.check_timeouts();
        }
    }

    fn handle_client_event(&mut self, token: Token, event: &mio::event::Event) {
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
        let conn = match self.connections.get_mut(&token) {
            Some(c) => c,
            None => {
                return;
            }
        };

        let mut buf = [0u8; 4096];
        loop {
            match conn.stream.read(&mut buf) {
                Ok(0) => {
                    self.close_connection(token);
                    return;
                }
                Ok(n) => {
                    conn.read_buffer.extend_from_slice(&buf[..n]);
                    conn.last_activity = std::time::Instant::now();

                    // Use our refactored helper to check if the full request (Header + Body) is here
                    if crate::http::request::HttpRequest::is_complete(&conn.read_buffer) {
                        conn.request_complete = true;
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

        if conn.request_complete {
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
            None => return,
        };
        let idx = conn.server_idx;
        match crate::http::request::HttpRequest::parse(&conn.read_buffer) {
            Some(req) => (req.method, req.uri, req.headers, req.body, idx),
            None => ("".to_string(), "".to_string(), std::collections::HashMap::new(), Vec::new(), 999),
        }
    };

    if server_idx == 999 {
        self.send_error(token, 400, "Bad Request");
        return;
    }

    let (path_only, query_string) = match uri.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (uri.clone(), String::new()),
    };

    // --- 2. CONFIG & ROUTE LOOKUP ---
    let server_cfg = &self.config.servers[server_idx];
    let route = match server_cfg.routes.iter()
        .filter(|r| path_only.starts_with(&r.path))
        .max_by_key(|r| r.path.len()) 
    {
        Some(r) => r,
        None => { self.send_error(token, 404, "Not Found"); return; }
    };

    // --- 3. METHOD VALIDATION ---
    if !route.methods.contains(&method) && !route.methods.is_empty() {
        self.send_error(token, 405, "Method Not Allowed");
        return;
    }

    // --- 4. CONVENTION-BASED UPLOAD LOGIC ---
    // Rule: If it's POST/PUT and NOT a CGI script, treat it as an upload
    let is_cgi = route.cgi_extension.as_ref().map_or(false, |ext| path_only.ends_with(ext));

    if (method == "POST" || method == "PUT") && !is_cgi {
        let mut upload_path = std::path::PathBuf::from(&route.root);
        upload_path.push("uploads");

        let mut upload_performed = false;

        if let Some(form) = crate::http::request::HttpRequest::parse_multipart(&headers, &body) {
            if self.handle_multipart_upload(form, &upload_path).is_ok() {
                upload_performed = true;
            }
        } else if !body.is_empty() {
            let filename = path_only.split('/').last().unwrap_or("raw_upload.bin");
            if self.handle_raw_upload(&body, &upload_path, filename).is_ok() {
                upload_performed = true;
            }
        }

        if upload_performed {
            let response = "HTTP/1.1 201 Created\r\nContent-Length: 18\r\nConnection: keep-alive\r\n\r\nUpload Successful"
                .as_bytes().to_vec();
            self.finalize_response(token, response);
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
            self.send_error(token, 501, "Autoindex Not Implemented");
            return;
        } else {
            self.send_error(token, 403, "Forbidden");
            return;
        }
    }

    // --- 7. CGI EXECUTION ---
    if is_cgi {
        if !full_path.exists() {
            self.send_error(token, 404, "Not Found");
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

        let interpreter = route.cgi_interpreter.as_deref();
        match execute_cgi(&script_path_str, interpreter, &body, env_vars) {
            Ok(output) => {
                let response_bytes = Self::build_cgi_response(&output);
                self.finalize_response(token, response_bytes);
            }
            Err(e) => {
                eprintln!("[CGI Error] {}", e);
                self.send_error(token, 500, "CGI Execution Failed");
            }
        }
        return; 
    }

    // --- 8. STATIC FILE SERVING ---
    match std::fs::read(&full_path) {
        Ok(content) => {
            let mime = Self::get_mime_type(full_path.to_str().unwrap_or(""));
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                mime, content.len()
            );
            self.finalize_response(token, [header.as_bytes(), &content].concat());
        }
        Err(_) => self.send_error(token, 404, "Not Found"),
    }
}
    fn send_error(&mut self, token: Token, code: u16, msg: &str) {
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
            msg,
            code,
            msg
        );

        // 3. Try to find the custom error page from YAML
        if let Some(custom_path) = server_cfg.error_pages.get(&code) {
            // We trim start '/' because std::fs::read looks relative to the binary's execution path
            match std::fs::read_to_string(custom_path.trim_start_matches('/')) {
                Ok(content) => {
                    body = content;
                }
                Err(e) => {
                    eprintln!(
                        "[Config] Custom error page {} defined but could not be read: {}",
                        custom_path,
                        e
                    );
                }
            }
        }

        // 4. Build the Full HTTP Response
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

        // 5. Hand over to the network buffer and switch to WRITABLE
        self.finalize_response(token, response.into_bytes());
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
            .filter(|(_, conn)| now.duration_since(conn.last_activity) > timeout)
            .map(|(&t, _)| t)
            .collect();

        for t in to_remove {
            self.close_connection(t);
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
        if let Some(mut conn) = self.connections.remove(&token) {
            let _ = self.poll.registry().deregister(&mut conn.stream);
        }
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
            status_code,
            status_text,
            header_lines
        );

        [header.as_bytes(), body_part].concat()
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
