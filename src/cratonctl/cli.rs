use crate::cratonctl::error::CratonctlError;

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
    Health,
    Status,
    Services,
    Service { id: String },
    History { kind: HistoryKind },
    Diagnose { service: String },
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
    let mut iter = args.into_iter();
    let _program = iter.next();
    let mut rest: Vec<String> = iter.collect();
    if rest.is_empty() {
        return Err(CratonctlError::Usage(usage()));
    }

    let mut global = GlobalArgs::default();
    let mut index = 0usize;
    while index < rest.len() {
        match rest[index].as_str() {
            "--url" => {
                let value = rest.get(index + 1).ok_or_else(|| {
                    CratonctlError::Usage("--url requires a value".into())
                })?;
                global.url = Some(value.clone());
                index += 2;
            }
            "--token" => {
                let value = rest.get(index + 1).ok_or_else(|| {
                    CratonctlError::Usage("--token requires a value".into())
                })?;
                global.token = Some(value.clone());
                index += 2;
            }
            "--token-file" => {
                let value = rest.get(index + 1).ok_or_else(|| {
                    CratonctlError::Usage("--token-file requires a value".into())
                })?;
                global.token_file = Some(value.clone());
                index += 2;
            }
            "--json" => {
                global.json = true;
                index += 1;
            }
            "--quiet" => {
                global.quiet = true;
                index += 1;
            }
            "--no-color" => {
                global.no_color = true;
                index += 1;
            }
            "--help" | "-h" => return Err(CratonctlError::Usage(usage())),
            value if value.starts_with("--") => {
                return Err(CratonctlError::Usage(format!("unknown flag: {value}")));
            }
            _ => break,
        }
    }

    rest.drain(0..index);
    let command = parse_command(&rest)?;
    Ok(Cli { global, command })
}

fn parse_command(args: &[String]) -> Result<Command, CratonctlError> {
    let Some(head) = args.first() else {
        return Err(CratonctlError::Usage(usage()));
    };

    match head.as_str() {
        "health" if args.len() == 1 => Ok(Command::Health),
        "status" if args.len() == 1 => Ok(Command::Status),
        "services" if args.len() == 1 => Ok(Command::Services),
        "service" if args.len() == 2 => Ok(Command::Service {
            id: args[1].clone(),
        }),
        "history" if args.len() == 2 => Ok(Command::History {
            kind: parse_history_kind(&args[1])?,
        }),
        "diagnose" if args.len() == 2 => Ok(Command::Diagnose {
            service: args[1].clone(),
        }),
        "trigger" if args.len() == 2 => Ok(Command::Trigger {
            task: args[1].clone(),
        }),
        "restart" if args.len() == 2 => Ok(Command::Restart {
            service: args[1].clone(),
        }),
        "maintenance" => parse_maintenance_command(args),
        "breaker" if args.len() == 3 && args[1] == "clear" => Ok(Command::BreakerClear {
            service: args[2].clone(),
        }),
        "flapping" if args.len() == 3 && args[1] == "clear" => Ok(Command::FlappingClear {
            service: args[2].clone(),
        }),
        "backup" if args.len() == 2 && args[1] == "run" => Ok(Command::BackupRun),
        "backup" if args.len() == 2 && args[1] == "unlock" => Ok(Command::BackupUnlock),
        "disk" if args.len() == 2 && args[1] == "cleanup" => Ok(Command::DiskCleanup),
        _ => Err(CratonctlError::Usage(usage())),
    }
}

fn parse_maintenance_command(args: &[String]) -> Result<Command, CratonctlError> {
    match args {
        [head, action, service] if head == "maintenance" && action == "clear" => {
            Ok(Command::MaintenanceClear {
                service: service.clone(),
            })
        }
        [head, action, service, flag, reason]
            if head == "maintenance" && action == "set" && flag == "--reason" =>
        {
            if reason.trim().is_empty() {
                return Err(CratonctlError::Usage(
                    "maintenance set requires a non-empty --reason".into(),
                ));
            }
            Ok(Command::MaintenanceSet {
                service: service.clone(),
                reason: reason.clone(),
            })
        }
        _ => Err(CratonctlError::Usage(usage())),
    }
}

fn parse_history_kind(value: &str) -> Result<HistoryKind, CratonctlError> {
    match value {
        "recovery" => Ok(HistoryKind::Recovery),
        "backup" => Ok(HistoryKind::Backup),
        "remediation" => Ok(HistoryKind::Remediation),
        _ => Err(CratonctlError::Usage(format!(
            "unknown history kind: {value}"
        ))),
    }
}

fn usage() -> String {
    "cratonctl [--url URL] [--token TOKEN] [--token-file PATH] [--json] [--quiet] [--no-color] <command>\ncommands: health | status | services | service <id> | history <recovery|backup|remediation> | diagnose <service> | trigger <task> | restart <service> | maintenance set <service> --reason <text> | maintenance clear <service> | breaker clear <service> | flapping clear <service> | backup run | backup unlock | disk cleanup".into()
}

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
}
