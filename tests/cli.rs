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
    for expected in ["analyze", "clean", "--preset", "--tasks", "--exclude", "Exit codes"] {
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
