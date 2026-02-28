mod config;
mod server;
mod http;
mod handlers;

use crate::server::Server;

fn main() {
    let cfg = match config::parse_config("config.yaml") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Fatal Config Error: {}", e);
            return;
        }
    };

    let mut server = Server::new(cfg);

    if let Err(e) = server.bind() {
        eprintln!("[Fatal] {}", e);
        return;
    }

    server.run();
}
