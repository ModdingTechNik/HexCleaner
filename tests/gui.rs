//! Проверка параметров `HexCleaner.exe`: с параметрами окно не открывается.
//! Вывод перенаправлен, поэтому справка и ошибки печатаются, а не показываются в окне сообщения.

use std::process::{Command, Output};

fn gui(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_HexCleaner")).args(args).output().expect("HexCleaner должен запускаться")
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned() + &String::from_utf8_lossy(&output.stderr)
}

#[test]
fn help_describes_scheduled_mode() {
    for flag in ["--help", "-h", "/?"] {
        let output = gui(&[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert!(text(&output).contains("--scheduled"), "в справке нет --scheduled:\n{}", text(&output));
    }
}

#[test]
fn unknown_arguments_show_error_and_help() {
    for args in [&["--bogus"][..], &["clean"], &["--help", "extra"]] {
        let output = gui(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let text = text(&output);
        let unknown = args.iter().find(|arg| **arg != "--help").unwrap();
        assert!(text.contains(unknown) && text.contains("--scheduled"), "{args:?}:\n{text}");
    }
}

#[test]
fn scheduled_runs_cli_commands_under_its_own_name() {
    let output = gui(&["--scheduled", "help", "--lang", "en"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output).contains("HexCleaner --scheduled analyze"), "{}", text(&output));

    assert_eq!(gui(&["--scheduled", "wipe"]).status.code(), Some(1));
}
