To build a high-performance web server from scratch in Rust, you need to understand several low-level systems programming concepts. Because we are restricted to **one process, one thread, and no high-level crates**, the architecture relies heavily on how the operating system handles networking.

Here is a detailed explanation of each concept, its role, and why it is necessary.

---

### 1. Non-Blocking I/O
**The Concept:** Normally, when you call `read()` on a socket, the program "pauses" (blocks) until data arrives. In non-blocking mode, the function returns immediately. If no data is there, it returns an error code (like `EWOULDBLOCK`).

*   **Role:** It allows a single thread to manage thousands of connections. It ensures the server never "gets stuck" waiting for a slow client.
*   **Why:** Since we are using only **one thread**, if we blocked on a single client who is slow to send data, the entire server would freeze for everyone else.

### 2. I/O Multiplexing (`epoll`)
**The Concept:** `epoll` (on Linux) is a mechanism that allows the kernel to monitor a large number of file descriptors (sockets) and notify the program when one of them is "ready" (e.g., data has arrived to be read, or a buffer is empty to be written).

*   **Role:** It acts as the "Traffic Controller." Instead of the server constantly asking 1,000 clients "Are you ready?", the server asks the OS "Tell me which of these 1,000 clients did something."
*   **Why:** It is extremely efficient ($O(1)$ complexity in many cases). This is how Nginx and Node.js handle massive traffic. The project requirement specifically asks for `epoll` to minimize CPU usage while idling.

### 3. The Reactor Pattern (Event Loop)
**The Concept:** This is the heart of the server. It is a loop that:
1.  Calls `epoll_wait` to get a list of active events.
2.  Iterates through those events.
3.  Dispatches each event to a specific handler (e.g., "New connection," "Data received," "Ready to send").

*   **Role:** Orchestration. It manages the lifecycle of every request from the moment a socket opens to the moment it closes.
*   **Why:** It implements **Separation of Concern** at the execution level. The loop doesn't care *what* the data is; it only cares that data is *ready*.

### 4. Finite State Machines (FSM)
**The Concept:** Because I/O is non-blocking, a request might not arrive all at once. You might get the first 500 bytes of a header, then wait 10ms for the rest. An FSM tracks the "status" of each connection.
*   *States:* `ReadingHeaders` → `ReadingBody` → `Processing` → `WritingResponse` → `Closing`.

*   **Role:** Memory/Context Management. It remembers where a specific client is in the request process.
*   **Why:** Without this, you would lose track of partial requests. Since you can't use threads (where the stack keeps track of state), you must store the state manually in a `struct`.

### 5. HTTP/1.1 Parsing (The Protocol)
**The Concept:** Converting raw bytes into a structured `Request` object. This involves handling:
*   **The Request Line:** `GET /index.html HTTP/1.1`
*   **Headers:** Key-value pairs (e.g., `Content-Type: text/html`).
*   **Chunked Transfer Encoding:** A way of sending data in pieces when the total size isn't known yet.

*   **Role:** Communication. It ensures the server understands what the browser wants.
*   **Why:** Compliance with the RFC (Request for Comments) standard ensures your server works with Chrome, Firefox, and `curl`.

### 6. CGI (Common Gateway Interface)
**The Concept:** A standard way for a web server to execute an external program (like a Python or PHP script) to generate a web page dynamically.
*   The server `forks` a new process.
*   It passes request information via environment variables (like `PATH_INFO`).
*   The output of the script is captured and sent back to the browser.

*   **Role:** Dynamic Content. It moves the server beyond just serving static `.html` files.
*   **Why:** It separates the "Web Server" (which handles networking) from the "Application Logic" (which handles database queries or calculations).

### 7. Resource Management (RAII and File Descriptors)
**The Concept:** In Rust, Resource Acquisition Is Initialization (RAII) means when an object goes out of scope, its resources are cleaned up. In this project, you are dealing with "Raw File Descriptors" (integers).

*   **Role:** Preventing Memory Leaks and FD Leaks. Every socket opened must be closed. Every allocated buffer must be freed.
*   **Why:** The project requirements strictly state the server **must never crash and never leak**. In a long-running server, a tiny leak of 1KB per request will eventually crash the whole system.

### 8. Configuration Mapping (Routing)
**The Concept:** A logic layer that takes a URL (like `/api/upload`) and looks at the config file to decide:
*   Is this method (POST) allowed?
*   What is the root directory?
*   Is there a body size limit (to prevent DDoS)?

*   **Role:** Security and Flexibility. It acts as a "Guard" for your file system.
*   **Why:** You don't want a user to be able to request `GET /etc/passwd`. The routing layer ensures the user only sees what you explicitly allow in the config file.

---

### Summary of Separation of Concerns in this Codebase:

1.  **`network` module:** Only cares about bits, bytes, `epoll`, and raw sockets. (The "How").
2.  **`http` module:** Only cares about parsing strings and bytes into Request/Response objects. (The "What").
3.  **`server` module:** Only cares about the Event Loop and managing the list of active clients. (The "When").
4.  **`handlers` module:** Only cares about finding files on disk or running CGI scripts. (The "Action").

By separating these, you can change your CGI logic without breaking your socket logic, and vice versa.