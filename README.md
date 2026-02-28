# localserver

`localserver` is a custom HTTP/1.1 web server written in Rust.

It runs in a single process and single thread, using a non-blocking event loop based on `mio`.

## Features

- Event-driven non-blocking I/O (`mio`)
- Multi-listener setup (multiple server blocks)
- Static file serving
- CGI execution (configured by extension/interpreter)
- File uploads (raw and multipart)
- Chunked + unchunked request body handling
- Route method control (`GET`, `POST`, `DELETE`)
- Route redirections
- Directory index file + autoindex listing
- Host-based virtual server selection (`Host` header)
- Custom error pages + fallback HTML
- Client body-size and timeout limits
- Basic cookie/session support (`SESSION_ID`)

## Project Structure

```text
.
├── Cargo.toml
├── config.yaml
├── src/
│   ├── main.rs
│   ├── config/
│   │   ├── mod.rs
│   │   ├── models.rs
│   │   └── parser.rs
│   ├── handlers/
│   │   ├── mod.rs
│   │   └── cgi.rs
│   ├── http/
│   │   ├── mod.rs
│   │   └── request.rs
│   └── server/
│       ├── connection.rs
│       └── mod.rs
├── tests/
│   └── audit_smoke.sh
└── www/
```

## Requirements

- Rust (edition 2024)
- Cargo

## Build and Run

```bash
cargo check
cargo run
```

Server loads configuration from `config.yaml`.

## Configuration Overview

Top-level:

- `max_server_size`
- `timeout_seconds`
- `servers`

Per server:

- `host`
- `port`
- `server_name`
- `max_body_size`
- `error_pages`
- `routes`

Per route:

- `path`
- `root`
- `methods`
- `index`
- `autoindex`
- `redirect`
- `upload_dir`
- `cgi_extension`
- `cgi_interpreter`

## Quick Validation

Run smoke tests:

```bash
./tests/audit_smoke.sh
```

The script checks:

- `GET /`
- `POST /upload`
- `GET /upload/<file>` and `GET /uploads/<file>`
- `DELETE /upload/<file>`
- Redirect behavior
- Chunked upload
- Session cookie issuance
- Host-header virtual server request

## Notes

- No async runtime/framework is used (`tokio`, `hyper`, `axum`, etc.).
- The code is intentionally modular: config parsing, request parsing, CGI, and server loop are separated.
- For stress evidence (availability target), run your own benchmark (e.g. `siege`) and keep the output as audit proof.
