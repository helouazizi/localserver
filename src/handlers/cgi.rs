use std::collections::HashMap;
use std::io::Write;
use std::process::{ Child, Command, Stdio };

pub fn spawn_cgi_process(
    script_path: &str,
    interpreter: Option<&str>,
    body: &[u8],
    env_vars: HashMap<String, String>
) -> Result<Child, String> {
    let mut command = if let Some(interpreter_path) = interpreter {
        let mut cmd = Command::new(interpreter_path);
        cmd.arg(script_path);
        cmd
    } else {
        Command::new(script_path)
    };

    let mut child = command
        .envs(env_vars)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to execute CGI: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        if !body.is_empty() {
            stdin.write_all(body).map_err(|e| format!("CGI stdin write failed: {}", e))?;
        }
    }

    Ok(child)
}
