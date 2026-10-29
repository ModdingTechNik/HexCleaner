//! Проверка `HexCleanerCli.exe` как готовой программы: вывод и коды возврата.
//! Анализ и очистку здесь не запускаем — они пишут журналы в профиль пользователя.

use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_HexCleanerCli")).args(args).output().expect("HexCleanerCli должен запускаться")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn help_lists_commands_and_exit_codes() {
    let output = cli(&["help", "--lang", "en"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    for expected in ["analyze", "clean", "config", "trash restore", "--preset", "--tasks", "--exclude", "--no-trash", "Exit codes"] {
        assert!(text.contains(expected), "в справке нет «{expected}»:\n{text}");
    }
}

#[test]
fn list_shows_task_ids_and_built_in_presets() {
    let output = cli(&["list", "--lang", "en"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    for expected in ["user_temp", "recycle_bin", "safe", "recommended", "full"] {
        assert!(text.contains(expected), "в списке нет «{expected}»:\n{text}");
    }
}

#[test]
fn invalid_arguments_return_exit_code_1() {
    for args in [
        &["wipe"][..],
        &["clean", "--force"],
        &["clean", "--preset"],
        &["analyze", "--preset", "no-such-preset"],
        &["analyze", "--tasks", "no_such_task"],
        &["list", "--quiet"],
    ] {
        let output = cli(args);
        assert_eq!(output.status.code(), Some(1), "для {args:?} ожидался код 1");
        assert!(!output.stderr.is_empty(), "для {args:?} нет сообщения об ошибке");
    }
}

/// `config` меняет файл настроек. Настройки — во временной папке вместо `%APPDATA%`.
#[test]
fn config_sets_gets_and_resets_settings() {
    let app_data = std::env::temp_dir().join(format!("hexcleaner_cli_config_{}", std::process::id()));
    let config = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_HexCleanerCli"))
            .arg("config")
            .args(args)
            .args(["--lang", "en"])
            .env("APPDATA", &app_data)
            .output()
            .expect("HexCleanerCli должен запускаться")
    };

    let output = config(&["set", "trash.limit", "10GB"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(stdout(&config(&["get", "trash.limit"])).trim(), "10GB");
    let saved = std::fs::read_to_string(app_data.join(r"HexCleaner\settings.json")).unwrap();
    assert!(saved.contains("10737418240"), "{saved}");

    let output = config(&[]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    for expected in ["language", "trash.limit", "10GB", "trash.policy.<id>", "updates.check_on_start"] {
        assert!(text.contains(expected), "в настройках нет «{expected}»:\n{text}");
    }

    assert_eq!(config(&["reset", "trash.limit"]).status.code(), Some(0));
    assert_eq!(stdout(&config(&["get", "trash.limit"])).trim(), "5GB");

    for args in [&["set", "theme", "pink"][..], &["get", "no.such.setting"], &["set", "theme"], &["frobnicate"]] {
        let output = config(args);
        assert_eq!(output.status.code(), Some(1), "для config {args:?} ожидался код 1");
        assert!(!output.stderr.is_empty(), "для config {args:?} нет сообщения об ошибке");
    }

    let _ = std::fs::remove_dir_all(&app_data);
}

/// `trash` с пустой корзиной во временной папке вместо `%LOCALAPPDATA%`; удаление — только с `--yes`.
#[test]
fn trash_lists_and_requires_confirmation_to_delete() {
    let root = std::env::temp_dir().join(format!("hexcleaner_cli_trash_{}", std::process::id()));
    let trash = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_HexCleanerCli"))
            .arg("trash")
            .args(args)
            .args(["--lang", "en"])
            .env("APPDATA", root.join("roaming"))
            .env("LOCALAPPDATA", root.join("local"))
            .output()
            .expect("HexCleanerCli должен запускаться")
    };

    let output = trash(&[]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("Trash folder"), "{}", stdout(&output));

    for args in [&["empty"][..], &["show", "1"], &["restore", "latest"], &["delete", "1", "--yes"], &["show", "1", "--yes"]] {
        let output = trash(args);
        assert_eq!(output.status.code(), Some(1), "для trash {args:?} ожидался код 1");
        assert!(!output.stderr.is_empty(), "для trash {args:?} нет сообщения об ошибке");
    }
    assert_eq!(trash(&["empty", "--yes"]).status.code(), Some(0));

    let _ = std::fs::remove_dir_all(&root);
}
