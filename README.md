# HexCleaner

**English** | [Русский](README.ru.md) | [Українська](README.uk.md)

A cleaner for Windows junk files and caches. Written in Rust, with an [egui](https://github.com/emilk/egui) interface.

HexCleaner deletes only caches, temporary files, logs and error reports.
It never touches documents, game saves, or browser cookies, passwords and history.
The Recycle Bin is emptied only if the "Empty the Recycle Bin" item is checked (it is off by default).

**Download:** [latest release](https://github.com/ModdingTechNik/HexCleaner/releases/latest) ·
**Support the project:** [Patreon](https://patreon.com/ModdingTechNik)

## Building

```bash
cargo build --release
```

This produces two programs:

* `target/release/HexCleaner.exe` — the window;
* `target/release/HexCleanerCli.exe` — the command line (for scripts and Windows Task Scheduler).

Both run on the same core and share the same settings.

## How it works

* Items with a shield icon need administrator rights. Without them they are disabled and skipped
  during cleanup, but their checkboxes are remembered. The "Run as administrator" button
  restarts the program through UAC.
* Every item has a safety level — a colored dot in the list:
  🟢 "Safe" (temporary files and caches, everything is recreated automatically),
  🟡 "Moderate" (something will be downloaded again or diagnostic data is lost),
  🔴 "Caution" (event logs, Defender history, files used to repair installed programs).
* Presets: the built-in "Safe only", "Recommended" (safe and moderate) and "Full" are built
  from the safety levels. The current selection can be saved as your own preset and later
  applied or deleted. Items you have never touched are checked according to the recommended preset.
* The "Analyze" button deletes and changes nothing: it walks the same folders by the same rules
  and shows how much junk each item contains. An item can be expanded to see its folders with
  their size and file count (clicking a folder opens it in Explorer). Analysis results are written
  to an `analysis_….log` report. If you start a cleanup after an analysis, a progress bar at the
  bottom of the window shows how many of the found files have already been deleted.
* The item selection, your presets and everything on the "Settings" tab are stored in
  `%APPDATA%\HexCleaner\settings.json`. On first launch the language is taken from Windows settings.
* Every cleanup writes a log to `%LOCALAPPDATA%\HexCleaner\logs`: all deleted files with their
  sizes, errors and totals for each item. If "Compress previous logs" is checked in the settings
  (on by default), logs of previous cleanups are packed into zip archives (one per log)
  before a new cleanup.
* Files in use are skipped. Temporary files younger than an hour are not deleted.
* Before cleaning, the program uses Restart Manager to check whether running browsers or launchers
  (Steam, Epic) hold files of the selected items, and offers to close them — with checkboxes and
  an option to start them again after the cleanup. They are closed gracefully, the same way as
  when Windows shuts down (browsers keep their tabs); nothing is killed.
  Only programs from the item's `programs_to_close` list are offered: shared folders
  (the GPU shader cache, `INetCache`) are also held by unrelated programs and Windows components.
* For every file that could not be deleted, the log gives the reason, for example
  "in use by chrome.exe (PID 1234)", "running or loaded as a library",
  "access denied, owner NT SERVICE\TrustedInstaller". Blocking processes are looked up through
  Restart Manager no more than 200 times per item so as not to slow the cleanup down.
* Symbolic links and junctions are never followed, so they cannot lead to other files.
  Drive roots, the user profile, Windows, Program Files and similar folders are protected from
  cleanup even if the catalog contains a mistake.

## Command line

```bash
HexCleanerCli analyze --preset recommended --details
```

```bash
HexCleanerCli clean --preset safe --tasks recycle_bin --quiet
```

| Command | What it does |
|---|---|
| `analyze [what] [options]` | Analysis: how much junk is found. Deletes nothing |
| `clean [what] [options]` | Cleanup |
| `list` | Items (ids, safety level, whether checked in the program) and presets |
| `help` | Help with examples |

What to process — everything listed is combined:

* `--preset <name>` — `safe`, `recommended`, `full` or the name of your own preset;
* `--tasks <id,id,...>` — individual items (ids from `list`);
* `--exclude <id,id,...>` — remove items from the selection;
* `--selected` — items checked in the program window. If nothing is specified, these are used.

Options: `--details` (folders with junk), `--quiet` (totals only), `--lang <code>`,
`--compress-logs` / `--no-compress-logs`, `--close-apps` (close browsers and launchers that are
in the way before cleaning; with analyze, only list them), `--restart-apps` (start the closed
programs again after the cleanup).

Items that need administrator rights are skipped without them — run the command prompt
(or the scheduled task) as administrator to process them.
Ctrl+C stops gracefully: services are started again and the log is completed.

Exit codes (shown by Windows Task Scheduler):

| Code | Meaning |
|---|---|
| 0 | Done without errors |
| 1 | Invalid arguments |
| 2 | Done, but some files could not be deleted or some items were skipped |
| 3 | Could not start (e.g. could not create the log) |
| 4 | Stopped with Ctrl+C |

## Scheduled cleanup

The "Scheduled cleanup" tab creates tasks in Windows Task Scheduler (the `\HexCleaner\` folder):
what to clean (a preset or the checked items), when (daily, on chosen weekdays or on a day of the
month) and at what time, whether to close programs that are in the way, and whether to run with
administrator rights (this can be chosen only if HexCleaner itself runs as administrator).
Tasks can be enabled and disabled, edited, deleted and run right away.

A task runs `HexCleaner.exe --scheduled clean --tasks … --quiet` — the same command as
`HexCleanerCli.exe`, but without a window and without a flashing console; the log is written as usual.
Any command-line command can be run this way, even if `HexCleanerCli.exe` is missing:
`HexCleaner --scheduled help`. `HexCleaner.exe` accepts no other arguments: for them, as for
`--help`, it shows help — in the console if started from one, otherwise in a message box.
The program regularly compares its list with Task Scheduler: if a task was deleted in
Task Scheduler, it is marked in the list and can be recreated or removed.
HexCleaner tasks in Task Scheduler that are not in the list are shown separately.

## Settings and updates

The "Settings" tab: language and theme (dark, light or as in Windows); a notification when
analysis or cleanup finishes and flashing of the taskbar button (each can be turned off);
compression of previous logs; updates; links to the GitHub releases and to Patreon.

At startup the window asks GitHub for the latest release
(`api.github.com/repos/ModdingTechNik/HexCleaner/releases/latest`; drafts and pre-releases
are not included). If its tag is newer than the program version, a dialog appears:
"Update", "Remind me in a day / 3 days / a week", "Skip this version" (the program will remind
you about the next one) or "Don't check for updates" (can be turned back on in the settings).
The "Check now" button is there too: a manual check offers even a skipped version.
At startup GitHub is asked at most once every 5 minutes (the time of the last check is stored in
the settings, `updates.last_check`); the "Check now" button always works.

Versions are compared by SemVer rules with any number of parts: `0.10.0` is newer than `0.9.1`,
`0.1` and `0.1.0.0` are the same version, `0.2.0-beta.1` is older than `0.2.0`. The program
tells you if the installed version is newer than the latest release (e.g. built before it was
published), if there are no published releases (only drafts or pre-releases), and if the tag of
the latest release is not a version number.

Updating: the release zip archive is downloaded, verified against the SHA-256 checksum
computed by GitHub and unpacked next to the program. The running exe is renamed to
`*.update-old` (deleted at the next start), the new one takes its place, and the program restarts.
The path does not change, so scheduled tasks keep working. If the program folder is not writable
(e.g. `Program Files`), HexCleaner asks to be run as administrator. Networking goes through
WinHTTP, built into Windows (system proxy and certificates), without third-party libraries.

How to publish an update:

1. Bump `version` in `Cargo.toml` (e.g. `0.2.0`) and build with `cargo build --release`.
2. Pack `HexCleaner.exe` and `HexCleanerCli.exe` into a zip (any name, e.g. `release.zip`).
3. Create a GitHub release with a tag equal to the version (`0.2.0` or `v0.2.0`) and attach the
   archive. The program shows the release description in the dialog as "What's new". A release
   marked "Set as a pre-release" is not offered — that's how you can publish a test build.

## Languages

All program texts (interface, logs, error reasons) live in translation files; the code contains
only keys like `t!("footer.analyze")`. The built-in languages — Russian, Ukrainian and English
(`languages/ru.json`, `languages/uk.json`, `languages/en.json`) — are embedded in the program.
On first launch the language is taken from Windows settings.

At startup the program puts them into `%APPDATA%\HexCleaner\languages`
(the "Languages folder…" button on the "Settings" tab). To add your own language:

1. Copy `en.json`, for example, to `de.json` — the file name becomes the language code.
2. Change `name` (how the language is called in the list) and translate the texts in `strings`.
   Leave the placeholders in curly braces (`{files}`, `{size}` …) as they are.
3. Open the language list — the new language appears there.

Untranslated keys fall back to English, so a translation can be done gradually.
A `ru.json` or `en.json` file in this folder extends the built-in language: it only needs the
texts you want to change. A deleted built-in language file is recreated at the next start.

Tests check that every key used in the code has a translation, that the dictionaries contain no
unused keys, and that no untranslated Russian strings are left in the code.

## Icon

The sources are `assets/icon.svg` (for 32 px and larger) and `assets/icon-small.svg`
(simplified, for 16–24 px: at small sizes details blur together). After editing the SVG:

```bash
cargo run --example make_icon
```

This rebuilds `assets/icon.ico` (all Windows sizes) and `assets/icon-256.png`
(the window icon). `build.rs` embeds the icon and file information (`assets/*.rc`) into both exe files.

## Project structure

| File | Purpose |
|---|---|
| `src/lib.rs` | The core — a library shared by the window and the command line |
| `src/catalog.rs` | The list of cleanup items and paths. **New items are added here.** |
| `src/presets.rs` | Presets: built-in by safety level and saved by the user |
| `src/i18n.rs`, `languages/` | Localization: loading translations, the `t!` macro, built-in dictionaries |
| `src/cleaner/` | The cleanup engine: thread, events for the interface, file deletion, log, statistics |
| `src/system/` | Windows API wrappers (rights, services, event logs, memory, Restart Manager, HTTPS, SHA-256). All `unsafe` code lives here |
| `src/settings.rs` | Saving the selected items, presets and settings |
| `src/update.rs` | Updating from GitHub releases: version check, download, installation |
| `src/cli/` | Command line: argument parsing, item selection, output, exit codes |
| `src/schedule.rs` | A scheduled task and its XML for Task Scheduler |
| `src/main.rs`, `src/app.rs`, `src/widgets.rs` | The window (`HexCleaner.exe`) |
| `src/schedule_tab.rs`, `src/progress.rs` | The scheduled cleanup tab, cleanup progress after analysis |
| `src/settings_tab.rs`, `src/update_ui.rs` | The "Settings" tab, the update dialog |
| `src/bin/cli.rs` | The command line (`HexCleanerCli.exe`) |
| `tests/cli.rs`, `tests/gui.rs` | Tests of `HexCleanerCli.exe` and of `HexCleaner.exe` arguments as finished programs |
| `assets/`, `build.rs`, `examples/make_icon.rs` | The icon, file information and their build |

### Adding an item

Add another `CleanupTask` to `all_tasks()` in `src/catalog.rs`:

```rust
CleanupTask {
    id: "my_app_cache",                       // the checkbox is remembered by this id
    safety: Safety::Safe,                     // safety level: Safe, Moderate or Caution
    programs_to_close: &[],                   // programs that may be offered to close
    requires_admin: false,
    services_to_stop: &[],
    actions: vec![
        clear_dir(r"%LOCALAPPDATA%\MyApp\Cache"),          // clear the folder contents
        delete_files(r"%LOCALAPPDATA%\MyApp\*.log"),       // delete files by mask
    ],
},
```

Add the item's title and description to `languages/en.json`, `languages/ru.json` and
`languages/uk.json` with the keys `task.my_app_cache.title` and `task.my_app_cache.description`.

Paths may use environment variables (`%LOCALAPPDATA%`, `%WINDIR%` …),
the special `%STEAM%` variable (the Steam folder from the registry) and the `*` mask.

## Support

If HexCleaner is useful to you, you can support its development on
[Patreon](https://patreon.com/ModdingTechNik).
