//! Вшивает в программы иконку и сведения о файле (assets/*.rc).
//! Иконка собирается из SVG командой `cargo run --example make_icon`.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = |part: &str| std::env::var(format!("CARGO_PKG_VERSION_{part}")).unwrap();
    let macros = [
        format!("VERSION_MAJOR={}", version("MAJOR")),
        format!("VERSION_MINOR={}", version("MINOR")),
        format!("VERSION_PATCH={}", version("PATCH")),
    ];

    embed_resource::compile_for("assets/gui.rc", ["HexCleaner"], &macros).manifest_optional().unwrap();
    embed_resource::compile_for("assets/cli.rc", ["HexCleanerCli"], &macros).manifest_optional().unwrap();

    for file in ["assets/common.rc", "assets/icon.ico"] {
        println!("cargo:rerun-if-changed={file}");
    }
}
