use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use async_channel::Sender;
use async_process::{Child, ChildStdin, Command};
use async_std::io::ReadExt as _;
use async_trait::async_trait;
use nvim_rs::{Handler, Neovim, UiAttachOptions, Value};

pub enum NvimEvent {
    Redraw(Vec<Value>),
    Error(String),
    CloseCancelled,
    Exited(Option<String>),
}

enum Request {
    Input(String),
    Paste(String),
    Resize(usize, usize),
    Focus(bool),
    Mouse {
        button: &'static str,
        action: &'static str,
        modifiers: String,
        row: usize,
        col: usize,
    },
}

#[derive(Clone)]
pub struct NvimClient {
    nvim: Neovim<ChildStdin>,
    requests: Sender<Request>,
    events: Sender<NvimEvent>,
}

impl NvimClient {
    pub fn input(&self, keys: String) {
        self.request(Request::Input(keys));
    }

    pub fn paste(&self, text: String) {
        self.request(Request::Paste(text));
    }

    pub fn resize(&self, width: usize, height: usize) {
        self.request(Request::Resize(width, height));
    }

    pub fn focus(&self, gained: bool) {
        self.request(Request::Focus(gained));
    }

    pub fn mouse(
        &self,
        button: &'static str,
        action: &'static str,
        modifiers: String,
        row: usize,
        col: usize,
    ) {
        self.request(Request::Mouse {
            button,
            action,
            modifiers,
            row,
            col,
        });
    }

    pub fn confirm_quit(&self) {
        // This bypasses the request queue: the command blocks inside Neovim until the
        // user answers the confirmation dialog, and their keystrokes must keep flowing.
        let nvim = self.nvim.clone();
        let events = self.events.clone();
        async_std::task::spawn(async move {
            // When the user confirms, Neovim exits without answering the request; when
            // they cancel, the command completes. A follow-up request served by Neovim's
            // main loop tells the two apart deterministically.
            let quit = nvim.command("confirm qa").await;
            if nvim.eval("1").await.is_ok() {
                if let Err(error) = quit {
                    let _ = events
                        .send(NvimEvent::Error(format!("Neovim quit failed: {error}")))
                        .await;
                }
                let _ = events.send(NvimEvent::CloseCancelled).await;
            }
        });
    }

    fn request(&self, request: Request) {
        let _ = self.requests.try_send(request);
    }
}

pub struct NvimSession {
    pub client: NvimClient,
    _process: NvimProcess,
}

struct NvimProcess(Child);

impl Drop for NvimProcess {
    fn drop(&mut self) {
        // A session is normally dropped after Neovim exits gracefully. If its
        // owner disappears first (for example, Nuvi crashes), do not orphan it.
        let _ = self.0.kill();
    }
}

impl NvimSession {
    pub async fn spawn(
        args: Vec<OsString>,
        working_directory: Option<PathBuf>,
        events: Sender<NvimEvent>,
    ) -> Result<Self, String> {
        let executable = find_nvim().ok_or_else(|| {
            "Could not find Neovim. Install nvim or set NUVI_NVIM to its full path.".to_string()
        })?;
        let mut command = Command::new(executable);
        if let Some(directory) = working_directory {
            command.current_dir(directory);
        }
        command
            .env("NUVI", "1")
            .arg("--cmd")
            .arg("let g:nuvi = v:true")
            .arg("--embed")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| format!("Could not start Neovim: {error}"))?;
        let stdin = child.stdin.take().ok_or("Neovim stdin was not available")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Neovim stdout was not available")?;

        if let Some(mut stderr) = child.stderr.take() {
            let errors = events.clone();
            async_std::task::spawn(async move {
                let mut message = String::new();
                if stderr.read_to_string(&mut message).await.is_ok() && !message.trim().is_empty() {
                    let _ = errors.send(NvimEvent::Error(message.trim().into())).await;
                }
            });
        }

        let handler = NvimHandler {
            events: events.clone(),
        };
        let (nvim, io) = Neovim::new(stdout, stdin, handler);
        let connection_errors = events.clone();
        async_std::task::spawn(async move {
            if let Err(error) = io.await
                && !error.is_channel_closed()
            {
                let _ = connection_errors
                    .send(NvimEvent::Error(format!(
                        "Neovim connection closed: {error}"
                    )))
                    .await;
            }
        });

        if let Err(error) = nvim
            .set_client_info(
                "nuvi",
                vec![
                    ("major".into(), 0.into()),
                    ("minor".into(), 1.into()),
                    ("patch".into(), 0.into()),
                ],
                "ui",
                Vec::new(),
                Vec::new(),
            )
            .await
        {
            close_failed_start(&mut child, &nvim).await;
            return Err(format!("Could not identify Nuvi to Neovim: {error}"));
        }

        let mut options = UiAttachOptions::new();
        options
            .set_rgb(true)
            .set_linegrid_external(true)
            .set_term_name("nuvi");
        if let Err(error) = nvim.ui_attach(80, 24, &options).await {
            close_failed_start(&mut child, &nvim).await;
            return Err(format!("Could not attach the Nuvi UI: {error}"));
        }

        // A single worker delivers requests in call order; spawning a task per call
        // would let back-to-back keystrokes reach Neovim transposed.
        let (requests, request_queue) = async_channel::unbounded();
        let request_nvim = nvim.clone();
        let request_events = events.clone();
        async_std::task::spawn(async move {
            while let Ok(request) = request_queue.recv().await {
                let (operation, result) = match request {
                    Request::Input(keys) => ("input", request_nvim.input(&keys).await.map(|_| ())),
                    Request::Paste(text) => (
                        "paste",
                        request_nvim.paste(&text, true, -1).await.map(|_| ()),
                    ),
                    Request::Resize(width, height) => (
                        "resize",
                        request_nvim
                            .ui_try_resize(width as i64, height as i64)
                            .await,
                    ),
                    Request::Focus(gained) => ("focus", request_nvim.ui_set_focus(gained).await),
                    Request::Mouse {
                        button,
                        action,
                        modifiers,
                        row,
                        col,
                    } => (
                        "mouse",
                        request_nvim
                            .input_mouse(button, action, &modifiers, 0, row as i64, col as i64)
                            .await,
                    ),
                };
                if let Err(error) = result {
                    let _ = request_events
                        .send(NvimEvent::Error(format!(
                            "Neovim {operation} failed: {error}"
                        )))
                        .await;
                }
            }
        });

        let status = child.status();
        let exited = events.clone();
        async_std::task::spawn(async move {
            let error = match status.await {
                Ok(status) if status.success() => None,
                Ok(status) => Some(format!("Neovim exited unexpectedly ({status}).")),
                Err(error) => Some(format!("Could not read Neovim's exit status: {error}")),
            };
            let _ = exited.send(NvimEvent::Exited(error)).await;
        });
        let process = NvimProcess(child);

        Ok(Self {
            client: NvimClient {
                nvim,
                requests,
                events,
            },
            _process: process,
        })
    }
}

async fn close_failed_start(child: &mut Child, nvim: &Neovim<ChildStdin>) {
    let _ = nvim.input("<Esc>:qa!<CR>").await;
    let _ = async_std::future::timeout(Duration::from_secs(2), child.status()).await;
}

#[derive(Clone)]
struct NvimHandler {
    events: Sender<NvimEvent>,
}

#[async_trait]
impl Handler for NvimHandler {
    type Writer = ChildStdin;

    async fn handle_notify(&self, name: String, args: Vec<Value>, _neovim: Neovim<Self::Writer>) {
        match name.as_str() {
            "redraw" => {
                let _ = self.events.send(NvimEvent::Redraw(args)).await;
            }
            "nvim_error_event" => {
                let _ = self
                    .events
                    .send(NvimEvent::Error(format!("Neovim error: {args:?}")))
                    .await;
            }
            _ => {}
        }
    }
}

fn find_nvim() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("NUVI_NVIM") {
        return Some(path.into());
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            let candidate = directory.join("nvim");
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    ["/opt/homebrew/bin/nvim", "/usr/local/bin/nvim"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_std::future::timeout;

    use super::*;

    #[async_std::test]
    async fn embedded_session_receives_redraw_and_closes_gracefully() {
        if find_nvim().is_none() {
            return;
        }

        let (sender, receiver) = async_channel::bounded(256);
        let session = timeout(
            Duration::from_secs(5),
            NvimSession::spawn(vec!["--clean".into()], None, sender),
        )
        .await
        .expect("Neovim startup timed out")
        .expect("Neovim failed to start");

        timeout(Duration::from_secs(5), async {
            loop {
                match receiver.recv().await.expect("Neovim event channel closed") {
                    NvimEvent::Redraw(events)
                        if events.iter().any(|event| {
                            event
                                .as_array()
                                .and_then(|event| event.first())
                                .and_then(Value::as_str)
                                == Some("flush")
                        }) =>
                    {
                        break;
                    }
                    NvimEvent::Exited(_) => panic!("Neovim closed before its first redraw"),
                    _ => {}
                }
            }
        })
        .await
        .expect("Neovim did not redraw");

        let expected = Value::Array(vec!["first".into(), "second <tag>".into()]);
        session.client.paste("first\nsecond <tag>".into());
        timeout(Duration::from_secs(5), async {
            loop {
                if session.client.nvim.eval("getline(1, '$')").await.ok() == Some(expected.clone())
                {
                    break;
                }
                async_std::task::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Neovim did not receive pasted text");

        session
            .client
            .nvim
            .command("set nomodified")
            .await
            .expect("Neovim did not reset the test buffer");
        session.client.confirm_quit();
        timeout(Duration::from_secs(5), async {
            while !matches!(receiver.recv().await, Ok(NvimEvent::Exited(None))) {}
        })
        .await
        .expect("Neovim did not close");
    }
}
