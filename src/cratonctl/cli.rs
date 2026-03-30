use crate::cratonctl::error::CratonctlError;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalArgs {
    pub url: Option<String>,
    pub token: Option<String>,
    pub token_file: Option<String>,
    pub json: bool,
    pub quiet: bool,
    pub no_color: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub global: GlobalArgs,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help { text: String, top_level: bool },
    Health,
    Status,
    Services,
    Service { id: String },
    History { kind: HistoryKind },
    Diagnose { service: String },
    Doctor,
    Trigger { task: String },
    Restart { service: String },
    MaintenanceSet { service: String, reason: String },
    MaintenanceClear { service: String },
    BreakerClear { service: String },
    FlappingClear { service: String },
    BackupRun,
    BackupUnlock,
    DiskCleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    Recovery,
    Backup,
    Remediation,
}

pub fn parse(args: Vec<String>) -> Result<Cli, CratonctlError> {
    let help_flags = extract_help_flags(&args);
    let top_level_help = is_top_level_help_args(&args);
    match RawCli::try_parse_from(args) {
        Ok(raw) => raw.try_into_cli(),
        Err(err) => {
            if err.kind() == clap::error::ErrorKind::DisplayHelp {
                return Ok(Cli {
                    global: help_flags,
                    command: Command::Help {
                        text: err.to_string(),
                        top_level: top_level_help,
                    },
                });
            }
            Err(CratonctlError::Usage(err.to_string().trim_end().into()))
        }
    }
}

pub fn usage() -> String {
    let mut command = RawCli::command();
    let mut help = Vec::new();
    if command.write_long_help(&mut help).is_err() {
        return "cratonctl".into();
    }
    String::from_utf8_lossy(&help).into_owned()
}

fn extract_help_flags(args: &[String]) -> GlobalArgs {
    let mut global = GlobalArgs::default();
    for arg in &args[1..] {
        match arg.as_str() {
            "--json" => global.json = true,
            "--quiet" => global.quiet = true,
            "--no-color" => global.no_color = true,
            _ => {}
        }
    }
    global
}

fn is_top_level_help_args(args: &[String]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|arg| arg.starts_with('-'))
        .any(|arg| arg == "--help" || arg == "-h")
}

#[derive(Debug, Parser)]
#[command(
    name = "cratonctl",
    disable_version_flag = true,
    about = "Thin operator CLI for cratond's HTTP API",
    long_about = "Thin operator CLI for cratond's HTTP API.\nRead-only commands work without a token. Mutating commands require one.",
    after_help = EXTRA_HELP
)]
struct RawCli {
    #[command(flatten)]
    global: RawGlobalArgs,
    #[command(subcommand)]
    command: RawCommand,
}

#[derive(Debug, Args, Default)]
#[command(next_help_heading = "Global options")]
struct RawGlobalArgs {
    #[arg(long, value_name = "url", global = true)]
    url: Option<String>,
    #[arg(long, value_name = "token", global = true)]
    token: Option<String>,
    #[arg(long = "token-file", value_name = "path", global = true)]
    token_file: Option<String>,
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    quiet: bool,
    #[arg(long = "no-color", global = true)]
    no_color: bool,
}

#[derive(Debug, Subcommand)]
enum RawCommand {
    #[command(about = "Check daemon health")]
    Health,
    #[command(about = "Show operator status summary")]
    Status,
    #[command(about = "List services and current state")]
    Services,
    #[command(about = "Show one service")]
    Service {
        #[arg(value_name = "id")]
        id: String,
    },
    #[command(about = "Show historical events")]
    History {
        #[arg(value_enum, value_name = "kind")]
        kind: RawHistoryKind,
    },
    #[command(about = "Collect daemon-side diagnostics for one service")]
    Diagnose {
        #[arg(value_name = "service")]
        service: String,
    },
    #[command(about = "Run safe preflight checks")]
    Doctor,
    #[command(about = "Trigger an existing daemon task")]
    Trigger {
        #[arg(value_name = "task")]
        task: String,
    },
    #[command(about = "Request a service restart through daemon policy")]
    Restart {
        #[arg(value_name = "service")]
        service: String,
    },
    #[command(subcommand, about = "Manage maintenance mode")]
    Maintenance(RawMaintenanceCommand),
    #[command(subcommand, about = "Breaker operations")]
    Breaker(RawClearCommand),
    #[command(subcommand, about = "Flapping operations")]
    Flapping(RawClearCommand),
    #[command(subcommand, about = "Backup operations")]
    Backup(RawBackupCommand),
    #[command(subcommand, about = "Disk operations")]
    Disk(RawDiskCommand),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RawHistoryKind {
    Recovery,
    Backup,
    Remediation,
}

#[derive(Debug, Subcommand)]
enum RawMaintenanceCommand {
    #[command(about = "Set maintenance mode for a service")]
    Set {
        #[arg(value_name = "service")]
        service: String,
        #[arg(long, value_name = "text")]
        reason: String,
    },
    #[command(about = "Clear maintenance mode for a service")]
    Clear {
        #[arg(value_name = "service")]
        service: String,
    },
}

#[derive(Debug, Subcommand)]
enum RawClearCommand {
    #[command(about = "Clear state for a service")]
    Clear {
        #[arg(value_name = "service")]
        service: String,
    },
}

#[derive(Debug, Subcommand)]
enum RawBackupCommand {
    #[command(about = "Run backup now")]
    Run,
    #[command(about = "Unlock backup backend")]
    Unlock,
}

#[derive(Debug, Subcommand)]
enum RawDiskCommand {
    #[command(about = "Run disk cleanup")]
    Cleanup,
}

impl RawCli {
    fn try_into_cli(self) -> Result<Cli, CratonctlError> {
        Ok(Cli {
            global: GlobalArgs {
                url: self.global.url,
                token: self.global.token,
                token_file: self.global.token_file,
                json: self.global.json,
                quiet: self.global.quiet,
                no_color: self.global.no_color,
            },
            command: map_command(self.command)?,
        })
    }
}

fn map_command(command: RawCommand) -> Result<Command, CratonctlError> {
    match command {
        RawCommand::Health => Ok(Command::Health),
        RawCommand::Status => Ok(Command::Status),
        RawCommand::Services => Ok(Command::Services),
        RawCommand::Service { id } => Ok(Command::Service { id }),
        RawCommand::History { kind } => Ok(Command::History {
            kind: match kind {
                RawHistoryKind::Recovery => HistoryKind::Recovery,
                RawHistoryKind::Backup => HistoryKind::Backup,
                RawHistoryKind::Remediation => HistoryKind::Remediation,
            },
        }),
        RawCommand::Diagnose { service } => Ok(Command::Diagnose { service }),
        RawCommand::Doctor => Ok(Command::Doctor),
        RawCommand::Trigger { task } => Ok(Command::Trigger { task }),
        RawCommand::Restart { service } => Ok(Command::Restart { service }),
        RawCommand::Maintenance(command) => match command {
            RawMaintenanceCommand::Set { service, reason } => {
                if reason.trim().is_empty() {
                    return Err(CratonctlError::Usage(
                        "error: maintenance set requires a non-empty --reason".into(),
                    ));
                }
                Ok(Command::MaintenanceSet { service, reason })
            }
            RawMaintenanceCommand::Clear { service } => Ok(Command::MaintenanceClear { service }),
        },
        RawCommand::Breaker(RawClearCommand::Clear { service }) => {
            Ok(Command::BreakerClear { service })
        }
        RawCommand::Flapping(RawClearCommand::Clear { service }) => {
            Ok(Command::FlappingClear { service })
        }
        RawCommand::Backup(RawBackupCommand::Run) => Ok(Command::BackupRun),
        RawCommand::Backup(RawBackupCommand::Unlock) => Ok(Command::BackupUnlock),
        RawCommand::Disk(RawDiskCommand::Cleanup) => Ok(Command::DiskCleanup),
    }
}

const EXTRA_HELP: &str = "Read-only commands:\n  health\n  status\n  services\n  service <id>\n  history <recovery|backup|remediation>\n  diagnose <service>\n  doctor\n\nMutating commands:\n  trigger <task>\n  restart <service>\n  maintenance set <service> --reason <text>\n  maintenance clear <service>\n  breaker clear <service>\n  flapping clear <service>\n  backup run\n  backup unlock\n  disk cleanup\n\nAuth behavior:\n  Read-only commands do not require a token.\n  Mutating commands resolve token in this order:\n    1. --token\n    2. CRATONCTL_TOKEN\n    3. --token-file\n    4. /var/lib/craton/remediation-token\n\nExamples:\n  cratonctl status\n  cratonctl services\n  cratonctl service ntfy\n  cratonctl doctor\n  cratonctl restart ntfy --token-file /var/lib/craton/remediation-token\n  cratonctl maintenance set ntfy --reason \"manual work\"\n  cratonctl backup run --json --token \"$CRATONCTL_TOKEN\"";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> Cli {
        match parse(args.iter().map(|value| (*value).to_string()).collect()) {
            Ok(value) => value,
            Err(err) => panic!("unexpected parse error: {err}"),
        }
    }

    #[test]
    fn parses_global_flags_and_health() {
        let cli = parse_ok(&["cratonctl", "--json", "--no-color", "health"]);
        assert!(cli.global.json);
        assert!(cli.global.no_color);
        assert_eq!(cli.command, Command::Health);
    }

    #[test]
    fn parses_service_command() {
        let cli = parse_ok(&["cratonctl", "--url", "http://127.0.0.1:18800", "service", "ntfy"]);
        assert_eq!(cli.command, Command::Service { id: "ntfy".into() });
    }

    #[test]
    fn parses_history_command() {
        let cli = parse_ok(&["cratonctl", "history", "backup"]);
        assert_eq!(
            cli.command,
            Command::History {
                kind: HistoryKind::Backup
            }
        );
    }

    #[test]
    fn parses_restart_command() {
        let cli = parse_ok(&["cratonctl", "restart", "ntfy"]);
        assert_eq!(
            cli.command,
            Command::Restart {
                service: "ntfy".into()
            }
        );
    }

    #[test]
    fn parses_maintenance_set_command() {
        let cli = parse_ok(&[
            "cratonctl",
            "maintenance",
            "set",
            "ntfy",
            "--reason",
            "manual work",
        ]);
        assert_eq!(
            cli.command,
            Command::MaintenanceSet {
                service: "ntfy".into(),
                reason: "manual work".into()
            }
        );
    }

    #[test]
    fn usage_contains_sections() {
        let text = usage();
        assert!(text.contains("Global options:"));
        assert!(text.contains("Read-only commands:"));
        assert!(text.contains("Mutating commands:"));
        assert!(text.contains("Auth behavior:"));
        assert!(text.contains("Examples:"));
    }

    #[test]
    fn parses_doctor_command() {
        let cli = parse_ok(&["cratonctl", "doctor"]);
        assert_eq!(cli.command, Command::Doctor);
    }

    #[test]
    fn parses_help_as_command() {
        let cli = parse_ok(&["cratonctl", "--help"]);
        assert!(matches!(
            cli.command,
            Command::Help {
                top_level: true,
                ..
            }
        ));
    }
}
