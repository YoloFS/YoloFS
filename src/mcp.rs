use crate::changes::{ChangeSummary, commit_changes_silent, summarize_changes};
use crate::executor::{Sandbox, destroy_sandbox, run_shell_command_in_sandbox};
use anyhow::{Context, Result};
use rmcp::schemars::JsonSchema;
use rmcp::{
    Json, Peer, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CreateElicitationRequestParams, ElicitationAction, ElicitationSchema, ServerCapabilities,
        ServerInfo,
    },
    service::ElicitationMode,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ShellRequest {
    pub command: String,
    pub cwd: Option<String>,
    pub sandbox_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListChangesRequest {
    pub cwd: Option<String>,
    pub sandbox_dir: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeAction {
    Commit,
    Abort,
    Stage,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DecideChangesRequest {
    pub decision: StagedChangesDecision,
    pub cwd: Option<String>,
    pub sandbox_dir: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StagedChangesDecision {
    Abort,
    Stage,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommitChangesRequest {
    pub cwd: Option<String>,
    pub sandbox_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ShellResponse {
    pub cwd: String,
    pub sandbox_dir: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub changed_files: Vec<ChangeSummary>,
    pub decision_required: bool,
    pub available_actions: Vec<ChangeAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListChangesResponse {
    pub cwd: String,
    pub sandbox_dir: String,
    pub changed_files: Vec<ChangeSummary>,
    pub decision_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DecideChangesResponse {
    pub cwd: String,
    pub sandbox_dir: String,
    pub action: ChangeAction,
    pub previous_changed_files: Vec<ChangeSummary>,
    pub remaining_changed_files: Vec<ChangeSummary>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommitChangesResponse {
    pub cwd: String,
    pub sandbox_dir: String,
    pub committed: bool,
    pub previous_changed_files: Vec<ChangeSummary>,
    pub remaining_changed_files: Vec<ChangeSummary>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct AgfsMcpServer {
    default_cwd: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl AgfsMcpServer {
    pub fn new(default_cwd: PathBuf) -> Self {
        Self {
            default_cwd,
            tool_router: Self::tool_router(),
        }
    }

    fn resolve_cwd(&self, cwd: Option<&str>) -> Result<PathBuf> {
        let cwd = match cwd {
            Some(path) => {
                let path = PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    self.default_cwd.join(path)
                }
            }
            None => self.default_cwd.clone(),
        };

        cwd.canonicalize()
            .with_context(|| format!("Failed to resolve cwd {}", cwd.display()))
    }

    fn resolve_sandbox_dir(&self, cwd: &Path, sandbox_dir: Option<&str>) -> PathBuf {
        match sandbox_dir {
            Some(path) => {
                let path = PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                }
            }
            None => cwd.join(".staging"),
        }
    }

    fn open_sandbox(
        &self,
        cwd: &Path,
        sandbox_dir: Option<&str>,
        create: bool,
    ) -> Result<Option<Sandbox>> {
        let sandbox_dir = self.resolve_sandbox_dir(cwd, sandbox_dir);
        if !create && !sandbox_dir.exists() {
            return Ok(None);
        }

        Sandbox::new_at(sandbox_dir).map(Some)
    }

    fn build_commit_confirmation_message(
        &self,
        cwd: &Path,
        sandbox: &Sandbox,
        changed_files: &[ChangeSummary],
    ) -> String {
        let mut message = format!(
            "Allow agfs to commit {} staged change{} into `{}`?\nThis writes the staged files from `{}` into the real workspace.",
            changed_files.len(),
            if changed_files.len() == 1 { "" } else { "s" },
            cwd.display(),
            sandbox.root.display(),
        );

        if !changed_files.is_empty() {
            message.push_str("\n\nFiles:");
            for change in changed_files.iter().take(5) {
                let path = change
                    .cwd_relative_path
                    .as_deref()
                    .unwrap_or(change.path.as_str());
                message.push_str("\n- ");
                message.push_str(path);
            }
            if changed_files.len() > 5 {
                message.push_str(&format!("\n- ... and {} more", changed_files.len() - 5));
            }
        }

        message
    }

    async fn request_commit_confirmation(
        &self,
        peer: &Peer<RoleServer>,
        cwd: &Path,
        sandbox: &Sandbox,
        changed_files: &[ChangeSummary],
    ) -> Result<bool, String> {
        if !peer
            .supported_elicitation_modes()
            .contains(&ElicitationMode::Form)
        {
            return Err("MCP client does not support elicitation for commit approval.".into());
        }

        let requested_schema = ElicitationSchema::builder()
            .build()
            .map_err(|err| format!("Failed to build commit approval schema: {err}"))?;
        let message = self.build_commit_confirmation_message(cwd, sandbox, changed_files);
        let response = peer
            .create_elicitation(CreateElicitationRequestParams::FormElicitationParams {
                meta: None,
                message,
                requested_schema,
            })
            .await
            .map_err(|err| format!("Failed to request commit approval: {err}"))?;

        Ok(matches!(response.action, ElicitationAction::Accept))
    }
}

#[tool_router]
impl AgfsMcpServer {
    #[tool(
        name = "shell",
        description = "Run a shell command inside an agfs sandbox. The command result includes the current staged files so the agent can decide whether to commit, abort, or keep staging."
    )]
    async fn shell(
        &self,
        Parameters(request): Parameters<ShellRequest>,
    ) -> Result<Json<ShellResponse>, String> {
        if request.command.trim().is_empty() {
            return Err("command must not be empty".into());
        }

        let cwd = self
            .resolve_cwd(request.cwd.as_deref())
            .map_err(|err| err.to_string())?;
        let sandbox = self
            .open_sandbox(&cwd, request.sandbox_dir.as_deref(), true)
            .map_err(|err| err.to_string())?
            .expect("sandbox is always created");

        let result = run_shell_command_in_sandbox(&sandbox, &cwd, &request.command)
            .map_err(|err| err.to_string())?;
        let changed_files = summarize_changes(&sandbox, &cwd).map_err(|err| err.to_string())?;

        Ok(Json(ShellResponse {
            cwd: cwd.display().to_string(),
            sandbox_dir: sandbox.root.display().to_string(),
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
            decision_required: !changed_files.is_empty(),
            available_actions: if changed_files.is_empty() {
                Vec::new()
            } else {
                vec![
                    ChangeAction::Commit,
                    ChangeAction::Abort,
                    ChangeAction::Stage,
                ]
            },
            changed_files,
        }))
    }

    #[tool(
        name = "list_staged_changes",
        description = "List the files currently staged in an agfs sandbox without running a command."
    )]
    async fn list_staged_changes(
        &self,
        Parameters(request): Parameters<ListChangesRequest>,
    ) -> Result<Json<ListChangesResponse>, String> {
        let cwd = self
            .resolve_cwd(request.cwd.as_deref())
            .map_err(|err| err.to_string())?;
        let sandbox_dir = self.resolve_sandbox_dir(&cwd, request.sandbox_dir.as_deref());
        let changed_files = match self
            .open_sandbox(&cwd, request.sandbox_dir.as_deref(), false)
            .map_err(|err| err.to_string())?
        {
            Some(sandbox) => summarize_changes(&sandbox, &cwd).map_err(|err| err.to_string())?,
            None => Vec::new(),
        };

        Ok(Json(ListChangesResponse {
            cwd: cwd.display().to_string(),
            sandbox_dir: sandbox_dir.display().to_string(),
            decision_required: !changed_files.is_empty(),
            changed_files,
        }))
    }

    #[tool(
        name = "decide_changes",
        description = "Apply a non-commit decision to the current agfs staging area: abort the staged changes or keep them staged."
    )]
    async fn decide_changes(
        &self,
        Parameters(request): Parameters<DecideChangesRequest>,
    ) -> Result<Json<DecideChangesResponse>, String> {
        let cwd = self
            .resolve_cwd(request.cwd.as_deref())
            .map_err(|err| err.to_string())?;
        let sandbox_dir = self.resolve_sandbox_dir(&cwd, request.sandbox_dir.as_deref());
        let Some(sandbox) = self
            .open_sandbox(&cwd, request.sandbox_dir.as_deref(), false)
            .map_err(|err| err.to_string())?
        else {
            return Ok(Json(DecideChangesResponse {
                cwd: cwd.display().to_string(),
                sandbox_dir: sandbox_dir.display().to_string(),
                action: match request.decision {
                    StagedChangesDecision::Abort => ChangeAction::Abort,
                    StagedChangesDecision::Stage => ChangeAction::Stage,
                },
                previous_changed_files: Vec::new(),
                remaining_changed_files: Vec::new(),
                message: "No staged changes found.".into(),
            }));
        };

        let previous_changed_files =
            summarize_changes(&sandbox, &cwd).map_err(|err| err.to_string())?;
        if previous_changed_files.is_empty() {
            return Ok(Json(DecideChangesResponse {
                cwd: cwd.display().to_string(),
                sandbox_dir: sandbox.root.display().to_string(),
                action: match request.decision {
                    StagedChangesDecision::Abort => ChangeAction::Abort,
                    StagedChangesDecision::Stage => ChangeAction::Stage,
                },
                previous_changed_files,
                remaining_changed_files: Vec::new(),
                message: "No staged changes found.".into(),
            }));
        }

        let message = match request.decision {
            StagedChangesDecision::Abort => {
                destroy_sandbox(&sandbox).map_err(|err| err.to_string())?;
                "Aborted staged changes and removed the sandbox.".to_string()
            }
            StagedChangesDecision::Stage => "Kept staged changes in the sandbox.".to_string(),
        };

        let remaining_changed_files = match request.decision {
            StagedChangesDecision::Stage => {
                summarize_changes(&sandbox, &cwd).map_err(|err| err.to_string())?
            }
            StagedChangesDecision::Abort => Vec::new(),
        };

        Ok(Json(DecideChangesResponse {
            cwd: cwd.display().to_string(),
            sandbox_dir: sandbox.root.display().to_string(),
            action: match request.decision {
                StagedChangesDecision::Abort => ChangeAction::Abort,
                StagedChangesDecision::Stage => ChangeAction::Stage,
            },
            previous_changed_files,
            remaining_changed_files,
            message,
        }))
    }

    #[tool(
        name = "commit_changes",
        description = "Commit the current agfs staged changes into the real workspace.",
        annotations(
            title = "Commit staged changes",
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = false,
            idempotent_hint = false
        )
    )]
    async fn commit_changes(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<CommitChangesRequest>,
    ) -> Result<Json<CommitChangesResponse>, String> {
        let cwd = self
            .resolve_cwd(request.cwd.as_deref())
            .map_err(|err| err.to_string())?;
        let sandbox_dir = self.resolve_sandbox_dir(&cwd, request.sandbox_dir.as_deref());
        let Some(sandbox) = self
            .open_sandbox(&cwd, request.sandbox_dir.as_deref(), false)
            .map_err(|err| err.to_string())?
        else {
            return Ok(Json(CommitChangesResponse {
                cwd: cwd.display().to_string(),
                sandbox_dir: sandbox_dir.display().to_string(),
                committed: false,
                previous_changed_files: Vec::new(),
                remaining_changed_files: Vec::new(),
                message: "No staged changes found.".into(),
            }));
        };

        let previous_changed_files =
            summarize_changes(&sandbox, &cwd).map_err(|err| err.to_string())?;
        if previous_changed_files.is_empty() {
            return Ok(Json(CommitChangesResponse {
                cwd: cwd.display().to_string(),
                sandbox_dir: sandbox.root.display().to_string(),
                committed: false,
                previous_changed_files,
                remaining_changed_files: Vec::new(),
                message: "No staged changes found.".into(),
            }));
        }

        if !self
            .request_commit_confirmation(&peer, &cwd, &sandbox, &previous_changed_files)
            .await?
        {
            return Ok(Json(CommitChangesResponse {
                cwd: cwd.display().to_string(),
                sandbox_dir: sandbox.root.display().to_string(),
                committed: false,
                previous_changed_files: previous_changed_files.clone(),
                remaining_changed_files: previous_changed_files,
                message: "Commit was not approved. Kept staged changes in the sandbox.".into(),
            }));
        }

        commit_changes_silent(&sandbox).map_err(|err| err.to_string())?;

        Ok(Json(CommitChangesResponse {
            cwd: cwd.display().to_string(),
            sandbox_dir: sandbox.root.display().to_string(),
            committed: true,
            previous_changed_files,
            remaining_changed_files: Vec::new(),
            message: "Committed staged changes.".to_string(),
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AgfsMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Use `shell` to run a command. Inspect `changed_files`, then call `decide_changes` with `abort` or `stage`, or call `commit_changes` to request user approval before writing the staged changes into the workspace.",
        )
    }
}

pub async fn serve_mcp() -> Result<()> {
    let server = AgfsMcpServer::new(std::env::current_dir()?);
    server.serve(stdio()).await?.waiting().await?;
    Ok(())
}
