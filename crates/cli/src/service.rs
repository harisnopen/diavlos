//! Run the helper as a service: systemd (Linux), launchd (macOS), or a
//! logon entry for the user (Windows). Always as the user, never as root
//! or SYSTEM. Config file, not just flags.

use std::path::PathBuf;
use std::process::Command;

use diavlos_client::Paths;

const NAME: &str = "diavlos";

/// Per-user programs to start at logon.
#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Did an older version register the SYSTEM-level Windows service?
#[cfg(windows)]
fn legacy_service_exists() -> bool {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .and_then(|m| m.open_service(NAME, ServiceAccess::QUERY_STATUS))
        .is_ok()
}

fn exe() -> anyhow::Result<PathBuf> {
    Ok(std::env::current_exe()?)
}

#[cfg(target_os = "linux")]
fn unit_path() -> anyhow::Result<PathBuf> {
    let base = directories::BaseDirs::new().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    Ok(base
        .config_dir()
        .join("systemd/user")
        .join(format!("{NAME}.service")))
}

#[cfg(target_os = "macos")]
fn plist_path() -> anyhow::Result<PathBuf> {
    let base = directories::BaseDirs::new().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    Ok(base
        .home_dir()
        .join("Library/LaunchAgents")
        .join("sh.diavlos.helper.plist"))
}

/// One ExecStart word, so a path with a space, `%` or `$` stays one path.
#[cfg(any(target_os = "linux", test))]
fn systemd_quote(s: &str) -> String {
    let inner = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{inner}\"")
}

#[cfg(any(target_os = "macos", test))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn run(cmd: &mut Command) -> String {
    match cmd.output() {
        Ok(o) if o.status.success() => "ok".into(),
        Ok(o) => format!("failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => format!("not run: {e}"),
    }
}

/// Install and start the service. Returns what was done, line by line.
pub fn install(paths: &Paths) -> anyhow::Result<Vec<String>> {
    let exe = exe()?;
    let home = paths.home.display().to_string();
    let mut out = Vec::new();
    #[cfg(target_os = "linux")]
    {
        let unit = unit_path()?;
        std::fs::create_dir_all(unit.parent().unwrap())?;
        std::fs::write(
            &unit,
            format!(
                "[Unit]\nDescription=Diavlos helper\nAfter=network-online.target\n\n\
                 [Service]\nExecStart={} --home {} helper\nRestart=always\nRestartSec=2\n\n\
                 [Install]\nWantedBy=default.target\n",
                systemd_quote(&exe.display().to_string()),
                systemd_quote(&home)
            ),
        )?;
        out.push(format!("wrote {}", unit.display()));
        out.push(format!(
            "systemctl --user daemon-reload: {}",
            run(Command::new("systemctl").args(["--user", "daemon-reload"]))
        ));
        out.push(format!(
            "systemctl --user enable --now {NAME}: {}",
            run(Command::new("systemctl").args(["--user", "enable", "--now", NAME]))
        ));
        out.push(
            "on a server with no login session, also run: loginctl enable-linger $USER".into(),
        );
    }
    #[cfg(target_os = "macos")]
    {
        let plist = plist_path()?;
        std::fs::create_dir_all(plist.parent().unwrap())?;
        std::fs::write(
            &plist,
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>sh.diavlos.helper</string>
  <key>ProgramArguments</key><array>
    <string>{}</string><string>--home</string><string>{}</string><string>helper</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
"#,
                xml_escape(&exe.display().to_string()),
                xml_escape(&home)
            ),
        )?;
        out.push(format!("wrote {}", plist.display()));
        let uid = unsafe { libc::getuid() };
        let r = run(Command::new("launchctl").args([
            "bootstrap",
            &format!("gui/{uid}"),
            &plist.display().to_string(),
        ]));
        out.push(format!("launchctl bootstrap: {r}"));
    }
    #[cfg(windows)]
    {
        // Not a Windows service: that runs as LocalSystem, which is far more
        // power than a network-facing program holding one person's keys
        // needs, and cannot read that person's Credential Manager anyway.
        // Instead start at logon, as the user, from their own Run key. No
        // admin needed. conhost --headless keeps a console window from
        // opening for it.
        let cmd = format!(
            "conhost.exe --headless \"{}\" --home \"{home}\" helper",
            exe.display()
        );
        out.push(format!(
            "start at logon ({RUN_KEY}\\{NAME}): {}",
            run(Command::new("reg")
                .args(["add", RUN_KEY, "/v", NAME, "/t", "REG_SZ", "/d", &cmd, "/f"]))
        ));
        if legacy_service_exists() {
            out.push(format!(
                "an older install registered a Windows service {NAME} that runs as SYSTEM; \
                 remove it from an admin prompt with: sc.exe delete {NAME}"
            ));
        }
        // Start it now rather than at the next logon: any command does.
        out.push(format!(
            "start now: {}",
            run(Command::new(&exe).args(["--home", &home, "status"]))
        ));
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (exe, home);
        out.push("no service manager known for this platform; run `diavlos helper` from your init system".into());
    }
    Ok(out)
}

/// Stop and remove the service.
pub fn uninstall(_paths: &Paths) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    #[cfg(target_os = "linux")]
    {
        out.push(format!(
            "systemctl --user disable --now {NAME}: {}",
            run(Command::new("systemctl").args(["--user", "disable", "--now", NAME]))
        ));
        let unit = unit_path()?;
        if unit.exists() {
            std::fs::remove_file(&unit)?;
            out.push(format!("removed {}", unit.display()));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let plist = plist_path()?;
        let uid = unsafe { libc::getuid() };
        out.push(format!(
            "launchctl bootout: {}",
            run(Command::new("launchctl").args([
                "bootout",
                &format!("gui/{uid}"),
                &plist.display().to_string()
            ]))
        ));
        if plist.exists() {
            std::fs::remove_file(&plist)?;
            out.push(format!("removed {}", plist.display()));
        }
    }
    #[cfg(windows)]
    {
        out.push(format!(
            "remove start at logon: {}",
            run(Command::new("reg").args(["delete", RUN_KEY, "/v", NAME, "/f"]))
        ));
        out.push(format!(
            "stop the helper: {}",
            run(Command::new(exe()?).args(["--home", &_paths.home.display().to_string(), "stop"]))
        ));
        // An older install used a Windows service. Remove it if we may;
        // it takes an admin prompt, so say how if we may not.
        if legacy_service_exists() {
            use windows_service::service::ServiceAccess;
            use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
            let removed =
                ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
                    .and_then(|m| {
                        m.open_service(
                            NAME,
                            ServiceAccess::STOP
                                | ServiceAccess::DELETE
                                | ServiceAccess::QUERY_STATUS,
                        )
                    })
                    .and_then(|svc| {
                        let _ = svc.stop();
                        svc.delete()
                    });
            out.push(match removed {
                Ok(()) => format!("removed the older Windows service {NAME}"),
                Err(e) => format!(
                    "could not remove the older Windows service {NAME} ({e}); \
                     from an admin prompt run: sc.exe delete {NAME}"
                ),
            });
        }
    }
    if out.is_empty() {
        out.push("nothing to remove on this platform".into());
    }
    Ok(out)
}

/// Windows only: run under the service control manager.
#[cfg(windows)]
pub fn run_as_service(paths: Paths) -> anyhow::Result<()> {
    use std::sync::OnceLock;
    static PATHS: OnceLock<Paths> = OnceLock::new();
    let _ = PATHS.set(paths);

    windows_service::define_windows_service!(ffi_service_main, service_main);

    fn service_main(_args: Vec<std::ffi::OsString>) {
        use std::time::Duration;
        use windows_service::service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        };
        use windows_service::service_control_handler::{self, ServiceControlHandlerResult};

        let paths = PATHS.get().cloned().expect("paths set");
        let stop_paths = paths.clone();
        let handler = move |control| -> ServiceControlHandlerResult {
            match control {
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::Stop => {
                    let p = stop_paths.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new().expect("runtime");
                        rt.block_on(async {
                            let _ = diavlos_client::Client::new(p)
                                .call_if_running(&diavlos_client::proto::Request::Stop)
                                .await;
                        });
                    });
                    ServiceControlHandlerResult::NoError
                }
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };
        let Ok(status) = service_control_handler::register(NAME, handler) else {
            return;
        };
        let running = |state: ServiceState| ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        };
        let _ = status.set_service_status(running(ServiceState::Running));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt.block_on(crate::helper::run(paths));
        let _ = status.set_service_status(running(ServiceState::Stopped));
    }

    windows_service::service_dispatcher::start(NAME, ffi_service_main)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_with_spaces_and_specials_stay_one_word() {
        assert_eq!(
            systemd_quote("/home/Jane Doe/.diavlos"),
            "\"/home/Jane Doe/.diavlos\""
        );
        assert_eq!(systemd_quote(r#"/a%b$c"d\e"#), r#""/a%%b$$c\"d\\e""#);
        assert_eq!(xml_escape("/Users/A&B <x>"), "/Users/A&amp;B &lt;x&gt;");
    }
}
