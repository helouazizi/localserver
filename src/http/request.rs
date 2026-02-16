use std::collections::HashMap;

pub struct HttpRequest {
    pub method: String,
    pub uri: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct UploadedFile {
    pub field_name: String,
    pub file_name: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct MultipartForm {
    pub files: Vec<UploadedFile>,
}

impl HttpRequest {
    pub fn parse(raw_data: &[u8]) -> Option<Self> {
        let header_end = Self::find_header_end(raw_data)?;
        let header_bytes = &raw_data[..header_end];

        let header_str = std::str::from_utf8(header_bytes).ok()?;
        let mut lines = header_str.split("\r\n");

        // ---- Request Line ----
        let first_line = lines.next()?;
        let mut parts = first_line.split_whitespace();
        let method = parts.next()?.to_string();
        let uri = parts.next()?.to_string();

        // ---- Headers ----
        let mut headers = HashMap::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some((key, val)) = line.split_once(':') {
                headers.insert(key.trim().to_lowercase(), val.trim().to_string());
            }
        }

        // ---- Body ----
        let body = if let Some(len) = Self::get_content_length(header_bytes) {
            raw_data[header_end..header_end + len.min(raw_data.len() - header_end)].to_vec()
        } else {
            Vec::new()
        };

        Some(HttpRequest {
            method,
            uri,
            headers,
            body,
        })
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|pos| pos + 4)
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

    // -------------------------------------------------
    // MULTIPART PARSER (Associated function, no self)
    // -------------------------------------------------
    pub fn parse_multipart(
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Option<MultipartForm> {
        let content_type = headers.get("content-type")?;

        if !content_type.starts_with("multipart/form-data") {
            return None;
        }

        let boundary = content_type
            .split("boundary=")
            .nth(1)?
            .trim();

        let boundary_marker = format!("--{}", boundary);
        let boundary_bytes = boundary_marker.as_bytes();

        let mut files = Vec::new();
        let mut start = 0;

        while let Some(boundary_pos) = Self::find_bytes(body, boundary_bytes, start) {
            let mut part_start = boundary_pos + boundary_bytes.len();

            // Skip CRLF
            if body.get(part_start..part_start + 2) == Some(b"\r\n") {
                part_start += 2;
            }

            if let Some(next_boundary) =
                Self::find_bytes(body, boundary_bytes, part_start)
            {
                let part = &body[part_start..next_boundary - 2];

                if let Some(file) = Self::parse_part(part) {
                    files.push(file);
                }

                start = next_boundary;
            } else {
                break;
            }
        }

        Some(MultipartForm { files })
    }

    fn parse_part(part: &[u8]) -> Option<UploadedFile> {
        let header_end = Self::find_header_end(part)?;
        let header_bytes = &part[..header_end];
        let data = &part[header_end..];

        let header_str = std::str::from_utf8(header_bytes).ok()?;

        let mut field_name = String::new();
        let mut file_name = String::new();
        let mut content_type = String::new();

        for line in header_str.lines() {
            if line.starts_with("Content-Disposition") {
                if let Some(name_part) = line.split("name=\"").nth(1) {
                    field_name = name_part.split('"').next()?.to_string();
                }
                if let Some(file_part) = line.split("filename=\"").nth(1) {
                    file_name = file_part.split('"').next()?.to_string();
                }
            }

            if line.starts_with("Content-Type") {
                content_type = line
                    .split(':')
                    .nth(1)?
                    .trim()
                    .to_string();
            }
        }

        if file_name.is_empty() {
            return None;
        }

        Some(UploadedFile {
            field_name,
            file_name,
            content_type,
            data: data.to_vec(),
        })
    }

    fn find_bytes(
        haystack: &[u8],
        needle: &[u8],
        start: usize,
    ) -> Option<usize> {
        haystack[start..]
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|pos| pos + start)
    }
}
