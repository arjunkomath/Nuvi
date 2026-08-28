use std::{ffi::OsString, future::Future, path::PathBuf, process::Stdio};

use async_channel::Sender;
use async_process::{Child, ChildStdin, Command};
use async_std::io::ReadExt as _;
use async_trait::async_trait;
use nvim_rs::{Handler, Neovim, UiAttachOptions, Value};

pub enum NvimEvent {
    Redraw(Vec<Value>),
    Error(String),
    CloseCancelled,
    Closed,
}

#[derive(Clone)]
pub struct NvimClient {
    nvim: Neovim<ChildStdin>,
    events: Sender<NvimEvent>,
}

impl NvimClient {
    pub fn input(&self, keys: String) {
        self.rpc("input", move |nvim| async move {
            nvim.input(&keys).await.map(|_| ())
        });
    }

    pub fn resize(&self, width: usize, height: usize) {
        self.rpc("resize", move |nvim| async move {
            nvim.ui_try_resize(width as i64, height as i64).await
        });
    }

    pub fn focus(&self, gained: bool) {
        self.rpc("focus", move |nvim| async move {
            nvim.ui_set_focus(gained).await
        });
    }

    pub fn mouse(
        &self,
        button: &'static str,
        action: &'static str,
        modifiers: String,
        row: usize,
        col: usize,
    ) {
        self.rpc("mouse", move |nvim| async move {
            nvim.input_mouse(button, action, &modifiers, 0, row as i64, col as i64)
                .await
        });
    }

    pub fn confirm_quit(&self) {
        let nvim = self.nvim.clone();
        let events = self.events.clone();
        async_std::task::spawn(async move {
            match nvim.command("confirm qa").await {
                Ok(()) => {
                    async_std::task::sleep(std::time::Duration::from_millis(50)).await;
                    let _ = events.send(NvimEvent::CloseCancelled).await;
                }
                Err(error) => {
                    let _ = events
                        .send(NvimEvent::Error(format!("Neovim quit failed: {error}")))
                        .await;
                }
            }
        });
    }

    fn rpc<F, Fut, T, E>(&self, operation: &'static str, call: F)
    where
        F: FnOnce(Neovim<ChildStdin>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        let nvim = self.nvim.clone();
        let events = self.events.clone();
        async_std::task::spawn(async move {
            if let Err(error) = call(nvim).await {
                let _ = events
                    .send(NvimEvent::Error(format!(
                        "Neovim {operation} failed: {error}"
                    )))
                    .await;
            }
        });
    }
}

pub struct NvimSession {
    pub client: NvimClient,
    _child: Child,
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
        let closed = events.clone();
        async_std::task::spawn(async move {
            if let Err(error) = io.await
                && !error.is_channel_closed()
            {
                let _ = closed
                    .send(NvimEvent::Error(format!(
                        "Neovim connection closed: {error}"
                    )))
                    .await;
            }
            let _ = closed.send(NvimEvent::Closed).await;
        });

        nvim.set_client_info(
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
        .map_err(|error| format!("Could not identify Nuvi to Neovim: {error}"))?;

        let mut options = UiAttachOptions::new();
        options
            .set_rgb(true)
            .set_linegrid_external(true)
            .set_term_name("nuvi");
        nvim.ui_attach(80, 24, &options)
            .await
            .map_err(|error| format!("Could not attach the Nuvi UI: {error}"))?;

        Ok(Self {
            client: NvimClient { nvim, events },
            _child: child,
        })
    }
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
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    ["/opt/homebrew/bin/nvim", "/usr/local/bin/nvim"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_std::future::timeout;

    use super::*;

    #[async_std::test]
    async fn embedded_session_receives_redraw_and_closes() {
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
                    NvimEvent::Closed => panic!("Neovim closed before its first redraw"),
                    _ => {}
                }
            }
        })
        .await
        .expect("Neovim did not redraw");

        session.client.input("<Esc>:qa!<CR>".into());
        timeout(Duration::from_secs(5), async {
            while !matches!(receiver.recv().await, Ok(NvimEvent::Closed)) {}
        })
        .await
        .expect("Neovim did not close");
    }
}
