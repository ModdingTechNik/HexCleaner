//! Командная строка HexCleaner (`HexCleanerCli.exe`): анализ и очистка без окна —
//! например, из планировщика Windows. Вся логика — в `hexcleaner::cli`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exit_code = hexcleaner::cli::run(&args);
    std::process::exit(exit_code as i32);
}
