use crate::fix::structs::CommandOutput;
use crossterm::style::Stylize;
use libc::{geteuid, getuid, gid_t, uid_t};
use pyo3::impl_::callback::HashCallbackOutput;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

pub fn get_command_output(expand_command: String) -> io::Result<CommandOutput> {
    let split_command = shell_words::split(&expand_command)
        .map_err(|e| io::Error::other(format!("Failed to parse command: {e}")))?;

    if split_command.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Command is empty",
        ));
    }

    let mut command = Command::new(&split_command[0]);
    command
        .args(&split_command[1..])
        .env("LANG", "C") // Set locale to C to avoid issues with rules that depend on locale
        .env("LC_ALL", "C");

    let permission_issue = PermissionIssue::detect();
    permission_issue.fix(&mut command).map_err(|e| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("Failed to fix permissions: {}", e),
        )
    })?;

    let output = command.output()?;
    Ok(CommandOutput::from(output))
}

enum PermissionIssue {
    EuidNotEqualRuid, // SUID/SGID execution detected
    SudoEnvironment,  // SUDO_UID or SUDO_GID is set
    DoasEnvironment,  // Detected running in a Doas environment
    Normal,           // No permission issues detected
}

impl PermissionIssue {
    fn detect() -> Self {
        if unsafe { geteuid() != getuid() } {
            return PermissionIssue::EuidNotEqualRuid;
        }
        if std::env::var("SUDO_UID").is_ok() || std::env::var("SUDO_GID").is_ok() {
            return PermissionIssue::SudoEnvironment;
        }
        if std::env::var("DOAS_USER").is_ok() {
            return PermissionIssue::DoasEnvironment;
        }
        PermissionIssue::Normal
    }

    fn fix(self, command: &mut Command) -> Result<(), String> {
        let current_euid = unsafe { geteuid() };
        let current_uid = unsafe { getuid() };
        let current_egid = unsafe { libc::getegid() };
        let current_gid = unsafe { libc::getgid() };
        let change_supplementary_groups = match self {
            PermissionIssue::DoasEnvironment => true,
            PermissionIssue::SudoEnvironment => true,
            _ => false,
        };

        let target = match self {
            PermissionIssue::EuidNotEqualRuid => (current_uid, current_gid),
            PermissionIssue::SudoEnvironment => {
                let uid = std::env::var("SUDO_UID").map_err(|e| {
                    format!(
                        "{} Detected sudo environment, but cannot get SUDO_UID: {}",
                        "SECURITY ERROR:".red().bold(),
                        e
                    )
                })?;
                let gid = std::env::var("SUDO_GID").map_err(|e| {
                    format!(
                        "{} Detected sudo environment, but cannot get SUDO_GID: {}",
                        "SECURITY ERROR:".red().bold(),
                        e
                    )
                })?;
                (
                    uid.parse::<uid_t>().map_err(|e| {
                        format!(
                            "{} Invalid SUDO_UID value: {}",
                            "SECURITY ERROR:".red().bold(),
                            e
                        )
                    })?,
                    gid.parse::<gid_t>().map_err(|e| {
                        format!(
                            "{} Invalid SUDO_GID value: {}",
                            "SECURITY ERROR:".red().bold(),
                            e
                        )
                    })?,
                )
            }
            PermissionIssue::DoasEnvironment => {
                let doas_user = std::env::var("DOAS_USER").map_err(|e| {
                    format!(
                        "{} Detected doas environment, but cannot get DOAS_USER: {}",
                        "SECURITY ERROR:".red().bold(),
                        e
                    )
                })?;
                get_ids_by_username(doas_user)?
            }
            PermissionIssue::Normal => return Ok(()),
        };

        if target == (current_euid, current_egid) {
            return Ok(());
        }

        let user_context = get_user_context_by_uid(target.0)?;

        if change_supplementary_groups && current_euid != 0 {
            return Err(format!(
                "{} Cannot change supplementary groups when not running as root.",
                "SECURITY ERROR:".red().bold()
            ));
        }

        command.env("USER", &user_context.username);
        command.env("HOME", &user_context.home_dir);

        command.env_remove("SUDO_UID");
        command.env_remove("SUDO_GID");
        command.env_remove("SUDO_USER");
        command.env_remove("SUDO_COMMAND");
        command.env_remove("DOAS_USER");

        let username_cstring = std::ffi::CString::new(user_context.username).unwrap();

        unsafe {
            command.pre_exec(move || {
                if current_euid == 0 && change_supplementary_groups {
                    if libc::initgroups(username_cstring.as_ptr(), target.1) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                if libc::setgid(target.1) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::setuid(target.0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}
fn get_ids_by_username(username: String) -> Result<(uid_t, gid_t), String> {
    let user_cstr = std::ffi::CString::new(username.clone()).map_err(|e| {
        format!(
            "{} Username contains null bytes: {}",
            "SECURITY ERROR:".red().bold(),
            e
        )
    })?;
    let passwd = unsafe { libc::getpwnam(user_cstr.as_ptr()) };
    if passwd.is_null() {
        return Err(format!(
            "{} Cannot find user info for '{}'",
            "SECURITY ERROR:".red().bold(),
            username
        ));
    }
    unsafe { Ok(((*passwd).pw_uid, (*passwd).pw_gid)) }
}

struct UserContext {
    username: String,
    home_dir: String,
}

fn get_user_context_by_uid(uid: uid_t) -> Result<UserContext, String> {
    let passwd = unsafe { libc::getpwuid(uid) };
    if passwd.is_null() {
        return Err(format!(
            "{} Cannot find user info for UID '{}'",
            "SECURITY ERROR:".red().bold(),
            uid
        ));
    }
    unsafe {
        let username = std::ffi::CStr::from_ptr((*passwd).pw_name)
            .to_string_lossy()
            .into_owned();

        let home_dir = std::ffi::CStr::from_ptr((*passwd).pw_dir)
            .to_string_lossy()
            .into_owned();

        Ok(UserContext { username, home_dir })
    }
}
