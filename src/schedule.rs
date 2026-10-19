//! Плановая очистка: задания для Планировщика заданий Windows.
//!
//! Список заданий хранится в настройках программы ([`crate::settings::Settings::schedules`]),
//! а в Планировщике каждое задание регистрируется в папке `\HexCleaner\` под своим названием
//! (см. `system::task_scheduler`). Здесь — только описание задания и его XML для Планировщика,
//! без обращений к системе, поэтому всё проверяется тестами.
//!
//! Задание запускает `HexCleaner.exe --scheduled clean …`: это та же команда, что у
//! `HexCleanerCli.exe`, но оконная программа не показывает консольное окно при каждом запуске.

use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::t;

/// Ключ, с которым `HexCleaner.exe` молча выполняет команду командной строки и завершается.
pub const SCHEDULED_FLAG: &str = "--scheduled";

/// Сколько задание может выполняться, прежде чем Планировщик его остановит.
const EXECUTION_TIME_LIMIT: &str = "PT2H";

/// Символы, которые нельзя использовать в названии задания Планировщика.
const FORBIDDEN_NAME_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// Плановое задание очистки.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Название — оно же имя задания в Планировщике (в папке `\HexCleaner\`).
    pub name: String,
    /// Что очищать: идентификаторы пунктов (`CleanupTask::id`).
    pub tasks: Vec<String>,
    /// Откуда взят выбор пунктов («Рекомендуемый», «Мой набор»…) — только для показа.
    pub tasks_label: String,
    pub repeat: Repeat,
    pub hour: u8,
    pub minute: u8,
    /// Перед очисткой закрывать браузеры и лаунчеры, которые держат свои файлы.
    pub close_apps: bool,
    /// Запускать с правами администратора (без них пункты со щитом пропускаются).
    pub run_as_admin: bool,
    pub enabled: bool,
}

/// Как часто запускать.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    Daily,
    /// По выбранным дням недели; `days[0]` — понедельник, `days[6]` — воскресенье.
    Weekly { days: [bool; 7] },
    /// Каждый месяц в этот день (1–28: такие числа есть в любом месяце).
    Monthly { day: u8 },
}

/// Что не так с заданием перед сохранением.
#[derive(Debug, PartialEq, Eq)]
pub enum ScheduleError {
    EmptyName,
    ForbiddenCharacters,
    DuplicateName,
    NoTasks,
    NoWeekdays,
}

impl ScheduleError {
    pub fn message(&self) -> String {
        match self {
            ScheduleError::EmptyName => t!("schedule.error.empty_name"),
            ScheduleError::ForbiddenCharacters => {
                let chars: String = FORBIDDEN_NAME_CHARS.iter().map(|c| format!("{c} ")).collect();
                t!("schedule.error.forbidden_characters", chars = chars.trim_end())
            }
            ScheduleError::DuplicateName => t!("schedule.error.duplicate_name"),
            ScheduleError::NoTasks => t!("schedule.error.no_tasks"),
            ScheduleError::NoWeekdays => t!("schedule.error.no_weekdays"),
        }
    }
}

impl Schedule {
    /// Новое задание по умолчанию: раз в неделю, в воскресенье в 3 часа ночи.
    pub fn new(name: String, tasks: Vec<String>, tasks_label: String) -> Self {
        let mut days = [false; 7];
        days[6] = true;
        Self {
            name,
            tasks,
            tasks_label,
            repeat: Repeat::Weekly { days },
            hour: 3,
            minute: 0,
            close_apps: false,
            run_as_admin: false,
            enabled: true,
        }
    }

    /// Проверка перед сохранением. `other_names` — названия остальных заданий.
    pub fn validate<'a>(&self, other_names: impl IntoIterator<Item = &'a str>) -> Result<(), ScheduleError> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(ScheduleError::EmptyName);
        }
        if name.contains(FORBIDDEN_NAME_CHARS) {
            return Err(ScheduleError::ForbiddenCharacters);
        }
        // Планировщик не различает регистр в названиях заданий.
        if other_names.into_iter().any(|other| other.trim().to_lowercase() == name.to_lowercase()) {
            return Err(ScheduleError::DuplicateName);
        }
        if self.tasks.is_empty() {
            return Err(ScheduleError::NoTasks);
        }
        if matches!(self.repeat, Repeat::Weekly { days } if !days.contains(&true)) {
            return Err(ScheduleError::NoWeekdays);
        }
        Ok(())
    }

    /// Аргументы для `HexCleaner.exe`: `--scheduled clean --tasks a,b --quiet [--close-apps --restart-apps]`.
    pub fn arguments(&self) -> String {
        let mut arguments = format!("{SCHEDULED_FLAG} clean --tasks {} --quiet", self.tasks.join(","));
        if self.close_apps {
            arguments.push_str(" --close-apps --restart-apps");
        }
        arguments
    }

    /// Когда запускается, по-человечески: «По дням: пн, пт — в 03:00».
    pub fn describe_repeat(&self) -> String {
        let time = format!("{:02}:{:02}", self.hour, self.minute);
        match &self.repeat {
            Repeat::Daily => t!("schedule.repeat.daily_at", time = time),
            Repeat::Weekly { days } => {
                let names: Vec<String> = (0..7).filter(|&day| days[day]).map(weekday_short_name).collect();
                t!("schedule.repeat.weekly_at", days = names.join(", "), time = time)
            }
            Repeat::Monthly { day } => t!("schedule.repeat.monthly_at", day = day, time = time),
        }
    }

    /// XML-описание задания для Планировщика заданий Windows.
    /// `exe` — путь к `HexCleaner.exe`, `start` — дата, с которой расписание действует.
    pub fn task_xml(&self, exe: &Path, start: NaiveDate) -> String {
        let run_level = if self.run_as_admin { "HighestAvailable" } else { "LeastPrivilege" };
        let start_boundary = format!("{}T{:02}:{:02}:00", start.format("%Y-%m-%d"), self.hour, self.minute);
        format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>HexCleaner</Author>
    <Description>{description}</Description>
  </RegistrationInfo>
  <Triggers>
    <CalendarTrigger>
      <StartBoundary>{start_boundary}</StartBoundary>
      <Enabled>true</Enabled>
      {schedule}
    </CalendarTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>{run_level}</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>{EXECUTION_TIME_LIMIT}</ExecutionTimeLimit>
    <Enabled>{enabled}</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{command}"</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>"#,
            description = escape_xml(&t!("schedule.task_description")),
            schedule = self.schedule_xml(),
            enabled = self.enabled,
            command = escape_xml(&exe.display().to_string()),
            arguments = escape_xml(&self.arguments()),
        )
    }

    /// Часть `CalendarTrigger`, которая задаёт повторение.
    fn schedule_xml(&self) -> String {
        match &self.repeat {
            Repeat::Daily => "<ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay>".to_owned(),
            Repeat::Weekly { days } => {
                let days: String = (0..7).filter(|&day| days[day]).map(|day| format!("<{}/>", XML_WEEKDAYS[day])).collect();
                format!("<ScheduleByWeek><DaysOfWeek>{days}</DaysOfWeek><WeeksInterval>1</WeeksInterval></ScheduleByWeek>")
            }
            Repeat::Monthly { day } => {
                let months: String = XML_MONTHS.iter().map(|month| format!("<{month}/>")).collect();
                format!("<ScheduleByMonth><DaysOfMonth><Day>{day}</Day></DaysOfMonth><Months>{months}</Months></ScheduleByMonth>")
            }
        }
    }
}

/// Названия дней недели в XML Планировщика, с понедельника.
const XML_WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

const XML_MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];

/// Короткое название дня недели на языке интерфейса; `day`: 0 — понедельник.
pub fn weekday_short_name(day: usize) -> String {
    const KEYS: [&str; 7] = [
        "weekday.short.monday",
        "weekday.short.tuesday",
        "weekday.short.wednesday",
        "weekday.short.thursday",
        "weekday.short.friday",
        "weekday.short.saturday",
        "weekday.short.sunday",
    ];
    t!(KEYS[day])
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule() -> Schedule {
        Schedule::new("Еженедельная".to_owned(), vec!["user_temp".into(), "browsers_cache".into()], "Свой".into())
    }

    #[test]
    fn arguments_run_cli_command_quietly() {
        let mut schedule = schedule();
        assert_eq!(schedule.arguments(), "--scheduled clean --tasks user_temp,browsers_cache --quiet");
        schedule.close_apps = true;
        assert!(schedule.arguments().ends_with("--quiet --close-apps --restart-apps"));
    }

    #[test]
    fn xml_contains_trigger_command_and_rights() {
        let mut schedule = schedule();
        schedule.repeat = Repeat::Weekly { days: [true, false, false, false, true, false, false] };
        schedule.hour = 4;
        schedule.minute = 5;
        schedule.run_as_admin = true;
        let date = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        let xml = schedule.task_xml(Path::new(r"C:\Program Files\R&D\HexCleaner.exe"), date);

        assert!(xml.contains("<StartBoundary>2026-10-19T04:05:00</StartBoundary>"));
        assert!(xml.contains("<DaysOfWeek><Monday/><Friday/></DaysOfWeek>"));
        assert!(xml.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(xml.contains(r#"<Command>"C:\Program Files\R&amp;D\HexCleaner.exe"</Command>"#));
        assert!(xml.contains("<Arguments>--scheduled clean --tasks user_temp,browsers_cache --quiet</Arguments>"));
        assert!(xml.contains("<Enabled>true</Enabled>"));
    }

    #[test]
    fn monthly_and_daily_triggers() {
        let mut schedule = schedule();
        let date = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        schedule.repeat = Repeat::Monthly { day: 15 };
        let xml = schedule.task_xml(Path::new("HexCleaner.exe"), date);
        assert!(xml.contains("<DaysOfMonth><Day>15</Day></DaysOfMonth>") && xml.contains("<December/>"));

        schedule.repeat = Repeat::Daily;
        let xml = schedule.task_xml(Path::new("HexCleaner.exe"), date);
        assert!(xml.contains("<DaysInterval>1</DaysInterval>"));
    }

    #[test]
    fn validation() {
        let mut schedule = schedule();
        assert_eq!(schedule.validate(["Другое"]), Ok(()));
        assert_eq!(schedule.validate(["еженедельная"]), Err(ScheduleError::DuplicateName));

        schedule.name = "  ".into();
        assert_eq!(schedule.validate([]), Err(ScheduleError::EmptyName));
        schedule.name = "a/b".into();
        assert_eq!(schedule.validate([]), Err(ScheduleError::ForbiddenCharacters));

        schedule.name = "Ок".into();
        schedule.repeat = Repeat::Weekly { days: [false; 7] };
        assert_eq!(schedule.validate([]), Err(ScheduleError::NoWeekdays));
        schedule.tasks.clear();
        assert_eq!(schedule.validate([]), Err(ScheduleError::NoTasks));
    }
}
