// Copyright (c) Modding Forge

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    thread,
};
use tokio::sync::mpsc;
use wavekeys_core::AppCommand;
use windows::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP},
        WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
            KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
        },
    },
};

struct HookContext {
    sender: mpsc::Sender<AppCommand>,
    active: Arc<AtomicBool>,
}
static CONTEXT: OnceLock<HookContext> = OnceLock::new();

/// Starts the low-level hook on its own Windows message-loop thread.
pub fn start(sender: mpsc::Sender<AppCommand>, active: Arc<AtomicBool>) -> Result<(), String> {
    CONTEXT
        .set(HookContext {
            sender,
            active: active.clone(),
        })
        .map_err(|_| "keyboard hook already started".to_owned())?;
    thread::spawn(|| unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(callback), None, 0)
            .expect("install keyboard hook");
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let _ = windows::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(hook);
    });
    Ok(())
}

unsafe extern "system" fn callback(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN) {
        if let Some(context) = CONTEXT.get() {
            let key = (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode;
            let command = if key == VK_VOLUME_UP.0 as u32 {
                Some(AppCommand::VolumeUp)
            } else if key == VK_VOLUME_DOWN.0 as u32 {
                Some(AppCommand::VolumeDown)
            } else if key == VK_VOLUME_MUTE.0 as u32 {
                Some(AppCommand::ToggleMute)
            } else {
                None
            };
            if let Some(command) = command {
                if context.active.load(Ordering::Relaxed) {
                    let _ = context.sender.try_send(command);
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}
