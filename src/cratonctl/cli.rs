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
    AuthStatus,
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
    #[arg(
        long,
        value_name = "url",
        global = true,
        help = "Daemon base URL",
        long_help = "Daemon base URL. Resolution order: --url, CRATONCTL_URL, then http://127.0.0.1:18800."
    )]
    url: Option<String>,
    #[arg(
        long,
        value_name = "token",
        global = true,
        help = "Bearer token for mutating commands",
        long_help = "Bearer token for mutating commands. Read-only commands do not require a token."
    )]
    token: Option<String>,
    #[arg(
        long = "token-file",
        value_name = "path",
        global = true,
        help = "Read bearer token from file",
        long_help = "Read bearer token from file. Resolution order for mutating commands: --token, CRATONCTL_TOKEN, --token-file, then /var/lib/craton/remediation-token."
    )]
    token_file: Option<String>,
    #[arg(long, global = true, help = "Print a single JSON document")]
    json: bool,
    #[arg(long, global = true, help = "Minimize human-readable output")]
    quiet: bool,
    #[arg(long = "no-color", global = true, help = "Disable color output")]
    no_color: bool,
}

#[derive(Debug, Subcommand)]
enum RawCommand {
    #[command(about = "Check daemon health")]
    Health,
    #[command(
        subcommand,
        about = "Inspect auth/token readiness",
        long_about = "Inspect auth and token readiness for mutating commands without printing secrets or sending mutating requests.",
        after_help = AUTH_HELP
    )]
    Auth(RawAuthCommand),
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
    #[command(
        about = "Run safe preflight checks",
        long_about = "Run safe preflight checks against the daemon API and local token access.\nThis command never sends mutating requests.",
        after_help = DOCTOR_HELP
    )]
    Doctor,
    #[command(
        about = "Trigger an existing daemon task",
        long_about = "Trigger an existing daemon task through POST /trigger/{task}.\nThe CLI stays thin here: the daemon remains the source of truth for which tasks are accepted.",
        after_help = TRIGGER_HELP
    )]
    Trigger {
        #[arg(
            value_name = "task",
            help = "Task name accepted by the daemon",
            long_help = "Task name accepted by the daemon. Common examples are recovery and backup; the daemon validates the final value."
        )]
        task: String,
    },
    #[command(about = "Request a service restart through daemon policy")]
    Restart {
        #[arg(value_name = "service")]
        service: String,
    },
    #[command(
        subcommand,
        about = "Manage maintenance mode",
        long_about = "Manage maintenance mode through daemon remediation actions.\nUse set with a non-empty reason; clear removes maintenance.",
        after_help = MAINTENANCE_HELP
    )]
    Maintenance(RawMaintenanceCommand),
    #[command(subcommand, about = "Breaker operations")]
    Breaker(RawClearCommand),
    #[command(subcommand, about = "Flapping operations")]
    Flapping(RawClearCommand),
    #[command(
        subcommand,
        about = "Backup operations",
        long_about = "Backup-related operator commands. `run` triggers a backup through the daemon; `unlock` requests a restic unlock action.",
        after_help = BACKUP_HELP
    )]
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
enum RawAuthCommand {
    #[command(
        about = "Show auth and token status",
        long_about = "Show daemon URL, token resolution order, token file accessibility, and whether mutating commands are currently available.\nThis command never prints the token itself and never sends mutating requests.",
        after_help = AUTH_STATUS_HELP
    )]
    Status,
}

#[derive(Debug, Subcommand)]
enum RawMaintenanceCommand {
    #[command(
        about = "Set maintenance mode for a service",
        long_about = "Set maintenance mode for a service through the daemon remediation API.\nA non-empty reason is required and is forwarded to the daemon unchanged.",
        after_help = MAINTENANCE_SET_HELP
    )]
    Set {
        #[arg(value_name = "service")]
        service: String,
        #[arg(long, value_name = "text")]
        reason: String,
    },
    #[command(
        about = "Clear maintenance mode for a service",
        long_about = "Clear maintenance mode for a service through the daemon remediation API.",
        after_help = MAINTENANCE_CLEAR_HELP
    )]
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
    #[command(
        about = "Run backup now",
        long_about = "Trigger a backup through the daemon's existing trigger path.",
        after_help = BACKUP_RUN_HELP
    )]
    Run,
    #[command(
        about = "Unlock backup backend",
        long_about = "Request a backup backend unlock through the daemon remediation API.",
        after_help = BACKUP_UNLOCK_HELP
    )]
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
        RawCommand::Auth(RawAuthCommand::Status) => Ok(Command::AuthStatus),
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

const TRIGGER_HELP: &str = "Common task names:\n  recovery\n  backup\n\nExamples:\n  cratonctl trigger recovery --token-file /var/lib/craton/remediation-token\n  cratonctl trigger backup --token \"$CRATONCTL_TOKEN\"\n\nNotes:\n  This command requires mutating auth.\n  The daemon remains the source of truth for accepted task names.";

const MAINTENANCE_HELP: &str = "Examples:\n  cratonctl maintenance set ntfy --reason \"manual investigation\"\n  cratonctl maintenance clear ntfy\n\nNotes:\n  Maintenance commands require mutating auth.\n  The CLI does not manage maintenance duration locally.";
const MAINTENANCE_SET_HELP: &str = "Examples:\n  cratonctl maintenance set ntfy --reason \"manual investigation\"\n  cratonctl maintenance set api --reason \"planned restart\"\n\nNotes:\n  This command requires mutating auth.\n  The reason must be non-empty.";
const MAINTENANCE_CLEAR_HELP: &str = "Examples:\n  cratonctl maintenance clear ntfy\n\nNotes:\n  This command requires mutating auth.";

const BACKUP_HELP: &str = "Examples:\n  cratonctl backup run --token-file /var/lib/craton/remediation-token\n  cratonctl backup unlock --token \"$CRATONCTL_TOKEN\"\n\nNotes:\n  Both commands require mutating auth.\n  `run` goes through the daemon trigger path; `unlock` goes through remediation.";
const BACKUP_RUN_HELP: &str = "Examples:\n  cratonctl backup run --token-file /var/lib/craton/remediation-token\n  cratonctl backup run --token \"$CRATONCTL_TOKEN\"\n\nNotes:\n  This command requires mutating auth.\n  It uses the daemon trigger path rather than touching backup state directly.";
const BACKUP_UNLOCK_HELP: &str = "Examples:\n  cratonctl backup unlock --token-file /var/lib/craton/remediation-token\n  cratonctl backup unlock --token \"$CRATONCTL_TOKEN\"\n\nNotes:\n  This command requires mutating auth.\n  It requests a daemon-side remediation action.";

const DOCTOR_HELP: &str = "Checks:\n  daemon reachability\n  GET /health\n  GET /api/v1/state\n  token file accessibility\n  read-only and mutating readiness\n\nExamples:\n  cratonctl doctor\n  cratonctl --json doctor\n  cratonctl --url http://127.0.0.1:18800 doctor\n\nNotes:\n  `doctor` never sends mutating requests.\n  Use it as a safe preflight before restart/maintenance/backup actions.";

const AUTH_HELP: &str = "Commands:\n  status    show URL, token source resolution, token file status, and mutating readiness\n\nExamples:\n  cratonctl auth status\n  cratonctl auth status --token-file /var/lib/craton/remediation-token\n  cratonctl --json auth status\n\nNotes:\n  This command never prints the token itself.\n  Use it when mutating commands are unexpectedly unavailable.";

const AUTH_STATUS_HELP: &str = "Report fields:\n  daemon URL\n  token resolution order\n  autodiscovery token path\n  token file existence/readability\n  mutating readiness and explanation\n\nExamples:\n  cratonctl auth status\n  cratonctl auth status --token-file /var/lib/craton/remediation-token\n  cratonctl auth status --token \"$CRATONCTL_TOKEN\"\n  cratonctl --json auth status\n\nNotes:\n  This is a read-only report.\n  The token value is never printed.";

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
    fn parses_auth_status_command() {
        let cli = parse_ok(&["cratonctl", "auth", "status"]);
        assert_eq!(cli.command, Command::AuthStatus);
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

    #[test]
    fn doctor_help_mentions_preflight_checks() {
        let cli = parse_ok(&["cratonctl", "doctor", "--help"]);
        let Command::Help { text, top_level } = cli.command else {
            panic!("expected help command");
        };
        assert!(!top_level);
        assert!(text.contains("token file accessibility"));
        assert!(text.contains("never sends mutating requests"));
    }

    #[test]
    fn trigger_help_mentions_common_tasks() {
        let cli = parse_ok(&["cratonctl", "trigger", "--help"]);
        let Command::Help { text, top_level } = cli.command else {
            panic!("expected help command");
        };
        assert!(!top_level);
        assert!(text.contains("Common task names:"));
        assert!(text.contains("recovery"));
        assert!(text.contains("backup"));
    }

    #[test]
    fn auth_help_mentions_secret_safety() {
        let cli = parse_ok(&["cratonctl", "auth", "--help"]);
        let Command::Help { text, top_level } = cli.command else {
            panic!("expected help command");
        };
        assert!(!top_level);
        assert!(text.contains("never prints the token itself"));
        assert!(text.contains("mutating readiness"));
    }

    #[test]
    fn auth_status_help_mentions_read_only_secret_safe_report() {
        let cli = parse_ok(&["cratonctl", "auth", "status", "--help"]);
        let Command::Help { text, top_level } = cli.command else {
            panic!("expected help command");
        };
        assert!(!top_level);
        assert!(text.contains("never prints the token itself"));
        assert!(text.contains("This is a read-only report."));
        assert!(text.contains("mutating readiness and explanation"));
    }

    #[test]
    fn maintenance_set_help_mentions_reason_and_auth() {
        let cli = parse_ok(&["cratonctl", "maintenance", "set", "--help"]);
        let Command::Help { text, top_level } = cli.command else {
            panic!("expected help command");
        };
        assert!(!top_level);
        assert!(text.contains("requires mutating auth"));
        assert!(text.contains("reason must be non-empty"));
    }

    #[test]
    fn backup_run_help_mentions_trigger_path_and_auth() {
        let cli = parse_ok(&["cratonctl", "backup", "run", "--help"]);
        let Command::Help { text, top_level } = cli.command else {
            panic!("expected help command");
        };
        assert!(!top_level);
        assert!(text.contains("requires mutating auth"));
        assert!(text.contains("daemon trigger path"));
    }
}
