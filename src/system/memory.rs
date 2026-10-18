//! Очистка оперативной памяти: сброс рабочих наборов процессов.
//!
//! Процессы не завершаются. Windows выгружает из их памяти страницы, которые
//! давно не использовались; при обращении они будут подгружены обратно.

use std::mem::size_of;

use windows::Win32::System::ProcessStatus::{
    K32EmptyWorkingSet, K32EnumProcesses, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA};

use super::OwnedHandle;

pub struct TrimResult {
    /// Сколько процессов обработано.
    pub trimmed: u32,
    /// Сколько процессов открыть не удалось (системные и защищённые).
    pub inaccessible: u32,
    /// На сколько байт уменьшились рабочие наборы процессов.
    pub freed_bytes: u64,
}

pub fn trim_all_working_sets() -> TrimResult {
    let mut result = TrimResult { trimmed: 0, inaccessible: 0, freed_bytes: 0 };

    for pid in process_ids() {
        let access = PROCESS_SET_QUOTA | PROCESS_QUERY_LIMITED_INFORMATION;
        let Ok(process) = (unsafe { OpenProcess(access, false, pid) }) else {
            result.inaccessible += 1;
            continue;
        };
        let process = OwnedHandle(process);

        let before = working_set_size(&process);
        if unsafe { K32EmptyWorkingSet(process.0) }.as_bool() {
            result.trimmed += 1;
            result.freed_bytes += before.saturating_sub(working_set_size(&process));
        } else {
            result.inaccessible += 1;
        }
    }
    result
}

/// Номера всех запущенных процессов.
pub(super) fn process_ids() -> Vec<u32> {
    let mut ids = vec![0u32; 4096];
    let mut bytes_returned = 0;
    let ok = unsafe {
        K32EnumProcesses(ids.as_mut_ptr(), (ids.len() * size_of::<u32>()) as u32, &mut bytes_returned)
    };
    if !ok.as_bool() {
        return Vec::new();
    }
    ids.truncate(bytes_returned as usize / size_of::<u32>());
    // PID 0 — псевдопроцесс «Бездействие системы».
    ids.retain(|&pid| pid != 0);
    ids
}

fn working_set_size(process: &OwnedHandle) -> u64 {
    let mut counters = PROCESS_MEMORY_COUNTERS::default();
    let ok = unsafe {
        K32GetProcessMemoryInfo(process.0, &mut counters, size_of::<PROCESS_MEMORY_COUNTERS>() as u32)
    };
    if ok.as_bool() { counters.WorkingSetSize as u64 } else { 0 }
}
