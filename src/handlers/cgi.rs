use mio::unix::pipe::{ self, Receiver };
use std::collections::HashMap;
use std::io::Write;
use std::os::fd::{ FromRawFd, IntoRawFd, OwnedFd };
use std::process::{ Child, Command, Stdio };

pub fn spawn_cgi_process(
    script_path: &str,
    interpreter: Option<&str>,
    body: &[u8],
    env_vars: HashMap<String, String>
) -> Result<(Child, Receiver), String> {
    let mut command = if let Some(interpreter_path) = interpreter {
        let mut cmd = Command::new(interpreter_path);
        cmd.arg(script_path);
        cmd
    } else {
        Command::new(script_path)
    };

    let (sender, receiver) = pipe::new().map_err(|e| format!("Failed to create CGI pipe: {}", e))?;
    let sender_fd = sender.into_raw_fd();
    let sender_owned = unsafe { OwnedFd::from_raw_fd(sender_fd) };

    let mut child = command
        .envs(env_vars)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(sender_owned))
        .spawn()
        .map_err(|e| format!("Failed to execute CGI: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        if !body.is_empty() {
            stdin.write_all(body).map_err(|e| format!("CGI stdin write failed: {}", e))?;
        }
    }

    Ok((child, receiver))
}
