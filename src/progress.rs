//! Прогресс очистки по итогам предыдущего анализа: анализ уже посчитал, сколько файлов
//! в каждом пункте, поэтому во время очистки видно, какая часть работы сделана.

use hexcleaner::cleaner::TaskReport;

/// Где сейчас пункт очистки.
#[derive(Clone, Copy, Debug)]
pub enum TaskProgress {
    NotStarted,
    /// Выполняется: сколько уже удалено и сколько не удалось удалить.
    Running(TaskReport),
    /// Закончен (или пропущен): засчитывается целиком.
    Finished(TaskReport),
}

/// Ход всей очистки.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CleanupProgress {
    /// Доля сделанной работы, от 0 до 1.
    pub fraction: f32,
    pub deleted_files: u64,
    /// Сколько файлов нашёл анализ.
    pub expected_files: u64,
    pub freed_bytes: u64,
    /// Сколько места обещал анализ.
    pub expected_bytes: u64,
}

/// Прогресс по пунктам, для которых есть результат анализа (`expected`).
/// `None`, если ни для одного пункта очистки анализа нет.
///
/// Каждый пункт весит столько, сколько файлов в нём нашёл анализ (но не меньше одного,
/// чтобы учитывались и пункты без файлов — например, журналы событий). Законченный пункт
/// засчитывается целиком, даже если часть файлов оказалась занята или исчезла после анализа,
/// поэтому полоса прогресса доходит до конца и не застревает. У выполняющегося пункта
/// засчитываются удалённые и пропущенные файлы, но не больше найденных анализом.
pub fn cleanup_progress(tasks: impl IntoIterator<Item = (TaskProgress, Option<TaskReport>)>) -> Option<CleanupProgress> {
    let (mut done_weight, mut total_weight) = (0u64, 0u64);
    let (mut deleted_files, mut expected_files, mut freed_bytes, mut expected_bytes) = (0, 0, 0, 0);

    for (progress, expected) in tasks {
        let Some(expected) = expected else { continue };
        let weight = expected.deleted_files.max(1);
        total_weight += weight;
        expected_files += expected.deleted_files;
        expected_bytes += expected.freed_bytes;

        match progress {
            TaskProgress::NotStarted => {}
            TaskProgress::Running(report) => {
                done_weight += (report.deleted_files + report.failed).min(weight);
                deleted_files += report.deleted_files;
                freed_bytes += report.freed_bytes;
            }
            TaskProgress::Finished(report) => {
                done_weight += weight;
                deleted_files += report.deleted_files;
                freed_bytes += report.freed_bytes;
            }
        }
    }

    (total_weight > 0).then(|| CleanupProgress {
        fraction: done_weight as f32 / total_weight as f32,
        deleted_files,
        expected_files,
        freed_bytes,
        expected_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(files: u64, bytes: u64, failed: u64) -> TaskReport {
        TaskReport { deleted_files: files, freed_bytes: bytes, failed, freed_memory: 0 }
    }

    #[test]
    fn no_analysis_means_no_progress() {
        let tasks = [(TaskProgress::Running(report(5, 50, 0)), None)];
        assert_eq!(cleanup_progress(tasks), None);
    }

    #[test]
    fn counts_running_and_finished_tasks() {
        let tasks = [
            (TaskProgress::Finished(report(90, 900, 10)), Some(report(100, 1000, 0))),
            (TaskProgress::Running(report(40, 400, 10)), Some(report(200, 2000, 0))),
            (TaskProgress::NotStarted, Some(report(100, 1000, 0))),
            // Пункт без анализа (отмечен после него) в прогресс не входит.
            (TaskProgress::Running(report(7, 70, 0)), None),
        ];
        let progress = cleanup_progress(tasks).unwrap();

        // Готово: 100 (первый пункт целиком) + 50 (удалено и пропущено во втором) из 400.
        assert_eq!(progress.fraction, 150.0 / 400.0);
        assert_eq!((progress.deleted_files, progress.expected_files), (130, 400));
        assert_eq!((progress.freed_bytes, progress.expected_bytes), (1300, 4000));
    }

    #[test]
    fn never_goes_past_the_end() {
        // После анализа появились новые файлы — удалено больше, чем нашёл анализ.
        let tasks = [(TaskProgress::Running(report(500, 5000, 0)), Some(report(100, 1000, 0)))];
        assert_eq!(cleanup_progress(tasks).unwrap().fraction, 1.0);
    }

    #[test]
    fn tasks_without_files_still_count() {
        // Журналы событий: анализ даёт только объём, без файлов.
        let tasks = [
            (TaskProgress::Finished(report(0, 5000, 0)), Some(report(0, 6000, 0))),
            (TaskProgress::NotStarted, Some(report(0, 1000, 0))),
        ];
        assert_eq!(cleanup_progress(tasks).unwrap().fraction, 0.5);
    }
}
