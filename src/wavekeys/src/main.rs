// Copyright (c) Modding Forge

#[cfg(windows)]
mod icon;
#[cfg(windows)]
mod ipc_server;
#[cfg(windows)]
mod keyboard;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod tray;

use std::{process::ExitCode, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use wavekeys_core::{
    controller::parse_mix,
    ipc::{IpcCommand, IpcRequest},
    wavelink::{discover_endpoint, WaveLinkBackend, WaveLinkClient},
    Config, MixTarget,
};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if (args.is_empty() || args.first().is_some_and(|command| command == "serve"))
        && !ensure_single_instance()
    {
        return ExitCode::SUCCESS;
    }
    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("wavekeys: {error}");
            return ExitCode::FAILURE;
        }
    };
    let log_directory = Config::path()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("logs")));
    let appender = log_directory.and_then(|directory| {
        std::fs::create_dir_all(&directory)
            .ok()
            .map(|_| tracing_appender::rolling::daily(directory, "wavekeys.log"))
    });
    let (writer, guard) = appender
        .map(tracing_appender::non_blocking)
        .unwrap_or_else(|| tracing_appender::non_blocking(std::io::stderr()));
    std::mem::forget(guard);
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_new(&config.log_level).unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(writer)
        .with_target(false)
        .init();
    match run(config, args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "command failed");
            eprintln!("wavekeys: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn ensure_single_instance() -> bool {
    use windows::{
        core::w,
        Win32::{
            Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
            System::Threading::CreateMutexW,
        },
    };
    unsafe {
        let handle = CreateMutexW(None, true, w!("Local\\WaveKeys"));
        if handle.is_err() || GetLastError() == ERROR_ALREADY_EXISTS {
            return false;
        }
        std::mem::forget(handle);
        true
    }
}

#[cfg(not(windows))]
fn ensure_single_instance() -> bool {
    true
}

async fn run(config: Config, args: Vec<String>) -> Result<(), String> {
    // A normal Explorer launch has no command-line arguments: start the tray service.
    let command = args.first().map(String::as_str).unwrap_or("serve");
    match command {
        "init" => {
            let path = Config::create_default().map_err(|error| error.to_string())?;
            println!("configuration ready: {}", path.display());
        }
        "inspect" | "listen" => inspect(&config, command == "listen").await?,
        "serve" => serve(config).await?,
        "autostart" => configure_autostart(args.get(1).map(String::as_str))?,
        "status" => {
            send_ipc(IpcRequest {
                version: 1,
                command: IpcCommand::GetStatus,
                delta_percent: None,
                mix: None,
            })
            .await?
        }
        "mute" => {
            send_ipc(IpcRequest {
                version: 1,
                command: IpcCommand::ToggleMute,
                delta_percent: None,
                mix: None,
            })
            .await?
        }
        "pause" => {
            send_ipc(IpcRequest {
                version: 1,
                command: IpcCommand::PauseHook,
                delta_percent: None,
                mix: None,
            })
            .await?
        }
        "resume" => {
            send_ipc(IpcRequest {
                version: 1,
                command: IpcCommand::ResumeHook,
                delta_percent: None,
                mix: None,
            })
            .await?
        }
        "adjust" => {
            let delta = arg_value(&args, "--delta")
                .ok_or("adjust requires --delta <percent>")?
                .parse::<f32>()
                .map_err(|_| "--delta must be a number")?;
            let mix = arg_value(&args, "--mix").map(parse_mix).transpose()?;
            send_ipc(IpcRequest {
                version: 1,
                command: IpcCommand::AdjustCurrentApp,
                delta_percent: Some(delta),
                mix,
            })
            .await?;
        }
        "help" | "--help" | "-h" => print_help(),
        _ => return Err(format!("unknown command: {command}")),
    }
    Ok(())
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|value| value == flag)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn print_help() {
    println!("WaveKeys commands: init, serve, inspect, listen, status, adjust --delta N [--mix monitor|stream], mute, pause, resume, autostart enable|disable");
}

pub(crate) fn configure_autostart(action: Option<&str>) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let _ = action;
        Err("autostart is only available on Windows".to_owned())
    }
    #[cfg(windows)]
    {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let args: Vec<String> = match action {
            Some("enable") => vec![
                "add".into(),
                key.into(),
                "/v".into(),
                "WaveKeys".into(),
                "/t".into(),
                "REG_SZ".into(),
                "/d".into(),
                format!("\"{}\" serve", executable.display()),
                "/f".into(),
            ],
            Some("disable") => vec![
                "delete".into(),
                key.into(),
                "/v".into(),
                "WaveKeys".into(),
                "/f".into(),
            ],
            _ => return Err("usage: wavekeys autostart enable|disable".to_owned()),
        };
        let result = std::process::Command::new("reg")
            .args(args)
            .status()
            .map_err(|error| error.to_string())?;
        if result.success() {
            println!("autostart updated");
            Ok(())
        } else {
            Err("unable to update the per-user autostart entry".to_owned())
        }
    }
}

async fn inspect(config: &Config, listen: bool) -> Result<(), String> {
    let endpoint = if config.endpoint == "auto" {
        discover_endpoint()
            .await
            .map_err(|error| error.to_string())?
    } else {
        config.endpoint.clone()
    };
    let mut client =
        WaveLinkClient::connect(&endpoint, Duration::from_millis(config.rpc_timeout_ms))
            .await
            .map_err(|error| error.to_string())?;
    println!("connected to {endpoint}");
    println!(
        "channels: {}",
        client
            .call("getChannels", serde_json::Value::Null)
            .await
            .map_err(|error| error.to_string())?
    );
    println!(
        "mixes: {}",
        client
            .call("getMixes", serde_json::Value::Null)
            .await
            .map_err(|error| error.to_string())?
    );
    if listen {
        println!("listening; use Ctrl+C to stop");
        loop {
            tokio::select! {
                result = client.receive_notification() => println!("{:#}", result.map_err(|error| error.to_string())?),
                result = tokio::signal::ctrl_c() => { result.map_err(|error| error.to_string())?; break; }
            }
        }
    }
    Ok(())
}

async fn serve(config: Config) -> Result<(), String> {
    #[cfg(not(windows))]
    return Err("WaveKeys can only serve on Windows".to_owned());
    #[cfg(windows)]
    {
        let backend = WaveLinkBackend::new(
            config.endpoint.clone(),
            Duration::from_millis(config.rpc_timeout_ms),
        );
        let focus_backend = backend.clone();
        let reconnect_initial = Duration::from_millis(config.reconnect_initial_ms);
        let reconnect_max = Duration::from_millis(config.reconnect_max_ms);
        let (command_tx, command_rx) = mpsc::channel(64);
        let (overlay_tx, overlay_rx) = overlay::volume_channel();
        let (overlay_command_tx, overlay_command_rx) = overlay_rx.command_channel();
        let (controller, status_rx) = wavekeys_core::Controller::new(backend, config);
        let controller = controller.with_volume_updates(move |update| overlay_tx.send(update));
        let active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut hook_status = status_rx.clone();
        let hook_active = active.clone();
        tokio::spawn(async move {
            while hook_status.changed().await.is_ok() {
                hook_active.store(
                    hook_status.borrow().hook_active,
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
        });
        keyboard::start(command_tx.clone(), active.clone())?;
        tokio::spawn(controller.run(command_rx));
        tokio::spawn(focus_backend.track_focus(reconnect_initial, reconnect_max));
        tokio::spawn(async move {
            if let Err(error) = ipc_server::serve(command_tx, status_rx).await {
                tracing::error!(%error, "IPC server stopped");
            }
        });
        info!("WaveKeys service started");
        std::thread::Builder::new()
            .name("wavekeys-tray".to_owned())
            .spawn(move || {
                if let Err(error) = tray::run(overlay_command_tx) {
                    tracing::error!(%error, "tray service stopped");
                }
            })
            .map_err(|error| error.to_string())?;
        overlay::run(overlay_rx, overlay_command_rx)
    }
}

async fn send_ipc(request: IpcRequest) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        let _ = request;
        return Err("WaveKeys IPC is only available on Windows".to_owned());
    }
    #[cfg(windows)]
    {
        let response = ipc_server::request(request).await?;
        println!("{}", response.message);
        if response.ok {
            Ok(())
        } else {
            Err(response.message)
        }
    }
}

#[allow(dead_code)]
fn _mix_documentation(_: MixTarget) {}
