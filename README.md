# localserver


This is a significant systems programming challenge. Building a high-performance, non-blocking, single-threaded web server in Rust without using tokio or nix requires a deep understanding of the Linux epoll API and the HTTP/1.1 protocol.
***

Given the scope, I will provide a robust structural foundation, the core event loop using libc and epoll, and the architectural pattern for separation of concerns.

## Project Structure
```
.
├── Cargo.toml            # Rust project manifest
├── src/
│   ├── main.rs           # Application entry point
│   ├── config/           # Configuration parsing and validation
│   ├── network/          # Epoll abstraction and low-level socket handling
│   ├── http/             # HTTP request/response models and parsing logic
│   ├── server/           # Core reactor, event loop, and state management
│   └── handlers/         # Routing, static file serving, and CGI handling
└── www/                  # Public web root (static assets)
```


1. Cargo.toml
We only use libc for system calls.
```gargo.toml```

2. The Core: Non-blocking Epoll Wrapper
This handles the low-level OS event notifications.
``` src/network/poll.rs```

3. Connection State Management
Since we are single-threaded and non-blocking, we must store the "state" of every client (buffers, partial requests).
```src/server/connection.rs```
4. The Server Reactor
This coordinates the listening sockets and client connections.
```src/server/mod.rs```

5. HTTP Parser Concept
You should implement a simple state machine to parse the request header line by line.
```src/http/request.rs```

6. CGI Handling
Since you are allowed to fork, here is how you separate the CGI concern.
```src/handlers/cgi.rs```


### Key Implementation Guidelines to meet requirements:

1.  **Non-Blocking Sockets**: When you `accept` a connection or create a listener, you must set the O_NONBLOCK flag using `fcntl`.
    ```rust
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    ```
2.  **Edge-Triggered (EPOLLET)**: Use Edge-Triggered mode for epoll. This means you must read until `EAGAIN` or `EWOULDBLOCK`.
3.  **Chunked Requests**: You need a specific parser state that reads the hex size, then reads that many bytes, then repeats until a size of 0.
4.  **Configuration**: Create a `Config` struct. Parse the file into a `Vec<ServerBlock>`. Each `ServerBlock` contains `Route` objects.
5.  **Memory Management**: Because you are using `libc` and `unsafe`, ensure you are not leaking file descriptors. Every `fd` added to epoll must eventually be `close()`ed. Rust's `Drop` trait on a wrapper struct is your friend here.

### How to approach the CGI requirement
The prompt says you can't use `tokio`, but you can `fork`. However, since your server is single-threaded and non-blocking, a blocking `child.wait_with_output()` will freeze the whole server.
*   **The Pro approach**: Use `libc::pipe` to create pipes for stdin/stdout, set them to non-blocking, add the pipe's FD to your `epoll` loop. When the pipe is ready to read, collect the CGI output asynchronously.

### Memory Leak Prevention
*   Run your server under `valgrind` or use the `heaptrack` tool.
*   Since you are avoiding threads, you don't need to worry about data races, but be careful with `unsafe` blocks when converting between raw pointers and Rust slices.

This structure provides the "Separation of Concerns" requested: `Poller` handles OS interaction, `Server` handles logic/flow, `Request/Response` handles protocol, and `Handlers` handle the actual content.