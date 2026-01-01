use crate::fix::structs::CommandOutput;
use std::io;
use std::process::Command;

pub fn get_command_output(expand_command: String) -> io::Result<CommandOutput> {
    let split_command = shell_words::split(&expand_command)
        .map_err(|e| io::Error::other(format!("Failed to parse command: {e}")))?;
    let mut command = Command::new(&split_command[0]);
    command
        .args(&split_command[1..])
        .env("LANG", "C") // Set locale to C to avoid issues with rules that depend on locale
        .env("LC_ALL", "C");

    let output = command.output()?;
    Ok(CommandOutput::from(output))
}
