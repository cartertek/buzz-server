use std::{collections::BTreeMap, env, path::PathBuf, str::FromStr, time::Duration};

use buzz_server::api::{
    AddCommunityRequest, AgentCommandRequest, AgentLogsRequest, ChangeAgentStateRequest,
    CommandMetadata, CreateAgentInput, CreateAgentRequest, LifecycleRouteRequest,
    ListAgentsRequest, UpdateAgentInput, UpdateAgentRequest, UpdateCommunityRequest,
};
use buzz_server::{AgentId, CommunityConfigId, DesiredAgentState};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::timeout,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let mut socket = PathBuf::from("/run/buzz-server/lifecycle.sock");
    let first = arguments.next().ok_or_else(usage)?;
    let command = if first == "--socket" {
        socket = arguments.next().map(PathBuf::from).ok_or_else(usage)?;
        arguments.next().ok_or_else(usage)?
    } else {
        first
    };
    let values = parse_options(arguments.collect())?;
    let request = route(&command, &values)?;
    let mut value = send_request(&socket, &request).await?;
    if wire_error(&value) {
        print_value(&value)?;
        std::process::exit(1);
    }

    if is_mutating_command(&command) {
        let operation_id = operation_field(&value, "id")
            .ok_or("mutation response did not contain an operation ID")?
            .to_owned();
        let initial_status = operation_field(&value, "status")
            .ok_or("operation response did not contain a status")?;
        if matches!(initial_status, "pending" | "running") {
            value = send_request(
                &socket,
                &LifecycleRouteRequest::AwaitOperation {
                    operation_id: parse(&operation_id, "operation ID")?,
                },
            )
            .await
            .map_err(|error| {
                format!(
                    "operation {operation_id} completion wait failed: {error}; inspect it with `buzz-server agents operation --operation {operation_id}`"
                )
            })?;
            if wire_error(&value) {
                print_value(&value)?;
                std::process::exit(1);
            }
        }
        let status = operation_field(&value, "status")
            .ok_or("operation response did not contain a status")?;
        match status {
            "succeeded" => {}
            "failed" | "cancelled" => {
                print_value(&value)?;
                std::process::exit(1);
            }
            "pending" | "running" => {
                return Err(format!(
                    "operation {operation_id} did not finish within the server wait window; inspect it with `buzz-server agents operation --operation {operation_id}`"
                ));
            }
            other => return Err(format!("unknown operation status: {other}")),
        }
        if command != "purge" {
            if let Some(agent_id) = operation_field(&value, "agent_id") {
                value = send_request(
                    &socket,
                    &LifecycleRouteRequest::GetAgent {
                        agent_id: parse(agent_id, "agent ID")?,
                    },
                )
                .await?;
                if wire_error(&value) {
                    print_value(&value)?;
                    std::process::exit(1);
                }
            }
        }
    }

    if command == "community-relay" {
        let relay = value
            .pointer("/value/value/relay_url")
            .and_then(serde_json::Value::as_str)
            .ok_or("community response did not contain relay_url")?;
        println!("{relay}");
        return Ok(());
    }

    print_value(&value)
}

async fn send_request(
    socket: &PathBuf,
    request: &LifecycleRouteRequest,
) -> Result<serde_json::Value, String> {
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    const IO_TIMEOUT: Duration = Duration::from_secs(10);
    const COMPLETION_TIMEOUT: Duration = Duration::from_secs(125);

    let body = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    let mut stream = timeout(CONNECT_TIMEOUT, UnixStream::connect(socket))
        .await
        .map_err(|_| format!("timed out connecting to {}", socket.display()))?
        .map_err(|error| format!("cannot connect to {}: {error}", socket.display()))?;
    timeout(IO_TIMEOUT, async {
        stream
            .write_u32(u32::try_from(body.len()).map_err(|_| "request is too large")?)
            .await
            .map_err(|error| error.to_string())?;
        stream
            .write_all(&body)
            .await
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "timed out sending lifecycle request".to_owned())??;

    let response_timeout = if matches!(request, LifecycleRouteRequest::AwaitOperation { .. }) {
        COMPLETION_TIMEOUT
    } else {
        IO_TIMEOUT
    };
    let length = timeout(response_timeout, stream.read_u32())
        .await
        .map_err(|_| "timed out waiting for lifecycle response".to_owned())?
        .map_err(|error| error.to_string())? as usize;
    if length > buzz_server::transport::MAX_LIFECYCLE_RESPONSE_BYTES {
        return Err("server response exceeds the lifecycle limit".into());
    }
    let mut response = vec![0; length];
    timeout(IO_TIMEOUT, stream.read_exact(&mut response))
        .await
        .map_err(|_| "timed out reading lifecycle response".to_owned())?
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&response).map_err(|error| error.to_string())
}

fn print_value(value: &serde_json::Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn wire_error(value: &serde_json::Value) -> bool {
    value.get("status").and_then(serde_json::Value::as_str) == Some("error")
}

fn operation_field<'a>(value: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    (value.get("status")?.as_str()? == "ok").then_some(())?;
    (value.pointer("/value/resource")?.as_str()? == "operation").then_some(())?;
    value.pointer(&format!("/value/value/{field}"))?.as_str()
}

fn is_mutating_command(command: &str) -> bool {
    matches!(
        command,
        "create" | "update" | "enable" | "disable" | "delete" | "purge"
    )
}

fn parse_options(arguments: Vec<String>) -> Result<BTreeMap<String, String>, String> {
    let mut options = BTreeMap::new();
    let mut arguments = arguments.into_iter();
    while let Some(name) = arguments.next() {
        if !name.starts_with("--") {
            return Err(format!("unexpected argument {name}\n{}", usage()));
        }
        let value = if name == "--include-deleted" {
            "true".to_owned()
        } else {
            arguments
                .next()
                .ok_or_else(|| format!("missing value for {name}"))?
        };
        if options.insert(name, value).is_some() {
            return Err("an option was provided more than once".into());
        }
    }
    Ok(options)
}

fn route(
    command: &str,
    options: &BTreeMap<String, String>,
) -> Result<LifecycleRouteRequest, String> {
    let agent_id = || parse::<AgentId>(required(options, "--agent")?, "agent ID");
    let metadata = || -> Result<CommandMetadata, String> {
        let id = uuid::Uuid::now_v7();
        Ok(CommandMetadata {
            idempotency_key: options
                .get("--idempotency")
                .cloned()
                .unwrap_or_else(|| format!("cli-{command}-{id}")),
            correlation_id: options
                .get("--correlation")
                .cloned()
                .unwrap_or_else(|| format!("cli-{id}")),
        })
    };
    match command {
        "community-add" => Ok(LifecycleRouteRequest::AddCommunity(AddCommunityRequest {
            display_name: required(options, "--display-name")?.into(),
            relay_url: parse(required(options, "--relay-url")?, "relay URL")?,
        })),
        "community-update" => Ok(LifecycleRouteRequest::UpdateCommunity(
            UpdateCommunityRequest {
                community_id: parse::<CommunityConfigId>(
                    required(options, "--community")?,
                    "community ID",
                )?,
                display_name: required(options, "--display-name")?.into(),
            },
        )),
        "community-get" | "community-relay" => Ok(LifecycleRouteRequest::GetCommunity {
            community_id: parse::<CommunityConfigId>(
                required(options, "--community")?,
                "community ID",
            )?,
        }),
        "community-list" => Ok(LifecycleRouteRequest::ListCommunities),
        "community-delete" | "community-remove" => Ok(LifecycleRouteRequest::RemoveCommunity {
            community_id: parse::<CommunityConfigId>(
                required(options, "--community")?,
                "community ID",
            )?,
        }),
        "create" => Ok(LifecycleRouteRequest::CreateAgent(CreateAgentRequest {
            metadata: metadata()?,
            agent: create_input(options)?,
        })),
        "get" => Ok(LifecycleRouteRequest::GetAgent {
            agent_id: agent_id()?,
        }),
        "list" => Ok(LifecycleRouteRequest::ListAgents(ListAgentsRequest {
            community_config_id: optional_parse(options, "--community", "community ID")?,
            include_deleted: options.contains_key("--include-deleted"),
        })),
        "update" => Ok(LifecycleRouteRequest::UpdateAgent(UpdateAgentRequest {
            metadata: metadata()?,
            agent_id: agent_id()?,
            changes: UpdateAgentInput {
                display_name: options.get("--display-name").cloned(),
                system_prompt: options.get("--system-prompt").cloned(),
                runtime_id: optional_parse(options, "--runtime", "runtime ID")?,
            },
        })),
        "enable" | "disable" => Ok(LifecycleRouteRequest::ChangeAgentState(
            ChangeAgentStateRequest {
                metadata: metadata()?,
                agent_id: agent_id()?,
                desired_state: if command == "enable" {
                    DesiredAgentState::Enabled
                } else {
                    DesiredAgentState::Disabled
                },
            },
        )),
        "logs" => Ok(LifecycleRouteRequest::AgentLogs(AgentLogsRequest {
            agent_id: agent_id()?,
            after_cursor: options.get("--after").cloned(),
            limit: options
                .get("--limit")
                .map_or(Ok(100), |value| parse(value, "log limit"))?,
        })),
        "delete" | "purge" => {
            let request = AgentCommandRequest {
                metadata: metadata()?,
                agent_id: agent_id()?,
            };
            Ok(if command == "delete" {
                LifecycleRouteRequest::DeleteAgent(request)
            } else {
                LifecycleRouteRequest::PurgeAgent(request)
            })
        }
        "operation" => Ok(LifecycleRouteRequest::GetOperation {
            operation_id: parse(required(options, "--operation")?, "operation ID")?,
        }),
        _ => Err(usage()),
    }
}

fn create_input(options: &BTreeMap<String, String>) -> Result<CreateAgentInput, String> {
    Ok(CreateAgentInput {
        community_config_id: parse(required(options, "--community")?, "community ID")?,
        display_name: required(options, "--display-name")?.into(),
        system_prompt: required(options, "--system-prompt")?.into(),
        runtime_id: parse(required(options, "--runtime")?, "runtime ID")?,
    })
}

fn required<'a>(options: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, String> {
    options
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("missing required option {name}"))
}

fn optional_parse<T: FromStr>(
    options: &BTreeMap<String, String>,
    name: &str,
    description: &str,
) -> Result<Option<T>, String> {
    options
        .get(name)
        .map(|value| parse(value, description))
        .transpose()
}

fn parse<T: FromStr>(value: &str, description: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {description}: {value}"))
}

fn usage() -> String {
    "usage: buzz-agentctl [--socket PATH] <community-add|community-update|community-get|community-list|community-delete|community-relay|create|get|list|update|enable|disable|logs|delete|purge|operation> [--name value ...]".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_routes_are_typed_and_machine_serializable() {
        let agent = AgentId::new();
        let operation = buzz_server::OperationId::new();
        let agent = agent.to_string();
        let operation = operation.to_string();
        let cases = [
            ("get", vec!["--agent", agent.as_str()]),
            ("operation", vec!["--operation", operation.as_str()]),
            ("list", Vec::new()),
        ];
        for (command, words) in cases {
            let options = parse_options(words.into_iter().map(str::to_owned).collect()).unwrap();
            serde_json::to_vec(&route(command, &options).unwrap()).unwrap();
        }
    }
}
