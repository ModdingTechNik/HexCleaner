//! Очистка журналов событий Windows (то же самое, что `wevtutil cl` для каждого журнала).

use windows::Win32::System::EventLog::{EVT_HANDLE, EvtClearLog, EvtClose, EvtNextChannelPath, EvtOpenChannelEnum};
use windows::core::{HSTRING, PCWSTR, Result};

/// Очищает все журналы событий. Для каждого журнала вызывает `on_channel`
/// с его именем и результатом очистки.
pub fn clear_all(mut on_channel: impl FnMut(&str, Result<()>)) -> Result<()> {
    for channel in channel_names()? {
        let result = unsafe { EvtClearLog(None, &HSTRING::from(&channel), PCWSTR::null(), 0) };
        on_channel(&channel, result);
    }
    Ok(())
}

/// Имена всех зарегистрированных журналов событий.
fn channel_names() -> Result<Vec<String>> {
    let channels = EventHandle(unsafe { EvtOpenChannelEnum(None, 0)? });
    let mut names = Vec::new();
    let mut buffer = vec![0u16; 512];

    loop {
        let mut used = 0;
        match unsafe { EvtNextChannelPath(channels.0, Some(&mut buffer), &mut used) } {
            Ok(()) => {
                // `used` включает завершающий нулевой символ.
                let length = (used as usize).saturating_sub(1);
                names.push(String::from_utf16_lossy(&buffer[..length]));
            }
            // Имя не поместилось в буфер — увеличиваем его и пробуем снова.
            Err(_) if used as usize > buffer.len() => buffer.resize(used as usize, 0),
            // ERROR_NO_MORE_ITEMS — журналы закончились.
            Err(_) => break,
        }
    }
    Ok(names)
}

struct EventHandle(EVT_HANDLE);

impl Drop for EventHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = EvtClose(self.0);
        }
    }
}
