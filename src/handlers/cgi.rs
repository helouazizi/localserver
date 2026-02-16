use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

pub fn execute_cgi(
    script_path: &str,
    interpreter: Option<&str>,
    body: &[u8],
    env_vars: HashMap<String, String>
) -> Result<Vec<u8>, String> {
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

    if !body.is_empty() {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(body).map_err(|e| format!("CGI stdin write failed: {}", e))?;
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("CGI failed: {}", e))?;

    Ok(output.stdout)
}