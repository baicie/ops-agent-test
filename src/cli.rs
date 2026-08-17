use std::{
    io::{self, Write},
    path::PathBuf,
};

use clap::{Parser, Subcommand};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::{
    OpsCodexError, Result,
    app::build_runtime,
    config::Config,
    runtime::{
        AgentRuntime, EventEnvelope, IncidentContext, RuntimeEvent, ThreadId, TurnId, TurnInput,
    },
    server::{ServerState, router_with_web},
};

#[derive(Debug, Parser)]
#[command(name = "opscodex", version, about = "Local-first AIOps agent runtime")]
pub struct Cli {
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,
    #[arg(long, global = true)]
    pub enable_exec: bool,
    #[arg(
        long,
        global = true,
        help = "Block all change operations regardless of existing approvals"
    )]
    pub kill_switch: bool,
    #[arg(
        long,
        global = true,
        help = "Use the deterministic local model provider"
    )]
    pub fake_model: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Run {
        #[arg(value_name = "PROMPT")]
        input: String,
        #[arg(long)]
        service: Option<String>,
        #[arg(long)]
        environment: Option<String>,
        #[arg(long)]
        starts_at: Option<String>,
        #[arg(long)]
        ends_at: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
    },
    Serve {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long, default_value = "web/dist")]
        web_dir: PathBuf,
    },
    Migrate {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        verify: bool,
    },
    Export {
        #[arg(long)]
        thread: String,
        #[arg(long)]
        out: PathBuf,
    },
    Doctor,
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Storage {
        #[command(subcommand)]
        command: StorageCommand,
    },
    Audit {
        #[command(subcommand)]
        command: AuditCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Validate,
}

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    Verify,
    Backup {
        #[arg(long)]
        out: PathBuf,
    },
    Export {
        #[arg(long)]
        thread: String,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuditCommand {
    Verify,
}

pub async fn execute(cli: Cli) -> Result<()> {
    let mut config = Config::load(cli.config.as_deref())?;
    if cli.enable_exec {
        config.tools.exec = true;
    }
    if cli.kill_switch {
        config.remediation.kill_switch = true;
    }
    match &cli.command {
        Command::Migrate { dry_run, verify } => {
            return migrate_store(&config, *dry_run, *verify).await;
        }
        Command::Export { thread, out } => {
            return export_thread(&config, thread, out).await;
        }
        Command::Doctor => {
            let report = crate::ops::doctor(&config).await?;
            crate::ops::print_json(&report)?;
            return if report.is_ok() {
                Ok(())
            } else {
                Err(OpsCodexError::Protocol(
                    "doctor found blocking errors".into(),
                ))
            };
        }
        Command::Config {
            command: ConfigCommand::Validate,
        } => {
            crate::ops::validate_config(&config)?;
            println!("ok");
            return Ok(());
        }
        Command::Storage { command } => {
            return match command {
                StorageCommand::Verify => {
                    let detail = crate::ops::verify_store(&config).await?;
                    println!("{detail}");
                    Ok(())
                }
                StorageCommand::Backup { out } => {
                    let path = crate::ops::backup_store(&config, out).await?;
                    println!("backup {}", path.display());
                    Ok(())
                }
                StorageCommand::Export { thread, out } => export_thread(&config, thread, out).await,
            };
        }
        Command::Audit {
            command: AuditCommand::Verify,
        } => {
            let detail = crate::ops::verify_audit(&config).await?;
            println!("{detail}");
            return Ok(());
        }
        _ => {}
    }
    config.validate()?;
    let runtime = build_runtime(&config, cli.fake_model).await?;
    match cli.command {
        Command::Run {
            input,
            service,
            environment,
            starts_at,
            ends_at,
            workspace,
        } => {
            let incident_context = incident_from_flags(service, environment, starts_at, ends_at)?;
            run(runtime, input, incident_context, workspace).await
        }
        Command::Serve {
            host,
            port,
            web_dir,
        } => {
            serve(
                runtime,
                host.unwrap_or(config.server.host),
                port.unwrap_or(config.server.port),
                web_dir,
            )
            .await
        }
        Command::Migrate { .. }
        | Command::Export { .. }
        | Command::Doctor
        | Command::Config { .. }
        | Command::Storage { .. }
        | Command::Audit { .. } => unreachable!(),
    }
}

async fn migrate_store(config: &Config, dry_run: bool, verify: bool) -> Result<()> {
    let data_dir = Config::data_dir();
    let jsonl_dir = config
        .store
        .jsonl_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("threads"));
    let sqlite_path = config
        .store
        .sqlite_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("state.sqlite3"));
    let report = crate::store::migrate_jsonl_to_sqlite(
        jsonl_dir,
        sqlite_path,
        crate::store::MigrateOptions { dry_run, verify },
    )
    .await?;
    println!(
        "migrated threads={} events={} hash={}",
        report.threads, report.events, report.hash
    );
    if let Some(backup) = report.backup_dir {
        println!("jsonl backup {}", backup.display());
    }
    Ok(())
}

async fn export_thread(config: &Config, thread: &str, out: &PathBuf) -> Result<()> {
    let data_dir = Config::data_dir();
    let sqlite_path = config
        .store
        .sqlite_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("state.sqlite3"));
    let store = crate::store::SqliteStore::open(sqlite_path).await?;
    let report = crate::store::export_thread_jsonl(&store, thread, out).await?;
    println!(
        "exported thread {} ({} events) to {}",
        report.thread_id,
        report.events,
        report.path.display()
    );
    Ok(())
}

async fn run(
    runtime: std::sync::Arc<AgentRuntime>,
    input: String,
    incident_context: Option<IncidentContext>,
    workspace: Option<String>,
) -> Result<()> {
    let workspace_id = workspace
        .map(crate::runtime::WorkspaceId::new)
        .unwrap_or_default();
    workspace_id.validate()?;
    if !runtime.workspaces().is_empty() {
        runtime.workspaces().require(&workspace_id)?;
    }
    let thread_id = ThreadId::new();
    runtime
        .store()
        .create_thread(thread_id.clone(), workspace_id)
        .await?;
    let turn_id = TurnId::new();
    let cancellation = CancellationToken::new();
    let (events, receiver) = broadcast::channel(256);
    let renderer = tokio::spawn(render_events(runtime.clone(), receiver));
    println!("> {input}\n");
    let turn = runtime.run_turn(
        thread_id,
        turn_id,
        TurnInput {
            content: input,
            incident_context,
        },
        events.clone(),
        cancellation.clone(),
    );
    tokio::pin!(turn);
    let result = tokio::select! {
        result = &mut turn => result,
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|error| OpsCodexError::Protocol(error.to_string()))?;
            cancellation.cancel();
            turn.await
        }
    };
    drop(events);
    let _ = renderer.await;
    result
}

async fn render_events(
    runtime: std::sync::Arc<AgentRuntime>,
    mut receiver: broadcast::Receiver<EventEnvelope>,
) {
    let mut streaming = false;
    while let Ok(envelope) = receiver.recv().await {
        match envelope.event {
            RuntimeEvent::AssistantDelta { delta } => {
                print!("{delta}");
                let _ = io::stdout().flush();
                streaming = true;
            }
            RuntimeEvent::AssistantCompleted { .. } if streaming => {
                println!("\n");
                streaming = false;
            }
            RuntimeEvent::ToolProposed { tool, .. } => {
                if tool.contains('/') {
                    println!("[tool] {tool} proposed (external)");
                } else {
                    println!("[tool] {tool} proposed");
                }
            }
            RuntimeEvent::ToolExecutionStarted { tool, .. } => println!("[tool] {tool} running"),
            RuntimeEvent::ToolStarted { tool, .. } => println!("[tool] {tool} running"),
            RuntimeEvent::ToolCompleted {
                tool,
                evidence,
                success,
                ..
            } => println!(
                "[tool] {tool} {} ({} ms)",
                if success { "completed" } else { "failed" },
                evidence.duration_ms
            ),
            RuntimeEvent::ApprovalRequired {
                approval_id,
                tool,
                arguments,
            } => {
                println!("[approval] {tool} {arguments}");
                let approved = tokio::task::spawn_blocking(|| {
                    print!("Allow? [y/N] ");
                    let _ = io::stdout().flush();
                    let mut answer = String::new();
                    io::stdin().read_line(&mut answer).is_ok()
                        && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
                })
                .await
                .unwrap_or(false);
                let _ = runtime.policy().broker().resolve(&approval_id, approved);
            }
            RuntimeEvent::UserMessage {
                incident_context: Some(context),
                ..
            } => {
                if let Some(service) = &context.service {
                    println!("[incident] service={service}");
                }
            }
            RuntimeEvent::TurnFailed { error } => eprintln!("turn failed: {error}"),
            RuntimeEvent::TurnCancelled => eprintln!("turn cancelled"),
            _ => {}
        }
    }
}

fn incident_from_flags(
    service: Option<String>,
    environment: Option<String>,
    starts_at: Option<String>,
    ends_at: Option<String>,
) -> Result<Option<IncidentContext>> {
    if service.is_none() && environment.is_none() && starts_at.is_none() && ends_at.is_none() {
        return Ok(None);
    }
    let parse_time = |value: Option<String>| -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        value
            .map(|value| {
                chrono::DateTime::parse_from_rfc3339(&value)
                    .map(|value| value.with_timezone(&chrono::Utc))
                    .map_err(|error| OpsCodexError::Protocol(format!("invalid timestamp: {error}")))
            })
            .transpose()
    };
    let context = IncidentContext {
        service,
        environment,
        starts_at: parse_time(starts_at)?,
        ends_at: parse_time(ends_at)?,
        ..IncidentContext::default()
    };
    context.validate()?;
    Ok(Some(context))
}

async fn serve(
    runtime: std::sync::Arc<AgentRuntime>,
    host: String,
    port: u16,
    web_directory: PathBuf,
) -> Result<()> {
    let address = crate::ops::parse_listen_addr(&host, port)?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| OpsCodexError::Protocol(format!("failed to bind {address}: {error}")))?;
    let actual = listener.local_addr().unwrap_or(address);
    println!("OpsCodex listening on http://{actual}");
    axum::serve(
        listener,
        router_with_web(ServerState::new(runtime), web_directory),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|error| OpsCodexError::Protocol(format!("server failed: {error}")))
}
