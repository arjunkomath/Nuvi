use std::{
    env,
    ffi::{CStr, OsStr, OsString},
    fs,
    io::{self, IsTerminal as _, Read as _},
    mem::MaybeUninit,
    os::unix::{ffi::OsStringExt as _, process::CommandExt as _},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    ptr,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const DEFAULT_SHELL: &str = "/bin/zsh";
const DEFAULT_PASSWD_BUFFER_SIZE: usize = 16 * 1024;
const MAX_PASSWD_BUFFER_SIZE: usize = 1024 * 1024;
const SHELL_CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
const SHELL_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SHELLS_FILE: &str = "/etc/shells";

pub fn startup_path() -> io::Result<Option<OsString>> {
    let selected = selected_shell();
    if selected.is_none()
        && (std::io::stdin().is_terminal()
            || std::io::stdout().is_terminal()
            || std::io::stderr().is_terminal())
    {
        return Ok(None);
    }

    capture_path(selected.as_deref().unwrap_or(&automatic_shell())).map(Some)
}

pub fn automatic_shell() -> PathBuf {
    account_shell()
        .or_else(|| env::var_os("SHELL").map(PathBuf::from))
        .filter(|shell| is_executable(shell))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SHELL))
}

pub fn available_shells() -> Vec<PathBuf> {
    let mut shells = fs::read_to_string(SHELLS_FILE)
        .map(|contents| {
            contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(PathBuf::from)
                .filter(|shell| is_executable(shell))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let automatic = automatic_shell();
    if is_executable(&automatic) && !shells.contains(&automatic) {
        shells.push(automatic);
    }
    shells
}

pub fn selected_shell() -> Option<PathBuf> {
    let shell = fs::read_to_string(shell_file()?).ok()?;
    let shell = PathBuf::from(shell.trim());
    is_executable(&shell).then_some(shell)
}

pub fn save_selected_shell(shell: Option<&Path>) -> io::Result<()> {
    let Some(file) = shell_file() else {
        return Ok(());
    };
    let Some(shell) = shell else {
        return match fs::remove_file(file) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
    };
    if !is_executable(shell) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the selected shell is not executable",
        ));
    }
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(file, shell.to_string_lossy().as_bytes())
}

fn capture_path(shell: &Path) -> io::Result<OsString> {
    let name = shell.file_name().unwrap_or_else(|| OsStr::new("sh"));
    let mut arg0 = OsString::from("-");
    arg0.push(name);
    let mut command = Command::new(shell);
    command
        .arg0(arg0)
        .args(["-i", "-c", "/usr/bin/printf '\\0'; exec /usr/bin/env -0"])
        .stdin(Stdio::null());
    if let Some(home) = env::var_os("HOME") {
        command.current_dir(home);
    }
    let (status, output) = output_with_timeout(&mut command, SHELL_CAPTURE_TIMEOUT)?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "{} exited with {}",
            shell.display(),
            status
        )));
    }
    path_from_env_output(&output)
        .ok_or_else(|| io::Error::other(format!("{} did not report PATH", shell.display())))
}

fn output_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> io::Result<(ExitStatus, Vec<u8>)> {
    let mut child = command
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let Some(mut stdout) = child.stdout.take() else {
        terminate_process_group(&mut child);
        return Err(io::Error::other("shell stdout was not available"));
    };
    let (output_sender, output_receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout.read_to_end(&mut output).map(|_| output);
        let _ = output_sender.send(result);
    });

    let deadline = Instant::now() + timeout;
    let mut status = None;
    let mut output = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit_status)) => {
                    status = Some(exit_status);
                    kill_process_group(child.id());
                }
                Ok(None) => {}
                Err(error) => {
                    terminate_process_group(&mut child);
                    return Err(error);
                }
            }
        }
        if output.is_none() {
            match output_receiver.try_recv() {
                Ok(Ok(bytes)) => output = Some(bytes),
                Ok(Err(error)) => {
                    terminate_process_group(&mut child);
                    return Err(error);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    terminate_process_group(&mut child);
                    return Err(io::Error::other("shell output reader stopped unexpectedly"));
                }
            }
        }
        if let Some(status) = status
            && let Some(output) = output.take()
        {
            return Ok((status, output));
        }
        let now = Instant::now();
        if now >= deadline {
            terminate_process_group(&mut child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "shell environment capture timed out",
            ));
        }
        thread::sleep(SHELL_POLL_INTERVAL.min(deadline - now));
    }
}

fn terminate_process_group(child: &mut Child) {
    kill_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

fn kill_process_group(id: u32) {
    // SAFETY: capture children are placed in a process group whose ID is their PID.
    unsafe {
        libc::killpg(id as libc::pid_t, libc::SIGKILL);
    }
}

fn path_from_env_output(output: &[u8]) -> Option<OsString> {
    output
        .split(|byte| *byte == 0)
        .filter_map(|variable| variable.strip_prefix(b"PATH="))
        .next_back()
        .filter(|path| !path.is_empty())
        .map(|path| OsString::from_vec(path.to_vec()))
}

fn shell_file() -> Option<PathBuf> {
    env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library/Application Support/Nuvi")
            .join("shell")
    })
}

fn account_shell() -> Option<PathBuf> {
    // SAFETY: getpwuid_r writes to the provided passwd value and buffer. The
    // returned shell pointer is read only while both remain alive.
    unsafe {
        let configured_size = libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX);
        let initial_size = if configured_size > 0 {
            configured_size as usize
        } else {
            DEFAULT_PASSWD_BUFFER_SIZE
        }
        .clamp(DEFAULT_PASSWD_BUFFER_SIZE, MAX_PASSWD_BUFFER_SIZE);
        let mut buffer = vec![0_u8; initial_size];
        loop {
            let mut passwd = MaybeUninit::<libc::passwd>::uninit();
            let mut result = ptr::null_mut();
            let error = libc::getpwuid_r(
                libc::getuid(),
                passwd.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            );
            if error == libc::ERANGE && buffer.len() < MAX_PASSWD_BUFFER_SIZE {
                buffer.resize((buffer.len() * 2).min(MAX_PASSWD_BUFFER_SIZE), 0);
                continue;
            }
            if error != 0 || result.is_null() {
                return None;
            }
            let passwd = passwd.assume_init();
            if passwd.pw_shell.is_null() {
                return None;
            }
            let shell = PathBuf::from(OsString::from_vec(
                CStr::from_ptr(passwd.pw_shell).to_bytes().to_vec(),
            ));
            return is_executable(&shell).then_some(shell);
        }
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.is_absolute()
        && path
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_path_after_noisy_shell_startup() {
        let output = b"startup message\0USER=test\0PATH=/one:/two\0SHELL=/bin/zsh\0";
        assert_eq!(
            path_from_env_output(output),
            Some(OsString::from("/one:/two"))
        );
    }

    #[test]
    fn times_out_and_reaps_shell_process_group() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 10"]);
        let error = output_with_timeout(&mut command, Duration::from_millis(20)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
