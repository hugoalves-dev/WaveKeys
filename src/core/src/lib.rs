// Copyright (c) Modding Forge

//! Domain types and transport primitives for WaveKeys.

pub mod command;
pub mod config;
pub mod controller;
pub mod ipc;
pub mod volume;
pub mod wavelink;

pub use command::AppCommand;
pub use config::{Config, ControlScope};
pub use controller::{Controller, ControllerStatus};
pub use volume::{MixTarget, VolumeBackend, VolumeError, VolumeTargetIcon, VolumeUpdate};
