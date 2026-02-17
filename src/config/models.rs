#[derive(Clone)]
pub struct RouteConfig {
    pub path: String,
    pub root: String,
    pub upload_dir: Option<String>,
    pub methods: Vec<String>,
    pub index: Option<String>,
    pub autoindex: bool,
    pub redirect: Option<String>,
    pub cgi_extension: Option<String>,
    pub cgi_interpreter: Option<String>,
}

pub struct ServerConfig {
    pub host: String,
    pub port: String,
    pub server_name: String,
    pub max_body_size: usize,
    pub error_pages: std::collections::HashMap<u16, String>,
    pub routes: Vec<RouteConfig>,
}

pub struct Config {
    pub servers: Vec<ServerConfig>,
    pub max_server_size: usize,
    pub timeout_seconds: u64,
}