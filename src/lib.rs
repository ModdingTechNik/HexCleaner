//! Ядро HexCleaner — общее для окна (`HexCleaner.exe`, `src/main.rs`)
//! и командной строки (`HexCleanerCli.exe`, `src/bin/cli.rs`).
//!
//! * [`catalog`] — пункты очистки и их пути;
//! * [`cleaner`] — движок: анализ и очистка в отдельном потоке, журнал, статистика;
//! * [`disk_usage`] — место на диске: дерево папок с размерами;
//! * [`presets`] — наборы галочек;
//! * [`schedule`] — плановая очистка: задания для Планировщика заданий Windows;
//! * [`settings`] — настройки, общие для окна и командной строки;
//! * [`update`] — обновление программы из релизов GitHub;
//! * [`i18n`] — переводы, макрос [`t!`];
//! * [`cli`] — команды и параметры командной строки;
//! * [`system`] — обёртки над Windows API, весь `unsafe`.

pub mod catalog;
pub mod cleaner;
pub mod cli;
pub mod disk_usage;
pub mod i18n;
pub mod presets;
pub mod schedule;
pub mod settings;
pub mod system;
pub mod update;
