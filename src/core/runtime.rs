use std::{collections::BTreeMap, sync::Arc};

use futures_util::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    error::{Error, Result},
    hooks::{
        BeforeMessageContext, BeforeProviderRequestContext, BeforeToolExecutionContext,
        ContextContributionContext,
    },
    models::{
        ContextPriority, ContextSource, FinishReason, Message, MessagePart, ProjectId,
        ProviderEvent, ProviderRequest, Role, RuntimeEvent, SessionGroupId, SessionId, ToolCallId,
        ToolOutput, TurnId,
    },
    operations::Operations,
    registry::Registry,
    workdir::Workdir,
};

#[derive(Clone)]
pub struct TurnRequest {
    pub project_id: ProjectId,
    pub session_group_id: SessionGroupId,
    pub session_id: SessionId,
    pub provider_id: super::models::ProviderId,
    pub model: String,
    pub input: String,
    pub cancellation: CancellationToken,
}

impl TurnRequest {
    pub fn new(
        project_id: ProjectId,
        session_group_id: SessionGroupId,
        session_id: SessionId,
        provider_id: super::models::ProviderId,
        model: impl Into<String>,
        input: impl Into<String>,
    ) -> Self {
        Self {
            project_id,
            session_group_id,
            session_id,
            provider_id,
            model: model.into(),
            input: input.into(),
            cancellation: CancellationToken::new(),
        }
    }
}

#[derive(Clone)]
pub struct TurnEngine {
    pub registry: Registry,
    pub operations: Operations,
    pub workdir: Arc<dyn Workdir>,
}

impl TurnEngine {
    pub fn new(registry: Registry, operations: Operations, workdir: Arc<dyn Workdir>) -> Self {
        Self {
            registry,
            operations,
            workdir,
        }
    }

    pub async fn run(&self, request: TurnRequest) -> Result<TurnId> {
        let turn_id = TurnId::new();
        let user = Message::text(Role::User, request.input.clone(), Some(turn_id));
        let user_for_hook = Arc::new(user.clone());
        for hook in self.registry.hooks().before_message.clone() {
            hook.before_message(BeforeMessageContext {
                turn_id,
                message: user_for_hook.clone(),
            })
            .await?;
        }
        self.operations
            .add_conversation_message(user, ContextPriority::High)
            .await?;
        self.operations
            .emit(RuntimeEvent::TurnStarted { turn_id })
            .await?;

        let provider = self.registry.provider(request.provider_id).ok_or_else(|| {
            Error::Provider(format!(
                "provider {} is not registered",
                request.provider_id
            ))
        })?;
        let mut completed = false;
        while !completed {
            if request.cancellation.is_cancelled() {
                self.operations
                    .emit(RuntimeEvent::TurnCancelled { turn_id })
                    .await?;
                return Err(Error::Cancelled);
            }
            let state = self.operations.snapshot().await?;
            let mut messages = Vec::new();
            for part in state.active_context() {
                match part.source {
                    ContextSource::Message(id) => {
                        if let Some(message) = state.messages.get(&id) {
                            messages.push(Arc::new(message.clone()));
                        }
                    }
                    ContextSource::Custom(text) => {
                        messages.push(Arc::new(Message::text(Role::System, text, Some(turn_id))))
                    }
                }
            }
            let contributions = self
                .registry
                .hooks()
                .contributions(ContextContributionContext {
                    turn_id,
                    messages: messages.clone(),
                })
                .await?;
            for contribution in contributions {
                messages.push(Arc::new(Message::text(
                    Role::System,
                    contribution.text,
                    Some(turn_id),
                )));
            }
            let tools = self
                .registry
                .tools()
                .into_iter()
                .map(|tool| tool.definition())
                .collect();
            for hook in self.registry.hooks().before_provider_request.clone() {
                hook.before_provider_request(BeforeProviderRequestContext {
                    turn_id,
                    model: request.model.clone(),
                })
                .await?;
            }
            let provider_request = ProviderRequest {
                model: request.model.clone(),
                messages,
                tools,
                cancellation: request.cancellation.child_token(),
            };
            let mut stream = provider.request(provider_request).await?;
            let round = match self
                .collect_round(&mut stream, turn_id, &request.cancellation)
                .await
            {
                Ok(round) => round,
                Err(Error::Cancelled) => {
                    self.operations
                        .emit(RuntimeEvent::TurnCancelled { turn_id })
                        .await?;
                    return Err(Error::Cancelled);
                }
                Err(error) => return Err(error),
            };
            self.operations
                .emit(RuntimeEvent::ProviderRoundFinished {
                    turn_id,
                    reason: round.reason.clone(),
                })
                .await?;

            let assistant = Message {
                id: super::models::MessageId::new(),
                turn_id: Some(turn_id),
                role: Role::Assistant,
                content: round.parts,
                created_at: super::models::TimeSeq::new(),
                metadata: BTreeMap::new(),
            };
            let assistant_parts = assistant.content.clone();
            if !assistant_parts.is_empty() {
                self.operations
                    .add_conversation_message(assistant.clone(), ContextPriority::High)
                    .await?;
                self.operations
                    .emit(RuntimeEvent::AssistantMessageCommitted {
                        turn_id,
                        message: assistant,
                    })
                    .await?;
            }
            let had_calls = !round.calls.is_empty();
            let mut stop = false;
            for call in round.calls {
                if stop {
                    self.commit_tool_result(
                        turn_id,
                        call.id,
                        ToolOutput::Failure {
                            content: "Cancelled because execution stopped for user input.".into(),
                        },
                    )
                    .await?;
                    continue;
                }
                self.operations
                    .emit(RuntimeEvent::ToolExecutionStarted {
                        turn_id,
                        call_id: call.id,
                        name: call.name.clone(),
                    })
                    .await?;
                let output = match self.execute_tool(&request, turn_id, call).await {
                    Ok(output) => output,
                    Err(Error::Cancelled) => {
                        self.operations
                            .emit(RuntimeEvent::TurnCancelled { turn_id })
                            .await?;
                        return Err(Error::Cancelled);
                    }
                    Err(error) => return Err(error),
                };
                self.operations
                    .emit(RuntimeEvent::ToolExecutionFinished {
                        turn_id,
                        call_id: output.0,
                        output: output.1.clone(),
                    })
                    .await?;
                self.commit_tool_result(turn_id, output.0, output.1.clone())
                    .await?;
                if matches!(output.1, ToolOutput::Stop) {
                    stop = true;
                }
            }
            if stop || !had_calls {
                completed = true;
            }
        }
        self.operations
            .emit(RuntimeEvent::TurnCompleted { turn_id })
            .await?;
        Ok(turn_id)
    }

    async fn collect_round(
        &self,
        stream: &mut super::models::ProviderStream,
        turn_id: TurnId,
        cancellation: &CancellationToken,
    ) -> Result<CollectedRound> {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut calls: BTreeMap<u32, AssembledCall> = BTreeMap::new();
        let mut reason = FinishReason::Stop;
        loop {
            let item = tokio::select! { _ = cancellation.cancelled() => return Err(Error::Cancelled), item = stream.next() => item };
            let Some(item) = item else {
                break;
            };
            match item? {
                ProviderEvent::TextDelta { text: delta } => {
                    text.push_str(&delta);
                    self.operations
                        .emit(RuntimeEvent::ProviderStreamDelta {
                            turn_id,
                            text: delta,
                            reasoning: false,
                        })
                        .await?;
                }
                ProviderEvent::ReasoningDelta { text: delta } => {
                    reasoning.push_str(&delta);
                    self.operations
                        .emit(RuntimeEvent::ProviderStreamDelta {
                            turn_id,
                            text: delta,
                            reasoning: true,
                        })
                        .await?;
                }
                ProviderEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments,
                } => {
                    let call = calls.entry(index).or_insert_with(|| AssembledCall {
                        id: ToolCallId::new(),
                        provider_id: None,
                        name: String::new(),
                        arguments: String::new(),
                    });
                    if let Some(name) = name {
                        call.name = name;
                    }
                    if id.is_some() {
                        call.provider_id = id;
                    }
                    call.arguments.push_str(&arguments);
                }
                ProviderEvent::Usage { usage } => {
                    self.operations
                        .emit(RuntimeEvent::ProviderUsageUpdated { turn_id, usage })
                        .await?
                }
                ProviderEvent::Finished { reason: finished } => reason = finished,
            }
        }
        let mut parts = Vec::new();
        if !reasoning.is_empty() {
            parts.push(MessagePart::Reasoning { text: reasoning });
        }
        if !text.is_empty() {
            parts.push(MessagePart::Text { text });
        }
        for call in calls.values() {
            let arguments = serde_json::from_str(&call.arguments)
                .unwrap_or_else(|_| Value::String(call.arguments.clone()));
            parts.push(MessagePart::ToolCall {
                id: call.id,
                name: call.name.clone(),
                arguments,
            });
        }
        Ok(CollectedRound {
            parts,
            calls: calls.into_values().collect(),
            reason,
        })
    }

    async fn execute_tool(
        &self,
        request: &TurnRequest,
        turn_id: TurnId,
        call: AssembledCall,
    ) -> Result<(ToolCallId, ToolOutput)> {
        let Some(tool) = self.registry.tool_by_name(&call.name) else {
            return Ok((
                call.id,
                ToolOutput::Failure {
                    content: format!("unknown tool: {}", call.name),
                },
            ));
        };
        for hook in self.registry.hooks().before_tool.clone() {
            hook.before_tool_execution(BeforeToolExecutionContext {
                turn_id,
                call_id: call.id,
                tool_name: call.name.clone(),
            })
            .await?;
        }
        let input = serde_json::from_str::<Value>(&call.arguments)
            .unwrap_or_else(|_| Value::String(call.arguments.clone()));
        let context = super::models::ToolContext {
            project_id: request.project_id,
            session_group_id: request.session_group_id,
            session_id: request.session_id,
            turn_id,
            operations: self.operations.clone(),
            workdir: self.workdir.clone(),
            cancellation: request.cancellation.child_token(),
        };
        let output = match tool.execute(input, context).await {
            Ok(output) => output,
            Err(Error::Tool(message)) => ToolOutput::Failure { content: message },
            Err(error) => return Err(error),
        };
        for hook in self.registry.hooks().after_tool.clone() {
            hook.after_tool_execution(super::hooks::AfterToolExecutionContext {
                turn_id,
                call_id: call.id,
                tool_name: call.name.clone(),
                output: output.clone(),
            })
            .await?;
        }
        Ok((call.id, output))
    }

    async fn commit_tool_result(
        &self,
        turn_id: TurnId,
        call_id: ToolCallId,
        output: ToolOutput,
    ) -> Result<()> {
        let summary = match &output {
            ToolOutput::Success { content } => content
                .lines()
                .next()
                .unwrap_or("tool completed")
                .to_string(),
            ToolOutput::Failure { content } => {
                content.lines().next().unwrap_or("tool failed").to_string()
            }
            ToolOutput::Stop => "Waiting for user response.".into(),
        };
        let message = Message {
            id: super::models::MessageId::new(),
            turn_id: Some(turn_id),
            role: Role::Tool,
            content: vec![MessagePart::ToolResult {
                call_id,
                summary,
                result: output,
            }],
            created_at: super::models::TimeSeq::new(),
            metadata: BTreeMap::new(),
        };
        self.operations
            .add_conversation_message(message, ContextPriority::High)
            .await?;
        Ok(())
    }
}

struct CollectedRound {
    parts: Vec<MessagePart>,
    calls: Vec<AssembledCall>,
    reason: FinishReason,
}
struct AssembledCall {
    id: ToolCallId,
    provider_id: Option<String>,
    name: String,
    arguments: String,
}
