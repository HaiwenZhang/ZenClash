use std::{io, process::ExitStatus};

use super::linux::show_with;

fn status(code: u8) -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        ExitStatus::from_raw(i32::from(code) << 8)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt as _;
        ExitStatus::from_raw(u32::from(code))
    }
}

#[test]
fn nonzero_exit_falls_back_to_the_next_dialog() {
    let mut programs = Vec::new();
    show_with("startup failed", |command| {
        let program = command.get_program().to_str().unwrap();
        programs.push(program.to_owned());
        if program == "zenity" {
            assert!(
                command
                    .get_args()
                    .any(|argument| argument == "startup failed")
            );
        }
        Ok(status(u8::from(program == "zenity")))
    });
    assert_eq!(programs, ["zenity", "kdialog"]);
}

#[test]
fn missing_and_failed_dialogs_fall_back_to_notification() {
    let mut programs = Vec::new();
    show_with("startup failed", |command| {
        let program = command.get_program().to_str().unwrap();
        programs.push(program.to_owned());
        match program {
            "zenity" => Err(io::ErrorKind::NotFound.into()),
            "kdialog" => Ok(status(1)),
            _ => Ok(status(0)),
        }
    });
    assert_eq!(programs, ["zenity", "kdialog", "notify-send"]);
}

#[test]
fn successful_dialog_stops_fallback() {
    let mut programs = Vec::new();
    show_with("startup failed", |command| {
        programs.push(command.get_program().to_str().unwrap().to_owned());
        Ok(status(0))
    });
    assert_eq!(programs, ["zenity"]);
}

#[test]
fn unavailable_desktop_exhausts_the_candidates() {
    let mut programs = Vec::new();
    show_with("startup failed", |command| {
        programs.push(command.get_program().to_str().unwrap().to_owned());
        Err(io::ErrorKind::NotFound.into())
    });
    assert_eq!(programs, ["zenity", "kdialog", "notify-send"]);
}

#[test]
fn kdialog_dismissal_stops_fallback_and_escapes_markup_and_backslashes() {
    let mut programs = Vec::new();
    show_with("A&B/<node>\\new\nerror", |command| {
        let program = command.get_program().to_str().unwrap();
        programs.push(program.to_owned());
        if program == "zenity" {
            return Err(io::ErrorKind::NotFound.into());
        }
        assert_eq!(
            command.get_args().last().unwrap(),
            "<html>A&amp;B/&lt;node&gt;&#92;new<br>error</html>"
        );
        Ok(status(2))
    });
    assert_eq!(programs, ["zenity", "kdialog"]);
}

#[test]
fn notification_body_escapes_markup_without_changing_backslashes() {
    let mut programs = Vec::new();
    show_with(r"A&B/<node>\new", |command| {
        let program = command.get_program().to_str().unwrap();
        programs.push(program.to_owned());
        if program != "notify-send" {
            return Err(io::ErrorKind::NotFound.into());
        }
        assert_eq!(
            command.get_args().last().unwrap(),
            r"A&amp;B/&lt;node&gt;\new"
        );
        Ok(status(0))
    });
    assert_eq!(programs, ["zenity", "kdialog", "notify-send"]);
}

#[test]
fn zenity_dismissal_stops_fallback_and_preserves_literal_error_text() {
    let message = r"invalid path: /tmp/A&B/<native>\config";
    let mut calls = 0;
    show_with(message, |command| {
        calls += 1;
        assert_eq!(command.get_program(), "zenity");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--error",
                "--no-markup",
                "--title",
                "ZenClash",
                "--text",
                message
            ]
        );
        let variables = command
            .get_envs()
            .collect::<std::collections::BTreeMap<_, _>>();
        for key in ["ZENITY_OK", "ZENITY_ESC", "ZENITY_CANCEL"] {
            assert_eq!(
                variables[std::ffi::OsStr::new(key)],
                Some(std::ffi::OsStr::new("0"))
            );
        }
        assert_eq!(
            variables[std::ffi::OsStr::new("ZENITY_ERROR")],
            Some(std::ffi::OsStr::new("255"))
        );
        // Model the exit code configured for Escape/window dismissal.
        Ok(status(
            variables[std::ffi::OsStr::new("ZENITY_ESC")]
                .unwrap()
                .to_str()
                .unwrap()
                .parse()
                .unwrap(),
        ))
    });
    assert_eq!(calls, 1);
}
