use std::collections::HashMap;

pub struct HttpRequest {
    pub method: String,
    pub uri: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct UploadedFile {
    pub file_name: String,
    pub data: Vec<u8>,
}

pub struct MultipartForm {
    pub files: Vec<UploadedFile>,
}

impl HttpRequest {
    pub fn parse(raw_data: &[u8]) -> Option<Self> {
        let header_end = Self::find_header_end(raw_data)?;
        let header_bytes = &raw_data[..header_end];

        let header_str = std::str::from_utf8(header_bytes).ok()?;
        let mut lines = header_str.split("\r\n");

        let first_line = lines.next()?;
        let mut parts = first_line.split_whitespace();
        let method = parts.next()?.to_string();
        let uri = parts.next()?.to_string();

        let mut headers = HashMap::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some((key, val)) = line.split_once(':') {
                headers.insert(key.trim().to_lowercase(), val.trim().to_string());
            }
        }

        let body_slice = &raw_data[header_end..];
        let body = if Self::is_chunked_transfer(&headers) {
            let (decoded, _) = Self::decode_chunked_body(body_slice)?;
            decoded
        } else {
            let content_length = Self::get_content_length(header_bytes).unwrap_or(0);
            if body_slice.len() < content_length {
                return None;
            }
            body_slice[..content_length].to_vec()
        };

        Some(HttpRequest {
            method,
            uri,
            headers,
            body,
        })
    }

    pub fn is_complete(buf: &[u8]) -> bool {
        if let Some(header_end) = Self::find_header_end(buf) {
            let header_bytes = &buf[..header_end];
            let body_slice = &buf[header_end..];

            if let Some(headers) = Self::parse_headers_map(header_bytes) {
                if Self::is_chunked_transfer(&headers) {
                    return Self::decode_chunked_body(body_slice).is_some();
                }
            }

            let content_length = Self::get_content_length(header_bytes).unwrap_or(0);
            return body_slice.len() >= content_length;
        }
        false
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|pos| pos + 4)
    }

    fn get_content_length(header_bytes: &[u8]) -> Option<usize> {
        let header_str = std::str::from_utf8(header_bytes).ok()?;
        for line in header_str.lines() {
            let line_lower = line.to_lowercase();
            if line_lower.starts_with("content-length:") {
                return line_lower["content-length:".len()..].trim().parse().ok();
            }
        }
        None
    }

    fn parse_headers_map(header_bytes: &[u8]) -> Option<HashMap<String, String>> {
        let header_str = std::str::from_utf8(header_bytes).ok()?;
        let mut lines = header_str.split("\r\n");
        let _ = lines.next()?;

        let mut headers = HashMap::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some((key, val)) = line.split_once(':') {
                headers.insert(key.trim().to_lowercase(), val.trim().to_string());
            }
        }
        Some(headers)
    }

    fn is_chunked_transfer(headers: &HashMap<String, String>) -> bool {
        headers
            .get("transfer-encoding")
            .map(|v| { v.split(',').any(|part| part.trim().eq_ignore_ascii_case("chunked")) })
            .unwrap_or(false)
    }

    fn decode_chunked_body(body: &[u8]) -> Option<(Vec<u8>, usize)> {
        let mut decoded = Vec::new();
        let mut pos = 0usize;

        loop {
            let line_end_rel = body
                .get(pos..)?
                .windows(2)
                .position(|w| w == b"\r\n")?;
            let line_end = pos + line_end_rel;
            let size_line = std::str::from_utf8(&body[pos..line_end]).ok()?;
            let size_hex = size_line.split(';').next()?.trim();
            let chunk_size = usize::from_str_radix(size_hex, 16).ok()?;
            pos = line_end + 2;

            if chunk_size == 0 {
                let trailers = body.get(pos..)?;
                if trailers.starts_with(b"\r\n") {
                    pos += 2;
                    return Some((decoded, pos));
                }

                let trailer_end = trailers.windows(4).position(|w| w == b"\r\n\r\n")?;
                pos += trailer_end + 4;
                return Some((decoded, pos));
            }

            if body.len() < pos + chunk_size + 2 {
                return None;
            }

            decoded.extend_from_slice(&body[pos..pos + chunk_size]);
            pos += chunk_size;

            if &body[pos..pos + 2] != b"\r\n" {
                return None;
            }
            pos += 2;
        }
    }

    pub fn parse_multipart(
        headers: &HashMap<String, String>,
        body: &[u8]) -> Option<MultipartForm> {
        let content_type = headers.get("content-type")?;
        if !content_type.contains("multipart/form-data") {
            return None;
        }

        let boundary = content_type.split("boundary=").nth(1)?.trim();
        let boundary_bytes = format!("--{}", boundary).into_bytes();

        let mut files = Vec::new();
        let mut current_pos = 0;

        while let Some(start_pos) = Self::find_bytes(body, &boundary_bytes, current_pos) {
            let part_search_start = start_pos + boundary_bytes.len();

            let end_pos = match Self::find_bytes(body, &boundary_bytes, part_search_start) {
                Some(pos) => pos,
                None => {
                    break;
                } // End of multipart
            };

            let part_data = &body[part_search_start..end_pos];
            if let Some(file) = Self::parse_multipart_part(part_data) {
                files.push(file);
            }
            current_pos = end_pos;
        }

        Some(MultipartForm { files })
    }

    fn parse_multipart_part(part_data: &[u8]) -> Option<UploadedFile> {
        let data = if part_data.starts_with(b"\r\n") { &part_data[2..] } else { part_data };

        let header_end = Self::find_header_end(data)?;
        let header_bytes = &data[..header_end];
        let file_content = &data[header_end..];

        let actual_file_data = if file_content.ends_with(b"\r\n") {
            &file_content[..file_content.len() - 2]
        } else {
            file_content
        };

        let header_str = std::str::from_utf8(header_bytes).ok()?;
        let mut file_name = String::new();
        for line in header_str.lines() {
            if line.to_lowercase().starts_with("content-disposition:") {
                if
                    let Some(f) = line
                        .split("filename=\"")
                        .nth(1)
                        .and_then(|s| s.split('"').next())
                {
                    file_name = f.to_string();
                }
            }
        }

        if file_name.is_empty() {
            return None;
        }

        Some(UploadedFile {
            file_name,
            data: actual_file_data.to_vec(),
        })
    }

    fn find_bytes(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
        haystack
            .get(start..)?
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|pos| pos + start)
    }
}
