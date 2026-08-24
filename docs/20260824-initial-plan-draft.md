# AiriCode
Minimal, yet feature-rich coding agent

## File Structure
File은 4가지 Scope 중 하나이다
* `core`
* `plugins`
* `ui`
* `utils`

* `src/core/`
  * `config.rs`
  * `core.rs`
  * `registry.rs`
  * `mod.rs`

* `src/core/models/`
  * `command.rs`
  * `context.rs`
  * `message.rs`
  * `note.rs`
  * `project.rs`
  * `provider.rs`
  * `session.rs`
  * `session_group.rs`
  * `tool.rs`
  * `ui_state.rs`
  * `workdir.rs`

* `src/core/operations/`
  * Hook에서 사용할 수 있는 함수이다.
  * Plugin은 해당 Operation만을 사용해 core와 통신해야 한다.
  * `add_message.rs`
    * 컨텍스트에 자동으로 넣지 않고 메시지만 추가하는 Low-level operation
  * `add_context_part.rs`
  * `create_session.rs`
    * session group 내에 새 session을 추가하는 operation
  * `get_context.rs`
  * `get_messages.rs`
  * `invalidate_context_part.rs`
  * `invalidate_message.rs`
  * `request.rs`
  * ...

* `src/utils/`
  * `hashline.rs`

## 플러그인 목록
* `src/plugins/`
  * `fork.rs`
  * `compaction.rs`
  * `instruction_agents.rs`
  * `instruction_base.rs`
  * `mcp.rs`
  * `persistence.rs`
  * `provider_openai.rs`
  * `revert.rs`
  * `sandbox.rs`
  * `sandbox_bubblewrap.rs`
  * `sandbox_container.rs`
  * `sidequery.rs`
  * `skills.rs`
  * `tool_grep.rs`
  * `tool_patch.rs`
  * `tool_question.rs`
  * `tool_shell.rs`
  * `tool_subagents.rs`
  * `tool_todo.rs`
  * `tool_webfetch.rs`
  * `workdir_gitworktree.rs`

### ForkPlugin
* sessiongroup에 새 session을 만들고 message, notes, context 등을 복사한다.

### CompactionPlugin

### InstructionAgentsPlugin
* workdir의 root에 있는 agents.md 를 읽어서 contextcontribution함
* read tool call 시점에 context 목록에 해당 파일로부터 가장 가까운 agents.md 가 없을 경우 그 agents.md를 컨텍스트에 추가함

### PersistencePlugin
세션을 `.local/share/airicode/[projectname]-[projecthash]/sessions/[sessionhash].jsonl` (append-only json) 에 저장한다.
목적은 에이전트 실행 중간에 프로세스를 죽인 후 다시 실행했을 때 Context와 이전 메세지 이력을 복구하기 위함이다.

### SidequeryPlugin
현재 context를 더럽히지 않고 병렬로 질문을 한다.
Message가 아닌 Note에 Q: [질문] 을 추가하고 메세지 도착시 A: [답변] 을 추가한다.

### SkillsPlugin
스킬을 읽고 /[skill-name] command를 추가한다.
스킬 관련 tool을 추가한다.
활성화된 스킬 목록을 관리한다.
활성화된 스킬에 대해 ContextContribution을 해준다.

### SubagentsPlugin
### ToolQuestionPlugin
ToolOutput에서 action을 stop으로 주고, UI State에서 메세지 Draft 뒤에 Question과 선택지를 추가해준다.

### ToolReadPlugin
파일을 읽는다. hashline 형태로 입력해준다.
range로 읽을 수 있으며 line이나 byte 수가 너무 클 경우 거절한다.

### ToolPatchPlugin
hashline의 앞의 tag와 뒤의 내용을 입력할 경우 해당 hashline을 패치한다.
tool message는 기본적으로 summary만 출력되기 때문에 Diff Note를 별도로 추가해서 사용자에게 알린다.

### GitWorktreePlugin
* `.local/share/airicode/[projectname]-[projecthash]/worktree/[sessiongrouphash]/` 하위에 worktree를 만든다.
* toolcall 이 끝나면 commit을 한다.
* `/worktree-init` 시에 현재 workdir의 commit으로부터 새 worktree를 만든다.
* `/worktree-revert` 시에 head를 마지막 turn의 commit 위치로 되돌린다.
* `/worktree-commit` 시에 toolcall 커밋들을 squash 해서 workdir에 반영한다.
* `/worktree-discard` 시에 현재 worktree를 workdir의 commit 위치로 되돌린다.

## 구조
* Core는 최대한 미니멀하게, Plugin은 최대한 약결합되게 한다.
* Plugin과 다른 Plugin 간의 결합도 최대한 줄인다.
* Plugin이 있으면 기능이 있고, Plugin이 없으면 기능이 없다
* 단일 기능에 해당하는 전체 요소는 Plugin 하나에 구현될 수 있게 한다.

### Plugin
Plugin은 `id` `name` `config_schema` `init()` 을 가진다.

### Registry
* Plugin은 실행 시 `init` 이 호출된다.
* Plugin은 `init` 시에 Registry에 Hook, Command, Tool, Workdir, Provider 등을 등록할 수 있다.
  * Hook에서 동적으로 등록할 수도 있다. (예를 들어 특정 스킬에 대해 command를 추가하는 등)

### Hook
Init 시:
Init -> ConfigRead -> BeforeOpenProject -> OpenProject -> BeforeOpenSession -> OpenSession

Loop 시 > 사용자 메세지 전송 시:
BeforeMessage -> ContextContribution -> Message -> BeforeProviderRequest -> ProviderRequest

Loop 시 > 메세지 받을 시:
ProviderStream -> BeforeToolExecution -> ToolExecution -> TurnCompleted

그 외:
InvalidateContextPart
InvalidateMessage
AddMessage
AddContextPart
...



### Plugin API

### Models
소유 관계는 다음과 같다.
Core -> Config
Core -> CommandRegistry -> Command
Core -> HookRegistry -> Hook
Core -> ProviderRegistry -> Provider
Core -> PluginRegistry -> Plugin
Core -> ToolRegistry -> Tool
Core -> Project -> SessionGroup -> Workdir
                   SessionGroup -> Session -> Message
                                   Session -> Note
                                   Session -> Context -> ContextPart
                                   Session -> UIState
### Message
enum Role {
    System,
    User,
    Assistant,
    Tool,
}

enum MessagePart {
    Text {
        text: String,
    },
    ToolCall {
        id: ToolCallId,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: ToolCallId,
        summary: String,
        result: ToolOutput,
    },
    Reasoning {
        text: String,
    },
}

type MessageMetadata = BTreeMap<String, Value>;
struct Message {
    id: MessageId,
    role: Role,
    content: Vec<MessagePart>,
    created_at: TimeSeq,
    metadata: MessageMetadata,
}

### Notes
노트는 메시지와 비슷한 레벨로 표시되는 알림 등이다.
메시지는 아니지만 사용자에게 알려야할 내용들이 Notes로 표시된다.

enum NoteContent {
    Info {
        content: String,
    },
    Alert {
        content: String,
    },
    Diff {
        file: String,
        content: String
    }
}

type NoteMetadata = BTreeMap<String, Value>;
struct Note {
    id: NoteId,
    content: NoteContent,
    created_at: TimeSeq,
    metadata: NoteMetadata,
}

### Context
struct ContextPart {
    priority: ContextPriority,
    source: ContextSource,
    created_at: TimeSeq,
}

enum ContextPriority {
    Persistent,
    High,
    Low,
}

enum ContextSource {
    Message(Arc<Message>),
    Custom(String)
}

* Session에서 context 목록을 관리한다.
  * 이 목록에서 대부분의 context source는 message일 것이고, 혹시나 직접 `add_context_part` 를 했다면 custom도 있을 수 있음.
* Compaction이 `invalidate_context_part` operation을 통해 특정 ContextPart를 context목록에서 제거할 수 있다.
* `ContextContributionHook` 을 통해 목록을 건드리지 않고도 Context를 Augment할 수 있다.
  * Instruction 등의 plugin에서 필요한 내용을 context의 특정 위치에 추가해줌

### Session, SessionGroup
session 간에는 parent / child가 있으며 (subagents 를 위함) 해당 session들은 같은 sessiongroup에 속한다.
fork된 session 또한 같은 sessiongroup에 속해 workdir을 여러번 프로비저닝할 필요를 제거한다.

### Command
* Command는 name, schema, handler를 필요로 한다.
* schema는 다음과 같이 구성
  * type: select, autocomplete, string, number, bool
  * nargs: positional, optional, remainder
  * default value

### Config
* Config는 플러그인이 전부 로딩된 이후 파싱한다.
* HashMap<String, Value> 형태로 deserialize한 후 플러그인 내부에서 ConfigReadHook 에서 각자 플러그인 설정을 뽑아갈 수 있다.
  * 각 플러그인은 derive(JsonSchema) 해두고 Plugin::config_schema 로 제공한다.

### Workdir
* Workdir은 여러개의 Layer를 겹치는 식으로 동작한다.
* Sandbox, GitWorktree 등의 플러그인이 각각 자신의 Layer를 제공한다.

trait Workdir: Send + Sync {
    fn root(&self) -> &Path;
    async fn read(&self, path: &Path) -> Result<Vec<u8>>;
    async fn write(&self, path: &Path, data: &[u8]) -> Result<()>;
    async fn remove(&self, path: &Path) -> Result<()>;
    async fn execute(
        &self,
        command: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput>;
}

struct WorkdirLayerContext {
    project_id: ProjectId,
    project_name: String,
    session_id: Option<SessionId>,
}

trait WorkdirLayer: Send + Sync {
    fn id(&self) -> WorkdirLayerId;
    fn layer(&self, context: &WorkdirLayerContext, inner: Arc<dyn Workdir>) -> Arc<dyn Workdir>;
}

### Tool
struct ToolDefinition {
    name: String,
    description: String,
    input_schema: Value,
}

struct ToolContext {
    project_id: ProjectId,
    session_id: SessionId,
    turn_id: TurnId,
    workdir: std::sync::Arc<dyn Workdir>,
    cancellation: CancellationToken,
}

enum ToolOutput {
    Success {
        content: String
    },
    Failure {
        content: String
    },
    Stop
}

trait Tool: Send + Sync {
    fn id(&self) -> ToolId;
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: serde_json::Value, context: ToolContext) -> Result<ToolOutput>;
}

### Provider
struct Model {
    id: String,
    display_name: String,
}

struct ProviderRequest {
    model: String,
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    context: Context,
    cancellation: CancellationToken,
}

enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

enum ProviderEvent {
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallDelta {
        index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Usage {
        usage: Usage,
    },
    Finished {
        reason: FinishReason,
    },
}

Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn get_models(&self) -> Result<Vec<Model>>;
    async fn request(&self, request: ProviderRequest) -> Result<ProviderStream>;
}

### Cancellation Token
메세지 요청과 각 tool call 등은 cancel할 수 있게 한다.

### UI State
다음 상태를 저장한다.
* 선택된 모델
* 선택된 모드
* 입력중인 메시지
* 활성화된 목록 (skill, mcp, ...)

### Persistence
* Message (Agent, User, Tool Call, ...)
* Message Add
* Message Invalidation
* Context Part Add
* Context Part Invalidation

등의 이벤트를 jsonl로 저장한다.


## UI
UI는 여러개의 Fragment로 구성돼있다.
* `src/ui/terminal/`
  * `editbar.rs`
    * 하단의 상태바
    * 모드 선택, 모델 선택, variant 선택
  * `editor.rs`
    * 메시지 편집, command autocomplete
  * `messages.rs`
    * 메시지, note, tool 들 보여주기
  * `statusbar.rs`
    * 상단의 상태바
    * 세션 제목, 현재 토큰 사용량, 컨텍스트 크기

* `src/ui/web/` 도 만들 계획은 있으나, 지금 단계에서는 고려하지 않는다.
* 터미널 UI는 Ratatui로 구현한다.
