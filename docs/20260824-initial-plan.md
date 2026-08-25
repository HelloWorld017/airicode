# AiriCode — 제품 요구사항 및 전체 구현 계획

> **문서 상태:** Greenfield 구현 기준안  
> **프로젝트:** AiriCode  
> **슬로건:** Minimal, yet feature-rich coding agent  
> **주 구현 언어:** Rust  
> **초기 UI:** Ratatui 기반 Terminal UI  
> **핵심 아키텍처:** Minimal Core + Weakly-coupled In-process Plugins

---

## 1. 문서 목적

이 문서는 AiriCode를 **기존 프로토타입의 리팩터링이 아닌 완전한 신규 구현**으로 다시 만들기 위한 제품 요구사항(PRD), 아키텍처 계약, 구현 순서, 테스트 전략, 완료 조건을 정의한다.

기존 프로토타입은 다음 용도로만 참고한다.

- 현재 계획에 빠진 세부 구현 문제를 발견할 때
- 이미 시도해본 접근이 실제 Rust 코드에서 어떤 난점을 만들었는지 확인할 때
- JSONL 복구, typed hook, registrar 등 일부 아이디어를 검토할 때

반대로 다음은 하지 않는다.

- 기존 코드를 새 구현의 기반으로 삼지 않는다.
- 기존 파일 구조를 그대로 보존하지 않는다.
- 기존 public API와의 호환성을 목표로 하지 않는다.
- 프로토타입 구현이 이 문서와 충돌할 경우 프로토타입을 우선하지 않는다.

**설계 우선순위는 항상 이 문서와 원본 기획이 프로토타입보다 높다.**

---

# 2. 제품 정의

## 2.1 비전

AiriCode는 Core를 최대한 작게 유지하면서, 실제 코딩 에이전트에 필요한 대부분의 기능을 Plugin으로 조립할 수 있는 런타임이다.

Core가 책임지는 것은 다음 정도여야 한다.

- Project / SessionGroup / Session의 생명주기
- Message / Note / Context / UIState 모델
- Plugin / Registry / Hook 시스템
- Provider / Tool / Command의 공통 인터페이스
- Agent turn orchestration
- 상태 변경용 Operation
- Workdir abstraction 및 layer 조립
- Cancellation
- UI가 구독할 runtime event
- Plugin이 제공할 경우 durable persistence와 연결되는 commit path

그 외의 기능은 가능한 한 Plugin으로 존재한다.

예:

- `PersistencePlugin`이 없으면 세션 저장 기능이 없다.
- `ProviderOpenAIPlugin`이 없으면 OpenAI provider가 없다.
- `GitWorktreePlugin`이 없으면 worktree 기능이 없다.
- `SandboxPlugin`이 없으면 sandbox isolation이 없다.
- `SkillsPlugin`이 없으면 skill discovery/activation이 없다.
- `McpPlugin`이 없으면 MCP tool이 없다.
- `InstructionAgentsPlugin`이 없으면 `AGENTS.md`를 읽지 않는다.
- `CompactionPlugin`이 없으면 context compaction이 없다.

이 성질은 구현 취향이 아니라 제품의 핵심 요구사항이다.

---

## 2.2 핵심 설계 원칙

### 원칙 A — Minimal Core

Core는 여러 기능이 공통적으로 의존하는 **primitive와 orchestration만** 가진다.

Core가 알아도 되는 것:

- Tool이라는 개념
- Provider라는 개념
- Workdir이라는 개념
- Context라는 개념
- Hook과 Operation이라는 개념

Core가 알면 안 되는 것:

- OpenAI
- MCP
- Git worktree
- bubblewrap
- skill
- `AGENTS.md`
- compaction 정책
- sidequery UX

### 원칙 B — Plugin-local completeness

하나의 기능을 추가할 때 해당 기능의 전체 요소가 가능한 한 하나의 Plugin 안에 모여야 한다.

Plugin 하나가 필요에 따라 동시에 등록할 수 있는 것:

- Hook
- Command
- Tool
- Provider
- WorkdirLayer
- persistence backend/factory
- plugin-specific runtime state와 metadata

### 원칙 C — Plugin 간 약결합

Plugin이 다른 Plugin의 concrete type을 직접 import하지 않는다.

금지 예시:

```rust
use crate::plugins::tool_read::ReadTool;
use crate::plugins::persistence::JsonlStore;
```

Plugin 간 협력은 다음 Core API로 제한한다.

- Operation
- Hook
- Registry
- Tool/Command/Provider 인터페이스
- Workdir
- generic metadata / plugin state

### 원칙 D — 상태 변경은 Operation으로만

Plugin과 UI는 Core가 소유하는 Session state를 직접 mutate하지 않는다.

**durable state mutation의 유일한 진입점은 Operation이다.**

### 원칙 E — Append-only recovery

Persistence가 활성화되어 있다면 세션 복구 정보는 append-only 형식을 사용한다.

에이전트 실행 도중 프로세스를 강제 종료해도, 이미 commit된 Message / Context / Note / UI 설정은 다음 실행에서 복구할 수 있어야 한다.

### 원칙 F — 공유 Workdir은 의도된 정상 동작

`SessionGroup`에 속한 여러 Session, fork, parent/child subagent는 같은 logical Workdir을 공유한다.

그리고 이는 단순한 구현 편의가 아니라 **AiriCode의 의도된 동시성 모델**이다.

여러 에이전트가 동시에 다음을 수행할 수 있다.

- 같은 native workdir 읽기
- 서로 다른 파일 수정
- 같은 파일 수정 시도
- shell command 실행
- parent와 subagent가 동시에 작업
- 동일 managed Git worktree 사용

Core는 이를 이유로 SessionGroup 전체에 전역 write mutex를 두지 않는다.

충돌 안전성은 다음 계층에서 해결한다.

- hashline 기반 stale edit 탐지
- patch 직전 재검증
- 필요한 경우 atomic file replacement
- Git을 사용하는 경우 Git state/conflict 확인
- 특정 backend가 반드시 요구할 경우 해당 backend 내부 동기화
- sandbox가 켜져 있다면 sandbox 경계

즉 AiriCode는 **전체 Workspace를 직렬화하여 충돌을 숨기기보다, 실제 공유 작업공간에서 여러 agent가 협업하도록 허용하고 충돌을 명시적으로 표면화한다.**

### 원칙 G — Provider-neutral Core

Provider의 wire format, endpoint, SDK type, 특수 필드는 해당 Provider Plugin 안에만 존재한다.

### 원칙 H — UI 독립성

Core는 Ratatui를 몰라야 한다.

Terminal UI는 Core의 한 frontend일 뿐이며, 추후 Web UI를 추가하더라도 Core ownership을 다시 설계하지 않아야 한다.

---

# 3. 초기 제품 범위

## 3.1 포함

- Core runtime
- Plugin system
- Registry
- typed Hooks
- Operations
- Config aggregation
- Session / SessionGroup
- Message / Note / Context / UIState
- Provider streaming
- Tool execution
- Cancellation
- layered Workdir
- append-only JSONL persistence
- OpenAI Provider
- Ratatui TUI
- read / grep / patch / shell / question / todo / webfetch tools
- Git worktree
- Sandbox
- Fork
- Revert
- Compaction
- Sidequery
- Subagents
- `AGENTS.md` instructions
- Skills
- MCP

## 3.2 초기 버전의 비목표

초기 구현에서는 다음을 의도적으로 만들지 않는다.

- stable external Rust ABI plugin
- dylib plugin loading
- plugin marketplace
- Web UI
- 분산 Session
- multi-user collaboration
- SQLite primary store
- persistence log GC/compaction
- concurrent edit 자동 병합
- Git 자동 rebase/merge conflict 해결
- provider 특수 기능을 Core에 계속 추가하는 구조
- generic UI plugin framework

초기 Plugin은 모두 binary에 함께 컴파일되는 in-process Rust Plugin으로 충분하다.

---

# 4. 권장 소스 구조

```text
src/
  core/
    mod.rs
    core.rs
    config.rs
    registry.rs
    hooks.rs
    events.rs
    error.rs

    models/
      mod.rs
      id.rs
      command.rs
      context.rs
      message.rs
      note.rs
      project.rs
      provider.rs
      session.rs
      session_group.rs
      tool.rs
      ui_state.rs
      workdir.rs

    operations/
      mod.rs
      add_message.rs
      add_context_part.rs
      add_note.rs
      update_note.rs
      create_session.rs
      get_context.rs
      get_messages.rs
      invalidate_context_part.rs
      invalidate_message.rs
      request.rs
      update_ui_state.rs
      ...

    runtime/
      mod.rs
      reducer.rs
      session_actor.rs
      turn.rs
      provider_round.rs

  plugins/
    fork.rs
    compaction.rs
    instruction_agents.rs
    instruction_base.rs
    mcp.rs
    persistence.rs
    provider_openai.rs
    revert.rs
    sandbox.rs
    sandbox_bubblewrap.rs
    sandbox_container.rs
    sidequery.rs
    skills.rs
    tool_grep.rs
    tool_patch.rs
    tool_question.rs
    tool_read.rs
    tool_shell.rs
    tool_subagents.rs
    tool_todo.rs
    tool_webfetch.rs
    workdir_gitworktree.rs

  ui/
    terminal/
      mod.rs
      app.rs
      editbar.rs
      editor.rs
      messages.rs
      statusbar.rs

  utils/
    mod.rs
    hashline.rs
```

의존 방향:

```text
utils
  ↑
core
 ↑  ↑
plugins  ui
```

허용:

```text
utils   -> std / external crates
core    -> utils
plugins -> core + utils
ui      -> core + utils
```

금지:

```text
plugins -> plugins
core    -> plugins
core    -> ui
```

CI에서 `src/plugins/` 내부의 `crate::plugins::` cross-import를 검사해 실패시키는 것도 권장한다.

---

# 5. Core ownership model

```text
Core
├─ Config
├─ CommandRegistry -> Command
├─ HookRegistry -> Hook
├─ ProviderRegistry -> Provider
├─ PluginRegistry -> Plugin
├─ ToolRegistry -> Tool
└─ Project
   └─ SessionGroup
      ├─ Workdir
      └─ Session
         ├─ Message
         ├─ Note
         ├─ Context
         └─ UIState
```

## 5.1 SessionGroup

SessionGroup은 다음을 위한 단위이다.

- fork된 Session 묶음
- parent/child subagent 묶음
- 동일 filesystem state를 공유해야 하는 Session 묶음

SessionGroup이 Workdir을 소유한다.

```text
SessionGroup
  ├─ Session A ─┐
  ├─ Session B ─┼── same Arc<dyn Workdir>
  └─ Session C ─┘
```

새 child/fork Session을 만들 때 Workdir을 새로 provision하지 않는다.

---

# 6. ID 모델

도메인 identity가 필요한 곳에 raw String을 남발하지 않는다.

최소한 다음 newtype ID를 둔다.

```text
ProjectId
SessionGroupId
SessionId
TurnId
MessageId
NoteId
ContextPartId
ToolId
ToolCallId
ProviderId
PluginId
CommandId
WorkdirLayerId
CommitId
RegistrationId
```

요구사항:

- serde 가능
- clone 비용이 작음
- 안정적인 Display
- fork/replay 시 ID rewrite가 불필요할 정도의 전역 유일성

UUIDv7, ULID 또는 비슷한 sortable random ID 사용을 권장한다.

---

# 7. Message

```rust
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
    turn_id: Option<TurnId>,
    role: Role,
    content: Vec<MessagePart>,
    created_at: TimeSeq,
    metadata: MessageMetadata,
}
```

## 7.1 불변식

- 한번 추가된 Message는 immutable이다.
- runtime에서는 `Arc<Message>` 공유 가능.
- Message를 없애는 것은 물리 삭제가 아니라 invalidation이다.
- fork는 과거 immutable Message identity를 공유해도 된다.
- Provider streaming delta는 완료 전까지 Message가 아니다.
- 가능하면 모든 conversation Message는 `turn_id`를 가진다.

`turn_id`는 다음 기능에서 사용된다.

- `/revert <turn>`
- TUI turn grouping
- Git commit metadata
- tracing
- diagnostics

---

# 8. Note

Note는 사용자에게 보여야 하지만 Provider conversation Message는 아닌 정보이다.

```rust
enum NoteContent {
    Info {
        content: String,
    },
    Alert {
        content: String,
    },
    Diff {
        file: String,
        content: String,
    },
}

type NoteMetadata = BTreeMap<String, Value>;

struct Note {
    id: NoteId,
    content: NoteContent,
    created_at: TimeSeq,
    metadata: NoteMetadata,
}
```

필요 Operation:

```text
add_note
update_note
```

`update_note`는 Sidequery 같은 기능에 필수다.

예:

```text
Q: 이 함수가 왜 실패하지?
```

이후:

```text
Q: 이 함수가 왜 실패하지?
A: parser state가 요청 사이에 재사용되고 있다.
```

Note는 기본적으로 Context에 자동 삽입하지 않는다.

---

# 9. Ordering

Message, Note, ContextPart의 생성 시각은 `TimeSeq`로 기록한다.
`TimeSeq`는 millisecond timestamp와 같은 시각 안의 sequence를 함께 가지므로,
별도의 순서 모델 없이도 복구 후 동일한 순서를 재현할 수 있다.

Context는 `ContextPriority`가 아니라 `created_at` 순으로 정렬한다.

---

# 10. Context

```rust
struct ContextPart {
    id: ContextPartId,
    priority: ContextPriority,
    source: ContextSource,
    created_at: TimeSeq,
    metadata: BTreeMap<String, Value>,
}

enum ContextPriority {
    Persistent,
    High,
    Low,
}

enum ContextSource {
    Message(Arc<Message>),
    Custom(String),
}
```

## 10.1 Message history와 Context를 분리하는 이유

UI에서 보이는 history와 Provider에게 보내는 context는 다른 개념이다.

예:

- Compaction이 오래된 Message를 UI에는 남기고 Context에서는 제거할 수 있다.
- low-level `add_message`는 Message만 추가하고 Context에는 넣지 않을 수 있다.
- instruction plugin은 Message가 아닌 Custom Context를 추가할 수 있다.
- 특정 ContextPart만 invalidate할 수 있다.

## 10.2 ContextPart ID

`invalidate_context_part`가 안정적으로 동작하려면 ContextPart 자체의 ID가 반드시 필요하다.

## 10.3 persistence representation

Runtime에서는:

```rust
ContextSource::Message(Arc<Message>)
```

를 사용해도 되지만, JSONL에는 Message 전체를 중복 저장하지 않는다.

```rust
enum PersistedContextSource {
    Message(MessageId),
    Custom(String),
}
```

replay 시 MessageId를 `Arc<Message>`로 resolve한다.

## 10.4 metadata 예시

```text
plugin_id = "instruction_agents"
source_path = "src/foo/AGENTS.md"
skill_id = "rust-review"
compaction_id = "..."
covers = [...ContextPartId]
```

Core는 Plugin-specific metadata를 해석하지 않는다.

---

# 11. ContextContribution

Plugin이 durable Context 목록 자체를 수정하지 않고 Provider 요청 시점에만 Context를 augment할 수 있어야 한다.

이를 위해 `ContextContributionHook`을 둔다.

사용 예:

- InstructionBasePlugin
- 활성화된 SkillsPlugin
- root `AGENTS.md`
- 일시적인 provider hint

Provider input 생성:

```text
Stored active Context
        +
ContextContribution hooks
        ↓
ContextSnapshot
        ↓
Core materialization
        ↓
ProviderRequest.messages
```

Provider가 Core Context 자체의 의미를 다시 해석하지 않게 한다.

---

# 12. UI State

UIState는 durable state와 ephemeral state를 구분한다.

```rust
struct UIState {
    durable: DurableUIState,
    ephemeral: EphemeralUIState,
}

struct DurableUIState {
    selected_model: Option<ModelRef>,
    selected_mode: Option<String>,
    selected_variant: Option<String>,
    plugin_state: BTreeMap<String, Value>,
}

struct EphemeralUIState {
    draft: String,
    // cursor / selection / scroll 등
}
```

Core에 다음과 같은 feature-specific field를 직접 두지 않는다.

```text
active_skills
active_mcp_servers
pending_todos
```

대신:

```text
plugin_state["skills"]
plugin_state["mcp"]
```

처럼 namespace를 둔다.

Draft, cursor, scroll은 키 입력마다 persistence event가 생기지 않도록 durable state에서 제외한다.

---

# 13. Provider

```rust
struct Model {
    id: String,
    display_name: String,
    capabilities: ModelCapabilities,
}

struct ModelCapabilities {
    context_window: Option<u64>,
    tools: bool,
    reasoning: bool,
}

struct ModelRef {
    provider_id: ProviderId,
    model_id: String,
}
```

Model 선택은 provider와 묶는다.

`selected_model: String`만 두면 서로 다른 Provider가 같은 model ID를 제공할 때 모호하다.

## 13.1 ProviderRequest

권장 형태:

```rust
struct ProviderRequest {
    model: String,
    messages: Vec<Arc<Message>>,
    tools: Vec<ToolDefinition>,
    cancellation: CancellationToken,
}
```

Core는 `ModelRef.provider_id`로 Provider를 선택한 뒤 해당 Provider에 `model_id`를 넘긴다.

`ProviderRequest`에 `messages`와 Core `Context`를 동시에 넣지 않는다.

Provider는 이미 materialize된 provider-neutral input만 받는다.

## 13.2 Streaming event

```rust
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
```

```rust
trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn get_models(&self) -> Result<Vec<Model>>;
    async fn request(&self, request: ProviderRequest) -> Result<ProviderStream>;
}
```

OpenAI-specific type은 `provider_openai.rs` 밖으로 나오지 않는다.

---

# 14. Provider streaming과 Message finalize

Streaming 중에는 다음을 persistence하지 않는다.

```text
TextDelta
ReasoningDelta
ToolCallDelta
```

대신 runtime event로 UI에 보낸다.

TUI는 ephemeral Assistant draft를 렌더링한다.

Provider round가 종료된 후에만 immutable Assistant Message를 조립해 `add_message` 한다.

ToolCall arguments는 delta 문자열을 조립한 후 완료 시 JSON parse한다.

Cancel 시 이미 받은 partial response를 보존할지 정책을 하나로 정한다.

보존한다면 예:

```text
metadata["incomplete"] = true
metadata["cancelled"] = true
```

---

# 15. Tool

```rust
struct ToolDefinition {
    name: String,
    description: String,
    input_schema: Value,
}

struct ToolContext {
    project_id: ProjectId,
    session_group_id: SessionGroupId,
    session_id: SessionId,
    turn_id: TurnId,
    operations: Operations,
    workdir: Arc<dyn Workdir>,
    cancellation: CancellationToken,
}

enum ToolOutput {
    Success {
        content: String,
    },
    Failure {
        content: String,
    },
    Stop,
}

trait Tool: Send + Sync {
    fn id(&self) -> ToolId;
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: Value, context: ToolContext) -> Result<ToolOutput>;
}
```

`ToolContext`에 `Operations`가 있어야 다음이 가능하다.

- Patch → Diff Note 추가
- Question → pending question UI/plugin state 변경
- Subagents → child Session 생성
- Todo → plugin state / Note 변경

Plugin이 Core object를 직접 잡고 mutate하는 대신 모두 Operations를 사용한다.

## 15.1 ToolOutput 의미

### `Success`

정상 실행.

### `Failure`

모델에게 알려야 하는 예상 가능한 tool-level 실패.

예:

- grep no matches
- shell exit code 1
- stale hashline
- invalid path

### `Err(...)`

runtime/infrastructure 오류.

예:

- workdir backend unavailable
- session closed
- registry invariant violation

### `Stop`

현재 turn을 의도적으로 정지한다.

Question tool이 대표적인 예다.

Cancellation과는 다른 개념이다.

---

# 16. Shared Workdir 동시성 모델

이 부분은 AiriCode의 명시적인 제품 요구사항이다.

## 16.1 기본 정책

같은 SessionGroup의 모든 Session은 같은 logical Workdir을 공유한다.

```text
Session A -----┐
Session B -----┼--> Arc<dyn Workdir>
Subagent C ----┘
```

다른 Session에서 발생한 ToolCall끼리는 동시에 실행될 수 있다.

예:

```text
Session A: patch src/a.rs
Session B: cargo test
Session C: grep TODO
```

Core는 이 세 작업을 전역 mutex로 순차화하지 않는다.

## 16.2 필요한 성질

- `Workdir: Send + Sync`
- 각 backend의 thread safety
- read 후 write까지 파일이 그대로일 것이라고 가정하지 않음
- patch 직전 current bytes 재확인
- stale observation 탐지
- filesystem/Git conflict를 명시적인 오류로 surface

## 16.3 하지 않을 것

다음과 같은 Core-level 정책을 기본으로 두지 않는다.

```text
SessionGroup마다 mutating tool은 항상 1개만 실행
```

이는 사용자가 의도한 multi-agent shared-workspace behavior를 훼손한다.

## 16.4 backend-specific synchronization

특정 backend의 내부 data structure가 thread-safe하지 않거나, 짧은 critical section이 필요한 경우 해당 backend 내부에서 제한적인 lock을 사용할 수 있다.

하지만 그 lock이 전체 agent-level mutating tool을 장시간 직렬화하는 구조가 되어서는 안 된다.

---

# 17. Question Tool의 Stop 처리

`ToolQuestionPlugin`은:

1. pending question 상태를 기록한다.
2. Question과 choices를 UI에 표시한다.
3. `ToolOutput::Stop`을 반환한다.

그러나 Provider history 구조는 유효해야 한다.

Provider가 ToolCall을 생성했다면 Core는 해당 call에 대응하는 ToolResult를 생성해야 한다.

예:

```text
Waiting for user response.
```

한 Assistant Message에 여러 ToolCall이 있고 중간 Question에서 stop된다면 실행되지 않은 나머지 ToolCall에도 synthetic result를 둔다.

예:

```text
Cancelled because execution stopped for user input.
```

초기 구현에서는 **한 Assistant Message 내부 ToolCall은 안정적인 순서로 처리**한다.

이는 서로 다른 Session의 ToolCall concurrency와는 별개의 문제다.

---

# 18. Command

Command는 다음을 가진다.

- name
- argument schema
- handler
- optional autocomplete callback

Schema:

```text
type: select | autocomplete | string | number | bool
nargs: positional | optional | remainder
default: optional value
```

권장 인터페이스:

```rust
trait Command: Send + Sync {
    fn id(&self) -> CommandId;
    fn definition(&self) -> CommandDefinition;

    async fn execute(
        &self,
        input: CommandInput,
        context: CommandContext,
    ) -> Result<CommandOutput>;

    async fn complete(
        &self,
        context: CommandContext,
        argument_index: usize,
        prefix: &str,
    ) -> Result<Vec<Completion>> {
        Ok(vec![])
    }
}
```

`autocomplete` type은 schema만으로는 completion source가 없으므로 runtime callback이 필요하다.

CommandContext도 mutable Session object 대신 Operations를 가진다.

---

# 19. Workdir

```rust
trait Workdir: Send + Sync {
    fn root(&self) -> PathBuf;

    async fn read(&self, path: &Path) -> Result<Vec<u8>>;
    async fn write(&self, path: &Path, data: &[u8]) -> Result<()>;
    async fn remove(&self, path: &Path) -> Result<()>;

    async fn execute(
        &self,
        command: CommandSpec,
        cancellation: CancellationToken,
    ) -> Result<CommandOutput>;
}
```

`root()`는 `&Path`보다 owned `PathBuf`를 권장한다.

GitWorktree 같은 layer가 runtime에 active root를 바꿀 수 있기 때문이다.

## 19.1 WorkdirLayer

```rust
struct WorkdirLayerContext {
    project_id: ProjectId,
    project_name: String,
    session_group_id: SessionGroupId,
}

trait WorkdirLayer: Send + Sync {
    fn id(&self) -> WorkdirLayerId;

    fn layer(
        &self,
        context: &WorkdirLayerContext,
        inner: Arc<dyn Workdir>,
    ) -> Arc<dyn Workdir>;
}
```

`session_id`보다 `session_group_id`가 중요하다.

Workdir은 SessionGroup 단위로 provision되기 때문이다.

## 19.2 Layer ordering

숫자 priority만 두는 것보다 phase를 같이 두는 것을 권장한다.

```rust
enum WorkdirLayerPhase {
    Provision,
    Isolation,
    Observe,
}
```

예:

```text
GitWorktree -> Provision
Sandbox     -> Isolation
```

조립 결과:

```text
Sandbox(
  GitWorktree(
    NativeWorkdir
  )
)
```

Plugin끼리는 서로를 알지 않는다.

---

# 20. Plugin API

```rust
trait Plugin: Send + Sync {
    fn id(&self) -> PluginId;
    fn name(&self) -> &str;
    fn config_schema(&self) -> Value;

    async fn init(
        self: Arc<Self>,
        registrar: PluginRegistrar,
    ) -> Result<()>;
}
```

PluginRegistrar에서 등록 가능한 것:

- Hook
- Command
- Tool
- Provider
- WorkdirLayer
- SessionStore factory/backend
- 기타 Core가 정의한 generic registry entry

Skills/MCP처럼 runtime dynamic registration이 필요한 경우 init 이후에도 등록/해제가 가능해야 한다.

---

# 21. Registry

Registry는 runtime에 mutable할 수 있지만 handler 실행은 snapshot된 `Arc`로 한다.

권장 내부 구조:

```text
RwLock<RegistryState>
```

등록:

```text
write lock
-> validate
-> insert/remove
-> revision += 1
-> unlock
-> RegistryChanged RuntimeEvent
```

실행:

```text
read lock
-> Arc<dyn Tool/...> clone
-> unlock
-> execute
```

**Plugin callback, Provider request, Tool execution, Command handler 중 Registry lock을 잡고 있으면 안 된다.**

각 registration metadata:

```rust
owner: PluginId,
priority: i32,
order: u64,
registration_id: RegistrationId,
```

동적 등록에는 동적 해제가 필수다.

예:

```rust
let handle = registrar.register_command(...)?;
handle.remove().await?;
```

---

# 22. Config bootstrap

Config는 모든 Plugin이 schema를 제공할 기회를 가진 뒤 파싱한다.

권장 bootstrap:

```text
CoreBuilder
-> Plugin instance 등록
-> Plugin::init 호출
-> 성공한 Registry registration publish
-> 모든 Plugin config_schema 수집
-> raw config load/merge
-> schema validation
-> ConfigReadHook
-> BeforeOpenProject
-> OpenProject
```

Core Config에 다음을 직접 넣지 않는다.

```text
OpenAIConfig
SandboxConfig
CompactionConfig
SkillsConfig
```

Plugin이 자기 namespace를 직접 deserialize한다.

예:

```toml
[plugins.provider_openai.providers.work]
api_key_env = "WORK_OPENAI_API_KEY"

[plugins.provider_openai.providers.personal]
api_key_env = "OPENAI_API_KEY"
```

ConfigRead 시 Plugin이 여러 Provider instance를 동적으로 등록할 수 있다.

---

# 23. Hook lifecycle

## 23.1 Init / Open

```text
Init
-> ConfigRead
-> BeforeOpenProject
-> OpenProject
-> BeforeOpenSession
-> OpenSession
```

## 23.2 Close / Shutdown

초기 설계에 반드시 추가한다.

```text
BeforeCloseSession
-> CloseSession
-> BeforeCloseProject
-> CloseProject
-> Shutdown
```

MCP process, container, provider resource를 정리하려면 close lifecycle이 필요하다.

## 23.3 Agent turn

```text
BeforeMessage
-> User Message/Context commit
-> TurnStarted

각 Provider round:
  active Context snapshot
  -> ContextContribution
  -> materialize ProviderRequest
  -> BeforeProviderRequest
  -> ProviderRequest
  -> ProviderStream*
  -> ProviderRoundFinished
  -> Assistant Message commit

  ToolCall이 있으면:
    각 ToolCall마다
      BeforeToolExecution
      -> ToolExecution
      -> AfterToolExecution
      -> ToolResult commit
    -> 다음 Provider round

  ToolCall이 없으면:
    TurnCompleted
```

`TurnCompleted`는 Tool 실행 전이 아니라 **전체 logical turn 마지막**이다.

## 23.4 기타 Hook

필요에 따라:

- InvalidateContextPart
- InvalidateMessage
- AddMessage
- AddContextPart
- registry 변화 관련 hook

등을 추가할 수 있다.

Hook는 state mutation 자체가 아니다.

Hook 안에서 durable state 변경이 필요하면 Operation을 호출한다.

---

# 24. Hook failure policy

초기에 정책을 통일한다.

| Hook 종류 | 실패 시 기본 동작 |
|---|---|
| `Before*` | 현재 action 중단 |
| `ContextContribution` | 현재 provider round 실패 |
| durable store append | Operation 실패 |
| `After*` observer | 이미 commit된 state는 유지, log 및 필요 시 Alert Note |
| RuntimeEvent consumer | log 후 runtime 계속 |

Plugin마다 임의의 rollback semantics를 만들지 않는다.

---

# 25. Operation

Plugin과 UI가 Core durable state를 바꾸는 유일한 public entry point이다.

초기 Operation:

```text
create_session
add_message
add_context_part
add_note
update_note
invalidate_message
invalidate_context_part
get_messages
get_context
update_ui_state
request
```

필요하면 composite/internal Operation을 둔다.

예:

```text
add_conversation_message
```

은 한 번에:

```text
MessageAdded
ContextPartAdded(Message(message_id))
```

를 commit한다.

반대로 low-level `add_message`는 원래 기획대로 Context에 자동 추가하지 않는다.

---

# 26. Durable state / SessionCommit

PersistencePlugin을 단순 Message 저장소로 만들지 않는다.

Core의 durable state 변경 자체를 commit으로 표현한다.

```rust
struct SessionCommit {
    sequence: u64,
    commit_id: CommitId,
    created_at: TimeSeq,
    mutations: Vec<SessionMutation>,
}

enum SessionMutation {
    SessionCreated { /* ... */ },

    MessageAdded {
        message: Message,
    },
    MessageInvalidated {
        message_id: MessageId,
    },

    ContextPartAdded {
        part: PersistedContextPart,
    },
    ContextPartInvalidated {
        context_part_id: ContextPartId,
    },

    NoteAdded {
        note: Note,
    },
    NoteUpdated {
        note_id: NoteId,
        content: NoteContent,
        metadata: NoteMetadata,
    },

    DurableUIStateUpdated {
        /* ... */
    },
}
```

Session state는 reducer가 commit을 적용해 만든 projection이다.

## 26.1 Atomic logical operation

한 logical Operation이 여러 구조를 동시에 변경하고, 중간 상태가 invalid라면 하나의 SessionCommit에 여러 mutation을 넣는다.

예:

```text
MessageAdded
ContextPartAdded
```

둘 사이에 process가 죽어 Message만 있고 Context가 없는 상태가 생기지 않게 한다.

Compaction도:

```text
ContextPartAdded(summary)
ContextPartInvalidated(old1)
ContextPartInvalidated(old2)
```

를 하나의 commit으로 만들 수 있다.

---

# 27. PersistencePlugin

저장 경로:

```text
${XDG_DATA_HOME:-~/.local/share}/airicode/
  [projectname]-[projecthash]/
    sessions/
      [sessionhash].jsonl
```

`projecthash`는 canonical project root를 BLAKE3 등 안정적인 hash로 계산한다.

## 27.1 SessionStore

```rust
trait SessionStore: Send + Sync {
    async fn load(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<SessionCommit>>;

    async fn append(
        &self,
        session_id: SessionId,
        commit: &SessionCommit,
    ) -> Result<()>;
}
```

정상적인 상태 변경을 위해 `replace_messages()` 같은 전체 history rewrite API를 만들지 않는다.

## 27.2 commit ordering

Persistence 활성화:

```text
Operation validate
-> SessionCommit 생성
-> disk append
-> reducer apply
-> hooks/runtime event
```

Disk append가 실패하면 memory projection도 advance하지 않는다.

Persistence 비활성화:

```text
validate
-> commit 생성
-> reducer apply
-> runtime event
```

## 27.3 JSONL record

최소 포함:

- schema version
- sequence
- commit ID
- timestamp
- mutations

## 27.4 recovery policy

- empty log 허용
- 잘린 마지막 line은 명시된 정책에 따라 ignore/truncate recovery 가능
- 중간 line corruption은 오류
- sequence gap/duplicate는 오류
- 미지원 schema version은 명확한 오류

---

# 28. Session runtime / Actor

Session은 actor 또는 그에 준하는 단일 state owner로 운영한다.

권장:

```text
mpsc::Receiver<SessionRequest>
watch::Sender<SessionSnapshot>
```

Actor가 Session projection mutation ordering을 소유한다.

하지만 actor가 Provider/Tool을 직접 긴 시간 `.await`하며 멈춰 있으면 안 된다.

권장 구조:

```text
SessionActor
  |
  +-- TurnTask
       |
       +-- Provider request
       +-- Tool execution
       +-- TurnEvent -> SessionActor
```

Actor:

```rust
tokio::select! {
    Some(req) = session_rx.recv() => { ... }
    Some(event) = turn_rx.recv() => { ... }
    _ = cancellation.cancelled() => { ... }
}
```

Tool 안에서 `operations.add_note()` 등을 호출해도 actor가 자기 자신을 기다리는 deadlock이 생기지 않도록 한다.

---

# 29. Canonical Agent Turn State Machine

```text
User input
  -> BeforeMessage
  -> User Message + ContextPart commit
  -> TurnStarted

Provider round
  -> active stored Context snapshot
  -> ContextContribution
  -> provider-neutral message materialization
  -> Tool registry snapshot
  -> BeforeProviderRequest
  -> Provider.request
  -> ProviderStream runtime events
  -> ProviderRoundFinished
  -> Assistant Message commit

Assistant에 ToolCall 존재:
  -> ToolCall을 stable order로 실행
  -> 각 ToolResult commit
  -> 다음 Provider round

ToolCall 없음:
  -> TurnCompleted
```

한 User Message 이후 여러 Provider round가 하나의 Turn에 속한다.

---

# 30. Cancellation

`CancellationToken` hierarchy:

```text
Core
└─ Project
   └─ Session
      ├─ Turn
      │  └─ ToolCall
      ├─ Command
      ├─ Sidequery
      └─ Child/Subagent task
```

정책:

- Turn cancel → Provider stream + 해당 Turn active ToolCall cancel
- ToolCall cancel → 특정 tool 실행 중단
- Session close → active turn / sidequery / command 중단
- Core shutdown → 전체 중단
- `ToolOutput::Stop`은 cancellation 아님

Shell 실행은 가능한 플랫폼에서 child/process group까지 종료한다.

---

# 31. RuntimeEvent

RuntimeEvent는 UI/관찰용 ephemeral event다.

예:

```text
ProviderStreamDelta
ProviderUsageUpdated
ToolExecutionStarted
ToolExecutionFinished
TurnStarted
TurnCompleted
TurnCancelled
RegistryChanged
SessionSnapshotChanged
```

`RuntimeEvent`와 `SessionMutation`은 완전히 다른 개념이다.

PersistencePlugin은 Provider delta나 UI redraw event를 저장하지 않는다.

문자열 기반 generic feature event bus를 Plugin 간 주요 통신 수단으로 사용하지 않는다.

---

# 32. Plugin별 요구사항

## 32.1 PersistencePlugin

책임:

- SessionStore 제공
- JSONL append
- replay
- partial final line recovery
- schema versioning

conversation 의미 자체는 알지 않는다.

---

## 32.2 ProviderOpenAIPlugin

책임:

- OpenAI Provider 등록
- model 목록 제공
- AiriCode Message/Tool → OpenAI request 변환
- OpenAI stream → ProviderEvent 변환
- usage/finish reason mapping
- OpenAI-specific 오류 처리

Core가 OpenAI SDK type을 import하지 않는다.

---

## 32.3 InstructionBasePlugin

책임:

- 기본 coding-agent instruction을 ContextContribution으로 제공
- 필요하면 mode에 따라 instruction variation 제공

Plugin 제거 시 기본 instruction도 사라진다.

Core에는 “너는 coding agent다” 같은 prompt가 들어가지 않는다.

---

## 32.4 InstructionAgentsPlugin

책임:

1. project root의 `AGENTS.md`를 읽고 ContextContribution한다.
2. 파일 read 이후 해당 파일과 가장 가까운 nested `AGENTS.md`를 찾는다.
3. 해당 instruction이 stored Context에 없으면 `add_context_part` 한다.

ToolReadPlugin concrete type을 import하지 않는다.

Generic AfterToolExecutionHook에서 tool identity와 arguments를 검사하는 수준으로 연동한다.

ContextPart metadata에:

```text
source_path = "src/foo/AGENTS.md"
```

를 넣어 중복을 방지한다.

---

## 32.5 ToolReadPlugin

책임:

- Workdir로 파일 읽기
- hashline 형식 출력
- range read
- 과도한 line/byte 요청 거부
- binary/NUL input 거부
- cancellation

---

## 32.6 Hashline

Hashline은 단순 표시 형식이 아니라 **optimistic concurrency control** 역할을 한다.

예:

```text
12:a83f2|fn foo() {
13:82bd1|    bar();
14:c08a1|}
```

Patch가 과거 read에서 본 tag를 사용한다.

현재 파일의 대응 내용이 달라졌다면:

```text
stale patch: file changed since read
```

처럼 실패한다.

공유 Workdir에서 다른 Session/Subagent가 파일을 수정할 수 있으므로 이 검증이 특히 중요하다.

동일한 bytes에 대해 hashline은 deterministic해야 한다.

---

## 32.7 ToolPatchPlugin

흐름:

```text
current bytes 재-read
-> hashline anchor validation
-> stale이면 Failure
-> patched bytes 생성
-> Workdir.write
-> unified diff 생성
-> Diff Note 추가
-> 짧은 ToolResult 반환
```

ToolResult 예:

```text
Updated src/foo.rs (+8/-2)
```

전체 diff는 Note에 둔다.

중요:

- 과거 read 결과를 그대로 신뢰하지 않는다.
- patch 직전에 current bytes를 반드시 다시 본다.
- 가능하면 NativeWorkdir whole-file write는 temp file + atomic rename 형태를 사용한다.
- write 직전과 실제 write 사이의 race까지 완벽히 제거하지 못하더라도 silent wrong patch를 최소화한다.

장기적으로 필요하다면 compare-and-swap 성격의 Workdir API 확장을 검토할 수 있지만 v1 Core에 미리 넣지는 않는다.

---

## 32.8 ToolGrepPlugin

- Workdir-visible files 검색
- output size 제한
- file/line reference 제공
- cancellation

---

## 32.9 ToolShellPlugin

- 반드시 Workdir.execute 사용
- sandbox/worktree layer를 우회하지 않음
- exit status 제공
- stdout/stderr 크기 제한 또는 streaming 정책
- cancellation

서로 다른 Session의 Shell 실행은 동시에 가능하다.

---

## 32.10 ToolQuestionPlugin

- pending question을 UI/plugin state에 기록
- Draft 뒤에 question과 choices 표시
- `ToolOutput::Stop`
- 다음 user input은 정상적인 새 User Message/Turn으로 처리

---

## 32.11 ToolTodoPlugin

Todo를 Core의 고정 model로 추가하지 말고 우선 Plugin-owned state / generic plugin state / Note를 활용한다.

여러 Plugin이 공통으로 필요하다는 근거가 생기기 전까지 Core primitive로 승격하지 않는다.

---

## 32.12 ToolWebfetchPlugin

- URL fetch
- timeout/size limit
- output normalization
- cancellation
- HTTP implementation을 Core에서 격리

---

## 32.13 ForkPlugin

Fork는 같은 SessionGroup 안에 새 Session을 만든다.

복사 대상:

- Messages
- Notes
- active Context
- durable UI state

Workdir은 새로 provision하지 않는다.

runtime에서는 immutable Message의 `Arc`를 공유할 수 있다.

Persistence log는 가능하면 fork된 Session 하나만으로 self-contained replay가 가능하게 만든다.

---

## 32.14 RevertPlugin

Conversation revert는 물리 삭제가 아니다.

```text
MessageInvalidated
ContextPartInvalidated
```

mutation을 만든다.

사용자 UX는 Turn 기준을 권장한다.

```text
/revert <turn>
```

Conversation revert와 filesystem/Git revert는 별개다.

```text
/revert
/worktree-revert
```

Plugin끼리 서로 의존하지 않는다.

---

## 32.15 CompactionPlugin

초기에는 `/compact` 수동 command부터 구현한다.

흐름:

```text
active Context 조회
-> 대상 Low ContextPart 선택
-> 별도 Core request
-> summary 생성
-> atomic commit:
     ContextPartAdded(summary)
     ContextPartInvalidated(old...)
```

Message 자체는 invalidate하지 않는다.

따라서 사용자는 UI에서 과거 대화를 계속 볼 수 있다.

자동 compaction은 provider usage/context budget tracking이 안정된 뒤 추가한다.

---

## 32.16 SidequeryPlugin

목적:

> 현재 conversation Context를 오염시키지 않고 병렬 질문을 수행한다.

흐름:

```text
add Note: Q: ...
-> relevant Context snapshot
-> Core request(sidequery policy)
-> answer
-> update Note: Q + A
```

Sidequery는 일반 conversation Message를 추가하지 않는다.

기본적으로 mutating tool을 주지 않는 것을 권장한다.

- child cancellation token
- configurable concurrency limit

을 둔다.

---

## 32.17 SubagentsPlugin / ToolSubagentsPlugin

Subagent는:

- child Session
- same SessionGroup
- same Workdir
- explicit parent relation
- own Message/Context history
- parent에게 final result만 ToolResult 등으로 반환

Parent Context에 child의 전체 transcript를 자동 복사하지 않는다.

Parent와 child는 같은 Workdir에서 실제로 동시에 작업할 수 있다.

**이를 막기 위한 SessionGroup-wide write lock을 두지 않는다.**

Config 제한:

- max children
- max recursion depth
- optional max concurrent child launches

---

## 32.18 SkillsPlugin

- skill discovery
- `/[skill-name]` dynamic command 등록
- active skill 관리
- active skill ContextContribution
- 필요한 경우 dynamic tool 등록/해제

이 Plugin은 Registry의 runtime add/remove가 제대로 설계되었는지 검증하는 대표 기능이다.

---

## 32.19 McpPlugin

- MCP server process/connection lifecycle
- tool discovery
- dynamic Registry registration/removal
- cancellation
- error isolation
- close/shutdown cleanup

Tool namespace 권장:

```text
mcp.<server>.<tool>
```

MCP는 내부 Tool runtime이 충분히 안정된 뒤 구현한다.

---

## 32.20 GitWorktreePlugin

경로:

```text
${XDG_DATA_HOME:-~/.local/share}/airicode/
  [projectname]-[projecthash]/
    worktree/
      [sessiongrouphash]/
```

### `/worktree-init`

현재 source repository HEAD에서 managed worktree 생성.

SessionGroup logical Workdir이 해당 worktree를 바라보도록 layer 활성화.

### `/worktree-revert`

마지막 completed-turn checkpoint commit으로 managed worktree reset.

### `/worktree-commit`

agent/tool commit을 squash해서 source workdir/repository에 반영.

### `/worktree-discard`

managed worktree를 현재 source repository commit으로 되돌림.

### ToolCall commit

Tool 실행 후 실제 변경이 있다면 commit한다.

변경이 없으면 empty commit을 만들지 않는다.

Commit metadata 권장:

```text
Airicode-Session: ...
Airicode-Turn: ...
Airicode-Tool-Call: ...
```

### 공유 worktree concurrency

같은 SessionGroup의 여러 Session은 동일 managed worktree를 공유한다.

따라서 동시에 Git state를 변경할 수 있다.

Core 전역 lock으로 숨기지 않고 Plugin이 Git race를 명시적으로 다룬다.

예:

- commit 직전 HEAD가 변함
- index state 변경
- 다른 Session이 먼저 commit

안전하게 retry 가능한 경우에만 retry하고, 그렇지 않으면 오류를 surface한다.

초기 버전은 다음 경우 보수적으로 거부해도 된다.

- `/worktree-init` 시 source repository dirty
- `/worktree-commit` 시 source HEAD가 base에서 호환되지 않게 이동

자동 rebase/merge는 v1 비목표다.

---

## 32.21 SandboxPlugin

Sandbox는 WorkdirLayer다.

GitWorktreePlugin concrete type을 알지 않는다.

Backend 예:

- `sandbox_bubblewrap.rs`
- `sandbox_container.rs`

ToolShell만 특별 취급하지 말고 Workdir 계층을 통해 filesystem/execute isolation을 제공한다.

---

# 33. Terminal UI

```text
src/ui/terminal/
  app.rs
  editbar.rs
  editor.rs
  messages.rs
  statusbar.rs
```

## 33.1 statusbar

상단:

- Session title
- selected Provider/Model
- token usage
- context usage
- 실행 상태

## 33.2 messages

created_at 순 rendering:

- User
- Assistant
- ToolCall
- ToolResult
- Note
- Diff Note
- Alert
- streaming Assistant draft

## 33.3 editor

- message editing
- command parsing
- command autocomplete
- argument autocomplete
- question choices interaction

## 33.4 editbar

하단 상태:

- mode
- model
- variant
- input state

## 33.5 event loop

```rust
tokio::select! {
    terminal_input = ... => { ... }
    runtime_event = ... => { ... }
    _ = redraw_tick.tick() => { ... }
}
```

UI는 Core model을 직접 mutate하지 않는다.

Command/Operation만 호출한다.

---

# 34. Error model

Core error category는 안정적인 분류를 둔다.

예:

```text
Config
Registry
Plugin
Session
Provider
Tool
Command
Workdir
Persistence
Cancelled
InvalidState
Unsupported
```

Plugin source error는 chain으로 보존하되 사용자에게는 actionable summary를 보여준다.

Recoverable failure는 상황에 따라:

- ToolOutput::Failure
- Command error
- Alert Note
- status message

로 표현한다.

Persistence corruption 같은 상태 손상은 조용히 무시하지 않는다.

---

# 35. Logging / tracing

처음부터 structured tracing을 사용한다.

주요 field:

```text
project_id
session_group_id
session_id
turn_id
plugin_id
provider_id
tool_id
tool_call_id
commit_id
```

기본 로그에 다음을 남기지 않는다.

- API key
- 전체 environment secret
- 불필요한 provider secret header
- 제한 없는 tool output

Debug mode에서 protocol 진단을 더 노출할 수 있다.

---

# 36. Security baseline

기본 요구사항:

- API key를 Session JSONL에 저장하지 않음
- shell cancellation 시 child process 정리
- untrusted MCP/webfetch input에 timeout/size limit
- JSONL deserialize가 executable object를 만들지 않음
- tool result가 지나치게 커지는 경우 제한

---

# 37. Core 불변식

다음은 문서뿐 아니라 테스트로 고정한다.

1. Plugin은 Session projection을 직접 mutate하지 않는다.
2. durable state 변경은 Operation → SessionCommit → reducer 경로를 통한다.
3. ContextPart는 stable ID를 가진다.
4. Message는 immutable이다.
5. invalidate는 historical data의 물리 삭제가 아니다.
6. Provider streaming delta는 Message로 persistence하지 않는다.
7. `TurnCompleted`는 전체 provider/tool loop가 끝난 뒤 발생한다.
8. SessionGroup의 Session은 하나의 logical Workdir을 공유한다.
9. shared Workdir이라고 해서 Core가 global mutation serialization을 하지 않는다.
10. Plugin/handler 실행 중 Registry lock을 잡지 않는다.
11. Plugin-specific Config type이 Core Config에 들어가지 않는다.
12. Plugin-specific state는 generic namespace를 사용한다.
13. Provider-specific type이 Core model에 들어가지 않는다.
14. persistence replay 결과는 deterministic하다.
15. 하나의 atomic logical Operation이 invalid partial durable state를 만들지 않는다.
16. Plugin 제거 시 해당 기능이 사라지고 Core 수정이 필요하지 않다.
17. UI는 frontend이며 별도의 state owner가 아니다.

---

# 38. 테스트 전략

## 38.1 Pure reducer test

Provider나 async runtime 전에 가장 먼저 만든다.

예:

```text
empty + MessageAdded
MessageAdded + MessageInvalidated
ContextPartAdded + invalidation
NoteAdded + NoteUpdated
conversation message atomic commit
UI durable state update
```

Commit serialize → deserialize → replay 결과가 동일해야 한다.

## 38.2 Registry contract test

- duplicate ID
- priority/order
- dynamic register
- unregister
- owner cleanup
- concurrent registry change 중 snapshot 실행
- handler 실행 중 lock 미보유

## 38.3 Plugin contract test

Plugin 하나만 최소 Core에 넣어서 예상 capability만 등록되는지 검사한다.

예:

- Persistence → SessionStore만
- OpenAI → Provider
- GitWorktree → WorkdirLayer + Commands
- Skills 비활성화/제거 → skill command 제거

## 38.4 Vertical agent integration test

`FakeProvider + InMemoryWorkdir`로:

```text
User
-> Assistant ToolCall
-> ToolResult
-> second Provider round
-> Assistant final
-> TurnCompleted
```

추가:

- Question Stop
- provider stream cancel
- tool cancel
- multiple ToolCalls
- malformed tool arguments
- second provider round failure

## 38.5 Shared Workdir concurrency test

별도 1급 test category로 둔다.

동일 SessionGroup에서 두 개 이상 Session을 동시에 실행해 다음을 검사한다.

### Case A — 같은 파일 read

둘 다 정상.

### Case B — 서로 다른 파일 patch

둘 다 병렬 성공 가능.

### Case C — 같은 파일을 같은 old hashline에서 patch

먼저 반영된 patch 이후 다른 patch가 stale detection으로 실패하는 것이 정상적인 결과가 될 수 있다.

### Case D — shell이 파일을 바꾸는 동안 patch

patch가 현재 bytes를 재검증하며 silent stale apply를 하지 않는지 확인.

### Case E — parent/subagent 동시 수정

Core-level serialization 없이 실제 공유 state를 관찰하는지 확인.

### Case F — Git worktree commit race

GitPlugin이 race를 숨기지 않고 명확히 처리하는지 확인.

테스트의 목적은 deterministic serialization이 아니라 **의도된 concurrency에서 잘못된 silent mutation을 방지하는 것**이다.

## 38.6 Persistence crash/recovery test

- normal replay
- empty log
- partial final line
- corrupt middle line
- wrong sequence
- duplicate sequence
- unsupported schema
- process restart after commit
- streaming 중 crash
- append 직후 crash
- fork replay
- invalidation replay

## 38.7 Real Git integration test

실제 temp Git repo 생성:

- worktree init
- tool change commit
- no-op tool
- turn checkpoint
- revert
- squash/apply
- discard
- concurrent Session commits
- source HEAD divergence
- dirty source rejection

## 38.8 Sandbox integration test

backend별:

- cwd
- read/write boundary
- command execute
- environment
- cancellation
- isolation

## 38.9 TUI test

Rendering 자체보다 view model/reducer를 분리해 테스트한다.

- Message/Note ordering
- streaming draft → final Message 교체
- command completion
- question choices
- statusbar state
- model/mode selection

---

# 39. 구현 전략

모든 Plugin을 동시에 만들지 않는다.

먼저 architecture가 실제로 작동하는 최소 vertical slice를 만든다.

```text
Core
Plugin system
Config
Session actor
Context
FakeProvider
NativeWorkdir
ToolRead
ToolShell
InstructionBase
Persistence
minimal TUI
```

첫 목표 사용자 흐름:

```text
airicode .
-> Project open
-> Session create/open
-> User input
-> Provider stream
-> Assistant ToolCall
-> read/shell 실행
-> ToolResult
-> Provider continuation
-> final Assistant
-> process 강제 종료
-> restart
-> Session 복구
```

이 흐름이 안정적이기 전에는 MCP/Skills/Subagents 같은 복잡한 기능을 붙이지 않는다.

---

# 40. 단계별 전체 구현 계획

## Phase 0 — Architecture Contract

코드를 많이 쓰기 전에 다음을 확정한다.

- ID types
- ownership
- Message immutability
- Context invalidation
- Note update semantics
- SessionCommit/Mutation schema
- Operation/Hook/RuntimeEvent 경계
- Hook failure policy
- Cancellation hierarchy
- Registry conflict policy
- Config namespace
- Shared Workdir concurrency semantics
- Plugin dependency rule

### 완료 조건

- 간단한 `docs/architecture.md` 또는 이 문서가 저장소에 존재
- model skeleton이 compile
- foundational ownership ambiguity가 남지 않음

---

## Phase 1 — Pure Models + Reducer

구현:

```text
IDs
Message
Note
Context
UIState
Provider models
Tool models
Workdir models
Session
SessionGroup
Project
SessionMutation
SessionCommit
SessionState::apply
```

아직 network/provider async runtime은 만들지 않는다.

### 테스트

- mutation replay
- invalidation
- serde roundtrip
- stable TimeSeq ordering
- bad sequence rejection

### 완료 조건

Session durable state를 commit만으로 완전히 재구성 가능.

---

## Phase 2 — Registry + Plugin Bootstrap + Config + Hooks

구현:

- Plugin trait
- PluginRegistrar
- Registry
- RegistrationHandle/unregister
- typed Hooks
- config schema aggregation
- ConfigRead lifecycle
- Project/Session open/close lifecycle

### 완료 조건

- Plugin 0개로 Core boot 가능
- 여러 Plugin을 넣어도 각 capability가 독립적으로 등록
- Plugin 제거 시 다른 Plugin compile dependency 없음

---

## Phase 3 — Operations + SessionActor

구현:

- Session actor
- durable Operations
- query Operations
- SessionCommit path
- reducer apply
- Session snapshot/watch
- RuntimeEvent 기본 구조

### 완료 조건

Plugin/UI가 mutable Session internals를 직접 잡지 않고 모든 state 변경 가능.

---

## Phase 4 — Turn Engine + FakeProvider

구현:

- TurnId
- provider round loop
- ContextContribution
- provider message materialization
- ProviderEvent assembler
- ToolCall assembler
- Tool execution
- ToolResult Message
- Stop semantics
- Cancellation

### 완료 조건

Network 없이 FakeProvider로 완전한 tool-using turn integration test 통과.

---

## Phase 5 — NativeWorkdir + ToolRead + ToolShell

구현:

- NativeWorkdir
- path validation
- read/write/remove
- execute
- process cancellation
- ToolRead
- ToolShell
- hashline primitive

### 완료 조건

FakeProvider가 실제 temp project를 read하고 command 실행 가능.

---

## Phase 6 — PersistencePlugin

구현:

- JSONL schema
- sequence
- append
- replay
- partial tail recovery
- project/session path
- session discovery

### 완료 조건

Core를 drop/kill 후 새 instance가 동일 durable Session snapshot을 복원.

---

## Phase 7 — ProviderOpenAI + InstructionBase

구현:

- OpenAI Provider adapter
- model listing
- config
- streaming
- tool translation
- usage
- finish reason
- base system instruction contribution

### 완료 조건

실제 모델로 `read/shell → ToolResult → continuation` coding-agent loop 성공.

---

## Phase 8 — Minimal Ratatui UI

구현:

- app event loop
- statusbar
- Message/Note rendering by TimeSeq
- streaming draft
- editor
- basic command autocomplete
- editbar
- model selection

### 완료 조건

Terminal에서 기본 coding agent로 dogfood 가능.

---

## Phase 9 — Editing Toolset

순서:

1. hashline 안정화
2. ToolPatch
3. Diff Note
4. ToolGrep
5. ToolTodo
6. ToolQuestion
7. ToolWebfetch

### 특별 완료 조건

- stale hashline patch가 predictably 실패
- 다른 Session이 파일을 수정해도 silent wrong patch를 하지 않음
- Diff 전체를 ToolResult에 넣지 않고 Note로 표시

---

## Phase 10 — GitWorktreePlugin

구현:

- Provision layer
- `/worktree-init`
- Tool change audit commit
- turn checkpoint
- `/worktree-revert`
- `/worktree-commit`
- `/worktree-discard`
- concurrent Git race handling

### 완료 조건

- 동일 SessionGroup의 여러 Session이 하나의 managed worktree 공유
- Core global write lock 없음
- Git race/error가 명확히 surface
- no-op tool에 empty commit 없음

---

## Phase 11 — Sandbox

구현:

- generic Sandbox layer
- bubblewrap backend
- container backend
- cancellation
- environment/path policy

### 완료 조건

ToolRead/ToolShell 코드를 바꾸지 않고 Sandbox layer on/off 가능.

---

## Phase 12 — Fork + Revert

구현:

- ForkPlugin
- parent/fork relation
- snapshot copy
- RevertPlugin
- turn-based invalidation

### 완료 조건

Fork/Revert 때문에 Message/Context/Persistence Core에 special case를 추가하지 않아도 됨.

---

## Phase 13 — Compaction + Sidequery

구현:

- `/compact`
- compaction request
- atomic Context replacement
- Sidequery Q Note
- Sidequery request
- Note update
- concurrency/cancellation limits

그 후 자동 compaction.

### 완료 조건

두 기능 모두 일반 conversation Message를 불필요하게 추가하지 않고 Core `request`를 재사용.

---

## Phase 14 — Subagents

구현:

- child Session
- parent relation
- same SessionGroup
- same Workdir
- result propagation
- depth/count limit
- concurrent parent/child test

### 완료 조건

Subagent를 위해 Core에 별도 filesystem model이나 global workdir lock 추가 불필요.

---

## Phase 15 — InstructionAgents

구현:

- root `AGENTS.md`
- nested nearest `AGENTS.md`
- read 이후 discovery
- duplicate prevention

### 완료 조건

ToolReadPlugin concrete type compile dependency 없음.

---

## Phase 16 — Skills

구현:

- discovery
- dynamic Commands
- activation state
- ContextContribution
- dynamic Tool registration/removal

### 완료 조건

Registry runtime mutation lifecycle이 실제 제품 기능으로 검증됨.

---

## Phase 17 — MCP

구현:

- config
- server lifecycle
- tool discovery
- namespaced registration
- disconnect/unregister
- cancellation
- shutdown cleanup
- failure isolation

### 완료 조건

MCP server를 연결/해제해도 Core restart가 필요하지 않고 unrelated Plugin state에 영향 없음.

---

## Phase 18 — Hardening

집중 영역:

- crash recovery
- cancellation race
- shared-workdir race
- hook failure
- registry mutation during execution
- malformed provider stream
- huge tool output
- Git race
- persistence schema migration
- tracing
- TUI UX

---

# 41. 권장 PR/Commit 순서

실제 개발에서는 다음 정도로 자른다.

1. `core: ids, models, session mutations, reducer`
2. `core: registries, plugin registrar, typed hooks`
3. `core: config bootstrap and lifecycle`
4. `core: operations and session actor`
5. `core: turn engine, fake provider, cancellation`
6. `workdir: native backend`
7. `tools: read and shell`
8. `plugin: append-only persistence`
9. `provider: OpenAI adapter`
10. `plugin: base instructions`
11. `ui: minimal Ratatui application`
12. `utils/tools: hashline and patch`
13. `tools: grep, todo, question, webfetch`
14. `workdir: Git worktree layer`
15. `workdir: sandbox layers`
16. `plugins: fork and revert`
17. `plugins: compaction and sidequery`
18. `plugins: subagents`
19. `plugin: AGENTS.md instructions`
20. `plugin: skills`
21. `plugin: MCP`
22. `hardening: recovery, races, diagnostics, UX`

각 PR은 repository를 buildable/test-green 상태로 남긴다.

Foundational Core redesign과 여러 Plugin 구현을 한 PR에 섞지 않는다.

---

# 42. Core v1 완료 조건

다음을 모두 만족하면 Core v1로 본다.

- Optional Plugin 0개로 Core boot 가능.
- Plugin이 Core 수정 없이 capability 등록 가능.
- cross-plugin concrete import 없음.
- Session durable state는 Operation으로만 변경.
- Persistence replay가 동일 Session 재구성.
- Provider streaming이 text/reasoning/tool/usage/finish/cancel 지원.
- 한 Turn에서 여러 Provider round 지원.
- Question/Stop이 provider history를 깨뜨리지 않음.
- SessionGroup의 Session들이 하나의 Workdir 공유.
- 여러 Session이 shared Workdir에 동시에 접근 가능.
- Core가 global workdir mutation lock을 강제하지 않음.
- hashline이 concurrent edit에 의한 stale patch를 탐지.
- Registry dynamic add/remove 가능.
- Ratatui가 Message/Note/ToolCall/ToolResult/streaming draft 렌더링.
- 실제 OpenAI turn에서 read/edit/run/respond 가능.
- process restart 시 persisted conversation/context 복구.

---

# 43. 초기 제품 dogfood 완료 조건

다음을 만족하면 본격적인 self-hosting/dogfooding 단계에 들어간다.

1. Project open 가능.
2. Session create/reopen/fork/revert 가능.
3. 여러 Session/Subagent가 의도적으로 하나의 Workdir을 공유.
4. concurrent edit가 hidden serialization이 아니라 명확한 success/conflict behavior를 가짐.
5. read/grep/patch/shell workflow 안정적.
6. GitWorktree가 optional/removable.
7. Sandbox가 optional/removable.
8. abrupt termination recovery test 통과.
9. Compaction이 visible history를 삭제하지 않음.
10. Sidequery가 main Context를 오염시키지 않음.
11. Subagent가 같은 SessionGroup에서 동작.
12. root/nested `AGENTS.md` 적용.
13. Skill이 dynamic behavior add/remove.
14. MCP Tool이 dynamic connect/disconnect.
15. Provider-specific code가 Provider Plugin 안에 격리.
16. Terminal UI가 별도 state owner가 아님.

---

# 44. Architecture Review Checklist

새 기능을 merge하기 전에 다음을 확인한다.

## Core boundary

- 이 기능 때문에 feature-specific concept를 Core에 추가하고 있는가?
- Operation / Hook / Registry / metadata / plugin state로 해결할 수 없는가?

## Plugin isolation

- 다른 Plugin concrete type을 import하는가?
- Plugin을 빼면 기능이 깨끗하게 사라지는가?

## State

- durable state를 Operation으로만 바꾸는가?
- SessionMutation으로 replay 가능한가?
- 여러 mutation이 atomic해야 하는가?

## Context

- visible history인가?
- stored provider Context인가?
- transient ContextContribution인가?
- 세 개를 섞고 있지 않은가?

## Concurrency

- 과거 read 이후 파일이 그대로라고 가정하는가?
- 다른 Session/Subagent가 지금 같은 파일을 수정하면 어떻게 되는가?
- 필요한 lock인가, 아니면 유효한 multi-agent behavior를 실수로 직렬화하는가?
- race가 발생했을 때 silent corruption 대신 오류를 surface하는가?

## Persistence

- mutation 직전/직후 process가 죽으면 어떻게 되는가?
- replay가 deterministic한가?

## Cancellation

- 이 task의 token owner는 누구인가?
- subprocess/stream까지 실제로 멈추는가?

## UI

- 이 state는 durable인가 ephemeral인가?
- UI가 Core를 직접 mutate하는가?

## Provider

- provider-specific concept가 Core로 새고 있는가?

여러 항목에서 문제가 발견되면 구현보다 boundary redesign을 먼저 한다.

---

# 45. 최종 아키텍처 요약

AiriCode의 중심축은 네 가지다.

## 45.1 상태 변경

```text
Plugin / UI
    ↓
Operations
    ↓
SessionCommit
    ↓
Reducer
    ↓
Session projection
```

Persistence 사용 시:

```text
Operations
    ↓
SessionCommit
    ↓
append JSONL
    ↓
Reducer
```

## 45.2 행동 확장

```text
Core lifecycle / action
    ↓
Hook
    ↓
Plugin behavior
```

Hook에서 durable state를 바꿀 때는 Operation을 호출한다.

## 45.3 Capability 제공

```text
Plugin 존재
  -> Tool / Provider / Command / WorkdirLayer 존재

Plugin 없음
  -> 기능 없음
```

Runtime register/unregister를 통해 Skills와 MCP를 구현한다.

## 45.4 공유 작업공간

```text
SessionGroup
  ├─ Session A ─┐
  ├─ Session B ─┼──> same Workdir
  └─ Subagent ──┘
```

이 공유는 native workdir에서도 정상이며 의도된 동작이다.

AiriCode는 이를 Core-level global lock으로 직렬화하지 않는다.

대신:

- hashline stale detection
- patch revalidation
- filesystem atomicity
- Git state checks
- backend-specific narrow synchronization

으로 충돌을 다룬다.

---

# 46. 구현 전에 반드시 고정할 결정

다음 항목은 downstream 영향이 크므로 greenfield implementation contract로 간주한다.

1. `ContextPartId`를 둔다.
2. Message는 immutable이다.
3. 삭제 대신 invalidation을 사용한다.
4. Note는 add/update Operation을 가진다.
5. ProviderRequest 전에 Core가 Context를 materialize한다.
6. ToolContext/CommandContext는 Operations로 Core와 통신한다.
7. WorkdirLayerContext에 SessionGroupId가 있다.
8. SessionGroup은 여러 Session에 하나의 logical Workdir을 공유한다.
9. shared Workdir access는 Core가 전역 직렬화하지 않는다.
10. hashline은 text edit의 기본 optimistic stale guard다.
11. `TurnCompleted`는 전체 tool/provider loop 후에 발생한다.
12. RuntimeEvent와 durable SessionMutation은 분리한다.
13. Persistence는 Message history rewrite가 아니라 append-only SessionCommit을 저장한다.
14. Registry는 runtime add/remove를 지원한다.
15. Core Config는 generic plugin config만 보유한다.
16. Plugin은 다른 Plugin을 직접 import하지 않는다.
17. close/shutdown lifecycle을 처음부터 지원한다.

이 항목들이 안정되면 Fork, Revert, Compaction, Sidequery, Subagents, Skills, MCP가 Core를 반복해서 뜯지 않고 구현될 가능성이 높다.

---

# 47. 첫 번째 Milestone — M1

첫 milestone은 의도적으로 작게 잡는다.

```text
AiriCode Greenfield M1

- Core models + reducer
- Plugin/Registry/Hook framework
- Config bootstrap
- Operations
- Session actor
- Turn engine
- FakeProvider
- NativeWorkdir
- ToolRead
- ToolShell
- PersistencePlugin
- ProviderOpenAIPlugin
- InstructionBasePlugin
- minimal Ratatui UI
```

## M1 Acceptance Scenario

```text
1. `airicode .`
2. Project가 열린다.
3. Session이 열리거나 생성된다.
4. 사용자가 coding question을 보낸다.
5. OpenAI Provider가 Assistant stream을 보낸다.
6. Assistant가 read 또는 shell을 호출한다.
7. Tool이 Workdir을 통해 실행된다.
8. ToolResult를 받은 Provider가 답변을 마친다.
9. UI에 전체 Message/Note가 TimeSeq 순으로 표시된다.
10. 프로세스를 강제로 종료한다.
11. AiriCode를 다시 실행한다.
12. durable Session / Message / Note / Context가 정확히 복구된다.
```

M1이 안정화된 뒤 가장 먼저 추가할 것은 **hashline + ToolPatch + shared-workdir concurrency tests**다.

그 이유는 AiriCode가 여러 agent가 하나의 작업공간을 공유하는 것을 정상 동작으로 삼기 때문에, 실제 file mutation의 correctness model을 다른 고급 기능보다 먼저 확립해야 하기 때문이다.

---

# 48. 구현 우선순위 한 줄 요약

```text
상태 모델
→ Plugin/Registry/Hook
→ Operations/Actor
→ Turn engine
→ Workdir
→ Persistence
→ 실제 Provider
→ 최소 TUI
→ 안전한 Editing
→ Worktree/Sandbox
→ Fork/Revert
→ Compaction/Sidequery
→ Subagents
→ Instructions/Skills
→ MCP
→ Hardening
```

핵심은 기능 수를 빠르게 늘리는 것이 아니라, **Plugin을 하나씩 추가해도 Core 경계가 무너지지 않는 구조를 먼저 증명하는 것**이다.
