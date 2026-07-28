// Copyright (c) Modding Forge

use crate::MixTarget;

/// A serialized action requested by the hook, IPC, or command line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AppCommand {
    /// Increase the selected mix by the configured step.
    VolumeUp,
    /// Decrease the selected mix by the configured step.
    VolumeDown,
    /// Toggle mute for the selected mix.
    ToggleMute,
    /// Change hook processing without terminating the process.
    SetHookPaused(bool),
    /// Adjust the focused application by an explicit percentage.
    Adjust {
        delta_percent: f32,
        target: MixTarget,
    },
    /// End the controller loop.
    Shutdown,
}
