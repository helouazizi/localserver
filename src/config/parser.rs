use crate::config::models::{ Config, ServerConfig, RouteConfig };
use std::collections::HashMap;
use std::fs;

#[derive(PartialEq)]
enum ParseMode {
    General,
    ErrorPages,
    Routes,
}

pub fn parse_config(path: &str) -> Result<Config, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut config = Config { servers: Vec::new(), max_server_size: 10485760, timeout_seconds: 30 };

    let mut current_server: Option<ServerConfig> = None;
    let mut current_route: Option<RouteConfig> = None;
    let mut mode = ParseMode::General;

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = raw_line
            .chars()
            .take_while(|c| c.is_whitespace())
            .count();
        let (key, value) = split_kv(trimmed);

        match indent {
            0 => {
                if key == "max_server_size" {
                    config.max_server_size = value.parse().unwrap_or(10485760);
                }
                continue; // "servers:" is also at indent 0
            }
            2 => {
                // NEW SERVER START
                // Detects "- host: ..." or "- port: ..." or just a dash "- "
                if trimmed.starts_with("- ") {
                    if let Some(mut s) = current_server.take() {
                        if let Some(r) = current_route.take() {
                            s.routes.push(r);
                        }
                        config.servers.push(s);
                    }
                    current_server = Some(default_server());
                    current_route = None;
                    mode = ParseMode::General;

                    let line_after_dash = &trimmed[2..];
                    if !line_after_dash.is_empty() {
                        let (k, v) = split_kv(line_after_dash);
                        apply_server_field(current_server.as_mut().unwrap(), k, v);
                    }
                    continue;
                }

                // If we are at indent 2 and it's not a dash, it's a server field
                if let Some(ref mut server) = current_server {
                    mode = ParseMode::General; // reset mode if we come back to indent 2
                    apply_server_field(server, key, value);
                }
            }

            4 | 6 | 8 => {
                if let Some(ref mut server) = current_server {
                    if key == "error_pages" {
                        mode = ParseMode::ErrorPages;
                        continue;
                    }
                    if key == "routes" {
                        mode = ParseMode::Routes;
                        continue;
                    }

                    match mode {
                        ParseMode::ErrorPages => {
                            if let Ok(code) = key.parse::<u16>() {
                                server.error_pages.insert(code, value.to_string());
                            } else {
                                // If key is not a number, we likely exited the error_pages block
                                mode = ParseMode::General;
                                apply_server_field(server, key, value);
                            }
                        }
                        ParseMode::Routes => {
                            if
                                trimmed.starts_with("- path") ||
                                (indent == 4 && trimmed.starts_with("- "))
                            {
                                if let Some(r) = current_route.take() {
                                    server.routes.push(r);
                                }
                                current_route = Some(default_route());
                                let line_after_dash = if trimmed.starts_with("- ") {
                                    &trimmed[2..]
                                } else {
                                    trimmed
                                };
                                let (k, v) = split_kv(line_after_dash);
                                apply_route_field(current_route.as_mut().unwrap(), k, v);
                            } else if let Some(ref mut route) = current_route {
                                apply_route_field(route, key, value);
                            }
                        }
                        ParseMode::General => {
                            apply_server_field(server, key, value);
                        }
                    }
                }
            }

            _ => {}
        }
    }

    // Finalize
    if let Some(mut s) = current_server {
        if let Some(r) = current_route {
            s.routes.push(r);
        }
        config.servers.push(s);
    }

    Ok(config)
}

fn apply_server_field(server: &mut ServerConfig, key: &str, value: &str) {
    match key {
        "host" => {
            server.host = value.to_string();
        }
        "server_name" => {
            server.server_name = value.to_string();
        }
        "max_body_size" => {
            // Explicitly parse as usize
            server.max_body_size = value.parse::<usize>().unwrap_or(1048576);
        }
        "port" => {
            server.port = value.to_string();
        }
        _ => {}
    }
}
fn apply_route_field(route: &mut RouteConfig, key: &str, value: &str) {
    match key {
        "path" => {
            route.path = value.to_string();
        }
        "root" => {
            route.root = value.to_string();
        }
        "index" => {
            route.index = Some(value.to_string());
        }
        "autoindex" => {
            route.autoindex = value == "true";
        }
        "redirect" => {
            route.redirect = Some(value.to_string());
        }
        "cgi_extension" => {
            route.cgi_extension = Some(value.to_string());
        }
        "cgi_interpreter" => {
            route.cgi_interpreter = Some(value.to_string());
        }
        "methods" => {
            route.methods = parse_list(value);
        }
        _ => {}
    }
}
fn split_kv(line: &str) -> (&str, &str) {
    if let Some((k, v)) = line.split_once(':') {
        (k.trim(), v.trim().trim_matches('"').trim_matches('\''))
    } else {
        (line.trim(), "")
    }
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .trim_matches(|c| c == '[' || c == ']' || c == ' ')
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn default_server() -> ServerConfig {
    ServerConfig {
        host: "0.0.0.0".to_string(),
        port: String::new(),
        server_name: "localhost".to_string(),
        max_body_size: 1024 * 1024,
        error_pages: HashMap::new(),
        routes: Vec::new(),
    }
}

fn default_route() -> RouteConfig {
    RouteConfig {
        path: "/".to_string(),
        root: "./www".to_string(),
        methods: Vec::new(),
        index: None,
        autoindex: false,
        redirect: None,
        cgi_extension: None,
        cgi_interpreter: None,
    }
}