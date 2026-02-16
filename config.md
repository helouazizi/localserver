YAML is an excellent choice for this project because it is highly structured, readable, and maps perfectly to Rust's `struct` data types.

Since you are not allowed to use high-level crates like `serde_yaml`, you would typically parse this by looking at indentation levels or using a simple YAML logic.

### 1. The Configuration File (`config.yaml`)

Here is how the server configuration looks refactored into YAML:

```yaml
# Global list of server instances
servers:
  - host: "127.0.0.1"             # The IP address to bind to
    ports: [8080, 8081]           # List of ports this server block listens on
    server_name: "myserver.com"   # Used for virtual hosting (Host header matching)
    client_max_body_size: 1048576 # Max upload size in bytes (1MB)

    # Mapping error codes to custom HTML files
    error_pages:
      404: "/www/errors/404.html"
      413: "/www/errors/413.html"
      500: "/www/errors/500.html"

    # Route definitions
    routes:
      - path: "/"                 # The URL prefix
        root: "./www/html"        # Local directory path
        methods: ["GET"]          # Allowed HTTP methods
        index: "index.html"       # Default file for directory requests
        autoindex: true           # Enable directory listing if index is missing

      - path: "/uploads"
        root: "./www/uploads"
        methods: ["POST", "DELETE"]
        client_max_body_size: 5000000 # Override global limit (5MB) for this route

      - path: "/old-site"
        redirect: "/new-site"     # HTTP 301 Redirection
        
      - path: "/cgi-bin"
        root: "./scripts"
        cgi_extension: ".py"      # Files ending in .py will be executed
        cgi_interpreter: "/usr/bin/python3"
```

---

### 2. Detailed Documentation

#### **Server-Level Settings**
*   **`servers:`**: A list of server configurations. Your program will loop through this list to create multiple listening sockets.
*   **`host`**: Specifies which network interface to use. `127.0.0.1` is local, `0.0.0.0` is public.
*   **`ports`**: An array. The project requires listening on multiple ports. In Rust, you will create one `TcpListener` for each port.
*   **`server_name`**: If two server blocks use the same port, the server checks the HTTP `Host` header. If `Host: myserver.com` is received, it uses this config.
*   **`client_max_body_size`**: The limit for `Content-Length`. If a client sends a `POST` request larger than this, the server stops reading and sends a `413 Payload Too Large` immediately.

#### **Error Handling**
*   **`error_pages`**: A dictionary/map.
    *   *Role:* When your code logic determines an error (e.g., a file isn't found), it checks this map.
    *   *Logic:* Instead of sending a blank 404, the server reads the file at the specified path and sends it as the response body.

#### **Route (Location) Settings**
*   **`path`**: The URI prefix. Your server must find the "longest match" (e.g., if you have `/` and `/uploads`, a request for `/uploads/img.png` matches the latter).
*   **`root`**: The base directory on your hard drive. 
    *   *Path Translation:* A request for `/uploads/file.txt` with root `./www/uploads` translates to opening `./www/uploads/file.txt`.
*   **`methods`**: A list of strings. If the client sends a `DELETE` request but the list only contains `["GET"]`, the server returns `405 Method Not Allowed`.
*   **`index`**: When the requested path is a directory (ends in `/`), the server appends this filename to the path.
*   **`autoindex`**: A boolean. If `true`, and no `index` file is found, the server generates an HTML string listing all files in the directory (using `std::fs::read_dir`).
*   **`redirect`**: If present, the server ignores the `root` and immediately sends a `301 Moved Permanently` header with the `Location: [path]` field.

#### **CGI Settings**
*   **`cgi_extension`**: Tells the server which files are scripts rather than static files.
*   **`cgi_interpreter`**: The absolute path to the binary that runs the script (Python, PHP, Ruby).

---

### 3. Data Structures in Rust (Separation of Concern)

To keep your code clean, you should map the YAML structure to Rust structs. This allows the rest of your code to use typed data instead of raw strings.

```src/config/models.rs
```

---

### 4. Why this is needed for the Localhost project

1.  **Multiple Ports Requirement**: The `ports: [8080, 8081]` layout makes it easy to satisfy the "listen on multiple ports" instruction.
2.  **One Process/One Thread**: Since you are single-threaded, you will parse this YAML once at startup, create all your listeners, and store them in your `Server` struct.
3.  **Scalability**: If you need to add a new website to your server, you just add a new item to the YAML `servers` list. You don't have to change a single line of Rust code.
4.  **CGI Mapping**: The YAML makes it clear which route is "static" and which route is "dynamic" (CGI) based on the presence of the `cgi_extension` key.

### How to parse without crates?
Since you can't use `serde_yaml`, your **Config Module** will:
1.  Read the file line by line.
2.  Count the leading spaces (indentation) to determine nesting.
3.  Split strings by `:` to get keys and values.
4.  Populate the structs manually. This demonstrates your ability to handle raw string manipulation in Rust.