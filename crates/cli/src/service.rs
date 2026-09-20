//! Run the helper as a service: systemd (Linux), launchd (macOS), or a
//! Windows service. Config file, not just flags.

use std::path::PathBuf;
use std::process::Command;

use diavlos_client::Paths;

const NAME: &str = "diavlos";

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
                exe.display(),
                home
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
                exe.display(),
                home
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
        use std::ffi::OsString;
        use windows_service::service::{
            ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType,
        };
        use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )?;
        let info = ServiceInfo {
            name: OsString::from(NAME),
            display_name: OsString::from("Diavlos helper"),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe.clone(),
            launch_arguments: vec![
                OsString::from("--home"),
                OsString::from(&home),
                OsString::from("helper"),
                OsString::from("--service"),
            ],
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };
        let service =
            manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)?;
        service.set_description("Diavlos helper: the channel between AI agents")?;
        out.push(format!("registered Windows service {NAME}"));
        match service.start::<OsString>(&[]) {
            Ok(()) => out.push("started".into()),
            Err(e) => out.push(format!("start: {e}")),
        }
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
        use windows_service::service::ServiceAccess;
        use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(
            NAME,
            ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        )?;
        let _ = service.stop();
        service.delete()?;
        out.push(format!("removed Windows service {NAME}"));
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
