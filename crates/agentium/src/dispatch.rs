// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Command dispatch for `agentium`.

use crate::{
    cli::{
        Cli, Commands, ConfigCommands, EvalCommands, ExternalToolCommands, InstallCommands,
        McpCommands, SkillCommands,
    },
    commands::{
        self, new_agent::NewAgentCli, new_static_tool::NewStaticToolCli, new_tool::NewToolCli,
        utils::resolve_runner_token,
    },
    eval, serve, skills,
};

pub fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Serve { runner } => serve::run(runner),

        Commands::Init {
            dir,
            runner_url,
            agent_name,
            with_agent,
        } => commands::init::run(
            std::path::Path::new(&dir),
            &runner_url,
            agent_name.as_deref(),
            with_agent,
        ),

        Commands::Config { command } => match command {
            ConfigCommands::Show { config } => {
                let path = config.as_deref().map(std::path::Path::new);
                commands::config_cmd::show(path)
            }
            ConfigCommands::Set { key, value, config } => {
                let path = config.as_deref().map(std::path::Path::new);
                commands::config_cmd::set(&key, &value, path)
            }
        },

        Commands::Install { command } => match command {
            InstallCommands::Agent {
                path,
                repository_url,
                url,
                rationale,
                origin,
                runner_token,
            } => {
                let token = resolve_runner_token(runner_token.as_deref())?;
                commands::install::install_agent(
                    path.as_deref(),
                    None,
                    repository_url.as_deref(),
                    url.as_deref(),
                    &rationale,
                    origin,
                    token,
                )
            }
            InstallCommands::Tool {
                dir,
                repository_url,
                runner_token,
                sandbox_rootfs,
                approved_by,
                yes,
                json,
            } => commands::install::install_tool(
                &dir,
                repository_url.as_deref(),
                runner_token.as_deref(),
                sandbox_rootfs.as_deref(),
                approved_by.as_deref(),
                yes,
                json,
            ),
        },

        Commands::SyncTypes { path, runner_token } => {
            let token = resolve_runner_token(runner_token.as_deref())?;
            commands::sync_types::run(path.as_deref(), token)
        }

        Commands::Skill { command } => match command {
            SkillCommands::Install { kind, dest } => {
                skills::install(&kind, dest.as_deref().map(std::path::Path::new))
            }
        },

        Commands::Eval { command } => match command {
            EvalCommands::Init { path } => eval::init_eval_manifest(std::path::Path::new(&path)),
            EvalCommands::Run {
                manifest,
                url,
                model,
                min_pass_rate,
                cases,
                deploy,
                path,
                runner_token,
            } => {
                let token = resolve_runner_token(runner_token.as_deref())?;
                eval::run_eval(
                    eval::EvalRunOptions {
                        manifest_path: std::path::PathBuf::from(manifest),
                        runner_url: url,
                        model_override: model,
                        min_pass_rate,
                        case_filter: cases,
                        deploy,
                        agent_path: path.map(std::path::PathBuf::from),
                    },
                    token,
                )
                .map(|_| ())
            }
            EvalCommands::Report => eval::report_last_run(),
        },

        Commands::NewTool {
            name,
            bundle,
            lang,
            access,
            runtime,
            invocation_mode,
            sandbox_source,
            sandbox_image,
            sandbox_entrypoint,
            generate_docker,
            description,
            output,
            dry_run,
        } => commands::new_tool::run_cli(NewToolCli {
            name,
            bundle,
            lang,
            access,
            runtime,
            invocation_mode,
            sandbox_source,
            sandbox_image,
            sandbox_entrypoint,
            generate_docker,
            description,
            output,
            dry_run,
        }),

        Commands::NewStaticTool {
            name,
            bundle,
            access,
            description,
            dry_run,
        } => commands::new_static_tool::run_cli(NewStaticToolCli {
            name,
            bundle,
            access,
            description,
            dry_run,
        }),

        Commands::NewAgent {
            name,
            tools,
            template,
            description,
            tags,
            subscriptions,
            repository_url,
            snapshot_cache,
            output,
            dry_run,
        } => commands::new_agent::run_cli(NewAgentCli {
            name,
            tools,
            template,
            description,
            tags,
            subscriptions,
            repository_url,
            snapshot_cache,
            output,
            dry_run,
        }),

        Commands::ListTools {
            repository_url,
            snapshot_cache,
        } => commands::list_tools::run(&repository_url, snapshot_cache.as_deref()),

        Commands::ListAgents => commands::list_agents::run(),

        Commands::ListEventSources {
            repository_url,
            snapshot_cache,
        } => commands::list_event_sources::run(&repository_url, snapshot_cache.as_deref()),

        Commands::Publish {
            agent_dir,
            repository_url,
            rationale,
            origin,
            runner_token,
        } => {
            let token = resolve_runner_token(runner_token.as_deref())?;
            commands::publish::run(&agent_dir, &repository_url, &rationale, origin, token)
        }

        Commands::Deploy {
            hash,
            url,
            runner_token,
        } => {
            let token = resolve_runner_token(runner_token.as_deref())?;
            commands::deploy::run(&hash, &url, token)
        }

        Commands::Undeploy {
            hash,
            url,
            runner_token,
        } => {
            let token = resolve_runner_token(runner_token.as_deref())?;
            commands::undeploy::run(&hash, &url, token)
        }

        Commands::ListDeployedInstances { url } => commands::list_deployed_instances::run(&url),

        Commands::ExportSnapshotCache {
            repository_url,
            output,
        } => commands::export_snapshot_cache::run(&repository_url, &output),

        Commands::CheckExternalTool { path } => commands::check_external_tool::run(&path),

        Commands::SandboxBindSync {
            tool_dir,
            rootfs,
            dockerfile,
            image,
            force,
            check,
            dry_run,
            json,
        } => {
            commands::sandbox_bind_sync::run(commands::sandbox_bind_sync::SandboxBindSyncRunArgs {
                tool_dir: &tool_dir,
                rootfs: rootfs.as_deref(),
                dockerfile: dockerfile.as_deref(),
                image: image.as_deref(),
                force,
                check,
                dry_run,
                as_json: json,
            })
        }

        Commands::SandboxOciPrepare {
            tool_dir,
            output,
            check,
            dry_run,
            json,
        } => commands::sandbox_oci_prepare::run(
            commands::sandbox_oci_prepare::SandboxOciPrepareRunArgs {
                tool_dir: &tool_dir,
                output: output.as_deref(),
                check,
                dry_run,
                as_json: json,
            },
        ),

        Commands::SnapshotReport {
            snapshot_cache,
            json,
        } => commands::snapshot_report::run(&snapshot_cache, json),

        Commands::Doctor {
            ci,
            warn_missing_catalog,
            repository_url,
            snapshot_cache,
        } => commands::doctor::run(
            ci,
            warn_missing_catalog,
            repository_url.as_deref(),
            snapshot_cache.as_deref(),
        ),

        Commands::ExternalTool { command } => match command {
            ExternalToolCommands::Enable {
                dir,
                repository_url,
                runner_token,
                sandbox_rootfs,
                approved_by,
                yes,
                json,
            } => commands::external_tool::enable(commands::external_tool::EnableParams {
                dir: &dir,
                repository_url: repository_url.as_deref(),
                runner_token: runner_token.as_deref(),
                sandbox_rootfs: sandbox_rootfs.as_deref(),
                approved_by: approved_by.as_deref(),
                yes,
                json_output: json,
            }),
            ExternalToolCommands::Inspect {
                name,
                cache_dir,
                json,
            } => commands::external_tool::inspect(&name, cache_dir.as_deref(), json),
            ExternalToolCommands::Refresh {
                name,
                dir,
                repository_url,
                runner_token,
                yes,
                json,
            } => commands::external_tool::refresh(
                &name,
                &dir,
                repository_url.as_deref(),
                runner_token.as_deref(),
                yes,
                json,
            ),
        },

        Commands::Mcp { command } => match command {
            McpCommands::List {
                repository_url,
                json,
            } => commands::mcp::list(&repository_url, json),
            McpCommands::Enable {
                server_id,
                config,
                repository_url,
                yes,
                runner_token,
            } => {
                let token = resolve_runner_token(runner_token.as_deref())?;
                commands::mcp::enable(&server_id, config.as_deref(), &repository_url, yes, token)
            }
            McpCommands::Server {
                server_id,
                version,
                repository_url,
                json,
            } => commands::mcp::server(&server_id, version, &repository_url, json),
            McpCommands::Versions {
                server_id,
                repository_url,
                json,
            } => commands::mcp::versions(&server_id, &repository_url, json),
            McpCommands::Tool {
                platform_tool_name,
                repository_url,
                json,
            } => commands::mcp::tool(&platform_tool_name, &repository_url, json),
        },

        Commands::Chat {
            agent,
            url,
            instance,
            verbose,
        } => commands::chat::run(&agent, &url, &instance, verbose),
    }
}
