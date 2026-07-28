// Copyright (c) Modding Forge

use crate::MixTarget;
use serde::{Deserialize, Serialize};

/// Fixed name of the local WaveKeys pipe.
pub const PIPE_NAME: &str = r"\\.\pipe\wavekeys";
/// Reject requests large enough to be an IPC denial-of-service vector.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;
/// Current compatible IPC message revision.
pub const IPC_VERSION: u8 = 1;

/// A versioned request accepted by the local service.
#[derive(Debug, Deserialize, Serialize)]
pub struct IpcRequest {
    /// Protocol revision.
    pub version: u8,
    /// Requested operation.
    pub command: IpcCommand,
    /// Optional percentage for an adjustment.
    pub delta_percent: Option<f32>,
    /// Optional mix override.
    pub mix: Option<MixTarget>,
}

/// Commands safe to invoke from a local client.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcCommand {
    /// Adjust the currently focused application.
    AdjustCurrentApp,
    /// Fetch status without changing the mixer.
    GetStatus,
    /// Toggle mute for the configured or supplied mix.
    ToggleMute,
    /// Stop consuming media keys.
    PauseHook,
    /// Resume media key handling.
    ResumeHook,
}

/// Response returned for every syntactically valid IPC request.
#[derive(Debug, Deserialize, Serialize)]
pub struct IpcResponse {
    /// Whether the request was accepted for processing.
    pub ok: bool,
    /// Human-readable current state or error.
    pub message: String,
    /// Whether the hook currently consumes media keys.
    pub hook_active: bool,
}

impl IpcResponse {
    /// Builds a successful response.
    pub fn ok(message: impl Into<String>, hook_active: bool) -> Self {
        Self {
            ok: true,
            message: message.into(),
            hook_active,
        }
    }

    /// Builds a rejected response.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            hook_active: false,
        }
    }
}
