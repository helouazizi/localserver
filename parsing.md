To keep your code clean and satisfy the "separation of concerns" requirement, we will treat parsing as two distinct architectural layers: **Static Parsing** (Config) and **Stream Parsing** (HTTP).

---

### 1. Manual YAML Parser Strategy (The "Indentation Scanner")

Since you cannot use `serde`, you must treat YAML as a tree based on indentation.

#### **Logic Flow:**
1.  **Read lines**: Filter out empty lines.
2.  **Calculate Depth**: Count the leading spaces (e.g., 0 spaces = Root, 2 spaces = Server, 4 spaces = Route).
3.  **Key-Value Extraction**: Split by the first `:` found.
4.  **Stateful Mapping**: Use a "Current Context" pointer to know if you are currently filling a `Server` or a `Route`.

#### **Implementation Strategy (`src/config/parser.rs`):**
```rust
// Logic concept
for line in file_lines {
    let indent = count_leading_spaces(&line);
    let (key, value) = split_at_colon(&line);

    match indent {
        0 => { /* New Server Block starts here */ }
        2 => { /* Filling Server Fields (host, port, etc.) */ }
        4 => { /* Filling Route Fields (path, methods) */ }
        _ => { /* Error or Nested Data */ }
    }
}
```

**Why this works:** It avoids complex regex. By simply counting spaces, you mimic the YAML hierarchy.

---

### 2. HTTP Request Parser Strategy (The "State Machine")

This is the most difficult part of the project because data arrives in fragments. You **must not** assume you have the whole request.

#### **The States (`src/http/request.rs`):**
*   **`START`**: Waiting for the first byte.
*   **`REQUEST_LINE`**: Reading until `\r\n`.
*   **`HEADERS`**: Reading line by line until `\r\n\r\n`.
*   **`BODY_FIXED`**: If `Content-Length` exists, read $N$ bytes.
*   **`BODY_CHUNKED`**: If `Transfer-Encoding: chunked` exists, enter the Chunked State Machine.
*   **`COMPLETE`**: Ready to generate a response.

#### **Logic for "Chunked" Parsing:**
Chunked encoding is a "loop within a state." For every chunk:
1.  **Read Size**: Read bytes until `\r\n`, convert hex string (e.g., `"1A"`) to integer (26).
2.  **Read Data**: Read exactly that many bytes.
3.  **Read Trailer**: Skip the following `\r\n`.
4.  **Check for End**: If Size was `0`, move to `COMPLETE`.

---

### 3. Detailed Data Flow

#### **A. How to handle partial data?**
Every `Connection` has a `read_buffer: Vec<u8>`. 
1. `libc::read` appends new bytes to the end of `read_buffer`.
2. The Parser looks at `read_buffer`.
3. If it finds a complete "token" (like a line ending in `\r\n`), it **removes** those bytes from the front of the buffer and updates the Request object.
4. If no complete token is found, the Parser returns `Pending` and waits for the next `epoll` event.

#### **B. Path Sanitization Logic**
To prevent `GET /../../etc/passwd`:
1.  Take the requested URI (e.g., `/images/../../config.yaml`).
2.  Split by `/`.
3.  Create a new stack (Vector).
4.  For each part: If it is `..`, pop from stack. If it is `.` or empty, ignore. Otherwise, push.
5.  Rejoin the stack. This ensures the user stays inside the `root` folder.

---

### 4. Summary of Strategy for your Code

| Feature | Parser Type | Critical Tool |
| :--- | :--- | :--- |
| **config.yaml** | One-time / Block | `String::split_once(':')` |
| **Request Line** | Stream / String | `raw_data.windows(2).position(|w| w == b"\r\n")` |
| **Headers** | Stream / Map | `HashMap<String, String>` |
| **Chunked Body** | Stream / Hex | `u32::from_str_radix(hex_str, 16)` |
| **File Upload** | Stream / Binary | `std::fs::File` with non-blocking writes |

---

### What to code next? (Action Plan)

**Safety Note:** Always check `Content-Length` against your `client_max_body_size` **before** you start reading the body into memory to prevent a memory-exhaustion attack (DDoS).