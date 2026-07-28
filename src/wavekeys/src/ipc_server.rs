// Copyright (c) Modding Forge

use std::io;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
    sync::{mpsc, watch},
};
use wavekeys_core::{
    ipc::{IpcCommand, IpcRequest, IpcResponse, IPC_VERSION, MAX_MESSAGE_BYTES, PIPE_NAME},
    AppCommand, ControllerStatus,
};

/// Runs the local, newline-framed named-pipe command service.
pub async fn serve(
    commands: mpsc::Sender<AppCommand>,
    status: watch::Receiver<ControllerStatus>,
) -> Result<(), String> {
    loop {
        let server = ServerOptions::new()
            .first_pipe_instance(false)
            .reject_remote_clients(true)
            .create(PIPE_NAME)
            .map_err(|error| error.to_string())?;
        server.connect().await.map_err(|error| error.to_string())?;
        let commands = commands.clone();
        let status = status.clone();
        tokio::spawn(async move {
            let _ = handle(server, commands, status).await;
        });
    }
}

/// Sends one local request and returns the service response.
pub async fn request(request: IpcRequest) -> Result<IpcResponse, String> {
    let client = ClientOptions::new()
        .open(PIPE_NAME)
        .map_err(|error| format!("WaveKeys service is not running: {error}"))?;
    let mut stream = BufReader::new(client);
    let payload = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    if payload.len() > MAX_MESSAGE_BYTES {
        return Err("IPC request is too large".to_owned());
    }
    stream
        .get_mut()
        .write_all(&payload)
        .await
        .map_err(|error| error.to_string())?;
    stream
        .get_mut()
        .write_all(b"\n")
        .await
        .map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    stream
        .read_until(b'\n', &mut response)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&response).map_err(|error| format!("invalid IPC response: {error}"))
}

async fn handle(
    server: NamedPipeServer,
    commands: mpsc::Sender<AppCommand>,
    status: watch::Receiver<ControllerStatus>,
) -> io::Result<()> {
    let mut stream = BufReader::new(server);
    let mut data = Vec::new();
    let size = stream.read_until(b'\n', &mut data).await?;
    let response = if size == 0 || size > MAX_MESSAGE_BYTES {
        IpcResponse::error("IPC request exceeds size limit")
    } else {
        match serde_json::from_slice::<IpcRequest>(&data) {
            Err(_) => IpcResponse::error("invalid IPC request"),
            Ok(request) if request.version != IPC_VERSION => {
                IpcResponse::error("unsupported IPC version")
            }
            Ok(request) => dispatch(request, commands, status).await,
        }
    };
    let bytes = serde_json::to_vec(&response).expect("IPC response is serializable");
    stream.get_mut().write_all(&bytes).await?;
    stream.get_mut().write_all(b"\n").await
}

async fn dispatch(
    request: IpcRequest,
    commands: mpsc::Sender<AppCommand>,
    status: watch::Receiver<ControllerStatus>,
) -> IpcResponse {
    let command = match request.command {
        IpcCommand::GetStatus => {
            return IpcResponse::ok(status.borrow().backend.clone(), status.borrow().hook_active)
        }
        IpcCommand::AdjustCurrentApp => match request.delta_percent {
            Some(delta_percent)
                if delta_percent.is_finite() && (-100.0..=100.0).contains(&delta_percent) =>
            {
                AppCommand::Adjust {
                    delta_percent,
                    target: request.mix.unwrap_or(wavekeys_core::MixTarget::Monitor),
                }
            }
            _ => {
                return IpcResponse::error("delta_percent must be finite and between -100 and 100")
            }
        },
        IpcCommand::ToggleMute => AppCommand::ToggleMute,
        IpcCommand::PauseHook => AppCommand::SetHookPaused(true),
        IpcCommand::ResumeHook => AppCommand::SetHookPaused(false),
    };
    match commands.try_send(command) {
        Ok(()) => IpcResponse::ok("command accepted", status.borrow().hook_active),
        Err(_) => IpcResponse::error("WaveKeys command queue is full or closed"),
    }
}
