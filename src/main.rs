mod config;
mod server;
mod http;
mod handlers;

use crate::server::Server;


fn main() {
    // debug();

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

fn debug() {
    let cfg = match config::parse_config("config.yaml") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Fatal Config Error: {}", e);
            return;
        }
    };

    println!("========================================");
    println!("      LOCALHOST SERVER CONFIG           ");
    println!("========================================");
    println!("Global Max Server Size: {} bytes", cfg.max_server_size);
    println!("----------------------------------------");

    for (i, server) in cfg.servers.iter().enumerate() {
        println!("SERVER [{}]", i + 1);
        println!("  Name:          {}", server.server_name);
        println!("  Host:Port:     {}:{}", server.host, server.port);
        println!("  Max Body Size: {} bytes", server.max_body_size);

        println!("  Error Pages:");
        if server.error_pages.is_empty() {
            println!("    (None)");
        } else {
            for (code, path) in &server.error_pages {
                println!("    {} -> {}", code, path);
            }
        }

        println!("  Routes:");
        for route in &server.routes {
            println!("    - Path:      {}", route.path);
            println!("      Root:      {}", route.root);
            println!("      Methods:   {:?}", route.methods);

            if let Some(index) = &route.index {
                println!("      Index:     {}", index);
            }


            if let Some(redir) = &route.redirect {
                println!("      Redirect:  {}", redir);
            }

            if let Some(cgi_ext) = &route.cgi_extension {
                let interpreter = route.cgi_interpreter.as_deref().unwrap_or("None");
                println!("      CGI:       {} (via {})", cgi_ext, interpreter);
            }
            println!("");
        }
        println!("----------------------------------------");
    }
}
