use crate::changes::{ChangeSummary, summarize_changes};
use crate::executor::{Sandbox, destroy_sandbox, run_shell_command_in_sandbox};
use anyhow::{Context, Result};
use rmcp::schemars::JsonSchema;
use rmcp::{
    Json, Peer, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CreateElicitationRequestParams, ElicitationAction, ElicitationSchema, EnumSchema,
        ServerCapabilities, ServerInfo,
    },
    serde_json,
    service::ElicitationMode,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

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
pub struct AskUserRequest {
    pub questions: Vec<AskUserQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskUserQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<AskUserOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskUserOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskUserResponse {
    pub answers: BTreeMap<String, AskUserAnswer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskUserAnswer {
    pub answers: Vec<String>,
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

    fn validate_ask_user_request(request: &AskUserRequest) -> Result<(), String> {
        if request.questions.is_empty() {
            return Err("ask_user requires at least one question.".into());
        }
        if request.questions.len() > 3 {
            return Err("ask_user supports at most three questions.".into());
        }

        let mut question_ids = HashSet::new();
        for question in &request.questions {
            if question.id.trim().is_empty() {
                return Err("ask_user question id must not be empty.".into());
            }
            if !question_ids.insert(question.id.as_str()) {
                return Err(format!(
                    "ask_user question id `{}` must be unique.",
                    question.id
                ));
            }
            if question.header.trim().is_empty() {
                return Err(format!(
                    "ask_user question `{}` must have a non-empty header.",
                    question.id
                ));
            }
            if question.header.chars().count() > 12 {
                return Err(format!(
                    "ask_user question `{}` header must be 12 characters or fewer.",
                    question.id
                ));
            }
            if question.question.trim().is_empty() {
                return Err(format!(
                    "ask_user question `{}` must have a non-empty prompt.",
                    question.id
                ));
            }
            if !(2..=3).contains(&question.options.len()) {
                return Err(format!(
                    "ask_user question `{}` must provide two or three options.",
                    question.id
                ));
            }

            let mut labels = HashSet::new();
            for option in &question.options {
                if option.label.trim().is_empty() {
                    return Err(format!(
                        "ask_user question `{}` has an empty option label.",
                        question.id
                    ));
                }
                if option.description.trim().is_empty() {
                    return Err(format!(
                        "ask_user question `{}` option `{}` must have a description.",
                        question.id, option.label
                    ));
                }
                if !labels.insert(option.label.as_str()) {
                    return Err(format!(
                        "ask_user question `{}` option labels must be unique.",
                        question.id
                    ));
                }
            }
        }

        Ok(())
    }

    fn build_ask_user_message(questions: &[AskUserQuestion]) -> String {
        let mut message = String::from("Answer the following questions to continue.");

        for question in questions {
            message.push_str("\n\n");
            message.push_str(&question.header);
            message.push_str(": ");
            message.push_str(&question.question);

            for option in &question.options {
                message.push_str("\n- ");
                message.push_str(&option.label);
                message.push_str(": ");
                message.push_str(&option.description);
            }
        }

        message
    }

    fn build_ask_user_schema(questions: &[AskUserQuestion]) -> Result<ElicitationSchema, String> {
        let mut schema = ElicitationSchema::builder()
            .title("Ask user")
            .description("Answer the selected options for each question.");

        for question in questions {
            let option_labels = question
                .options
                .iter()
                .map(|option| option.label.clone())
                .collect::<Vec<_>>();
            let option_titles = question
                .options
                .iter()
                .map(|option| format!("{}: {}", option.label, option.description))
                .collect::<Vec<_>>();

            let enum_schema = EnumSchema::builder(option_labels)
                .title(question.header.clone())
                .description(question.question.clone())
                .enum_titles(option_titles)
                .map_err(|err| {
                    format!(
                        "Failed to build ask_user schema for `{}`: {err}",
                        question.id
                    )
                })?
                .build();

            schema = schema.required_enum_schema(question.id.clone(), enum_schema);
        }

        schema
            .build()
            .map_err(|err| format!("Failed to build ask_user schema: {err}"))
    }

    async fn request_user_answers(
        peer: &Peer<RoleServer>,
        request: &AskUserRequest,
    ) -> Result<AskUserResponse, String> {
        if !peer
            .supported_elicitation_modes()
            .contains(&ElicitationMode::Form)
        {
            return Err("MCP client does not support form elicitation for ask_user.".into());
        }

        let requested_schema = Self::build_ask_user_schema(&request.questions)?;
        let message = Self::build_ask_user_message(&request.questions);
        let response = peer
            .create_elicitation(CreateElicitationRequestParams::FormElicitationParams {
                meta: None,
                message,
                requested_schema,
            })
            .await
            .map_err(|err| format!("Failed to request ask_user input: {err}"))?;

        match response.action {
            ElicitationAction::Accept => {
                let Some(content) = response.content else {
                    return Err("ask_user did not return any answers.".into());
                };
                let raw_answers = serde_json::from_value::<BTreeMap<String, String>>(content)
                    .map_err(|err| format!("Failed to parse ask_user answers: {err}"))?;

                let mut answers = BTreeMap::new();
                for question in &request.questions {
                    let Some(answer) = raw_answers.get(&question.id) else {
                        return Err(format!(
                            "ask_user response was missing an answer for `{}`.",
                            question.id
                        ));
                    };

                    if !question
                        .options
                        .iter()
                        .any(|option| option.label == *answer)
                    {
                        return Err(format!(
                            "ask_user response for `{}` was not one of the provided options.",
                            question.id
                        ));
                    }

                    answers.insert(
                        question.id.clone(),
                        AskUserAnswer {
                            answers: vec![answer.clone()],
                        },
                    );
                }

                Ok(AskUserResponse { answers })
            }
            ElicitationAction::Decline => Err("ask_user was declined by the user.".into()),
            ElicitationAction::Cancel => Err("ask_user was cancelled by the user.".into()),
        }
    }
}

#[tool_router]
impl AgfsMcpServer {
    #[tool(
        name = "shell",
        description = "Run a shell command inside an agfs sandbox. The command result includes the current staged files so the agent can decide whether to abort them or keep them staged."
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
                vec![ChangeAction::Abort, ChangeAction::Stage]
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
        name = "ask_user",
        description = "Ask the user one to three short multiple-choice questions and wait for a response.",
        annotations(
            title = "Ask the user",
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false,
            idempotent_hint = false
        )
    )]
    async fn ask_user(
        &self,
        peer: Peer<RoleServer>,
        Parameters(request): Parameters<AskUserRequest>,
    ) -> Result<Json<AskUserResponse>, String> {
        Self::validate_ask_user_request(&request)?;
        Ok(Json(Self::request_user_answers(&peer, &request).await?))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AgfsMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Use `shell` to run a command in the sandbox. Inspect `changed_files`, then call `decide_changes` with `abort` or `stage`. Use `ask_user` when you need a short multiple-choice answer from the user.",
        )
    }
}

pub async fn serve_mcp() -> Result<()> {
    let server = AgfsMcpServer::new(std::env::current_dir()?);
    server.serve(stdio()).await?.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::serde_json::json;

    fn sample_ask_request() -> AskUserRequest {
        AskUserRequest {
            questions: vec![AskUserQuestion {
                id: "sandbox_choice".into(),
                header: "Sandbox".into(),
                question: "What should happen to the staged changes?".into(),
                options: vec![
                    AskUserOption {
                        label: "Abort".into(),
                        description: "Discard the staged changes.".into(),
                    },
                    AskUserOption {
                        label: "Stage".into(),
                        description: "Keep the staged changes for later.".into(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn ask_user_validation_rejects_too_many_questions() {
        let request = AskUserRequest {
            questions: vec![
                sample_ask_request().questions[0].clone(),
                AskUserQuestion {
                    id: "second".into(),
                    header: "Second".into(),
                    question: "Second question?".into(),
                    options: sample_ask_request().questions[0].options.clone(),
                },
                AskUserQuestion {
                    id: "third".into(),
                    header: "Third".into(),
                    question: "Third question?".into(),
                    options: sample_ask_request().questions[0].options.clone(),
                },
                AskUserQuestion {
                    id: "fourth".into(),
                    header: "Fourth".into(),
                    question: "Fourth question?".into(),
                    options: sample_ask_request().questions[0].options.clone(),
                },
            ],
        };

        let error = AgfsMcpServer::validate_ask_user_request(&request).unwrap_err();
        assert_eq!(error, "ask_user supports at most three questions.");
    }

    #[test]
    fn ask_user_schema_uses_question_ids_and_option_labels() {
        let request = sample_ask_request();
        let schema = AgfsMcpServer::build_ask_user_schema(&request.questions).unwrap();
        let json = serde_json::to_value(schema).unwrap();

        assert_eq!(json["required"], json!(["sandbox_choice"]));
        assert_eq!(
            json["properties"]["sandbox_choice"]["title"],
            json!("Sandbox")
        );
        assert_eq!(
            json["properties"]["sandbox_choice"]["description"],
            json!("What should happen to the staged changes?")
        );
        assert_eq!(
            json["properties"]["sandbox_choice"]["oneOf"][0]["const"],
            json!("Abort")
        );
        assert_eq!(
            json["properties"]["sandbox_choice"]["oneOf"][1]["const"],
            json!("Stage")
        );
    }
}
