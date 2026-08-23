# AiriCode
Minimal, yet feature-rich coding agent

## File Structure
* `src/core/`
  * `context.rs`
  * `core.rs`
  * `message.rs`
  * `project.rs`
  * `provider.rs`
  * `session.rs`
  * `tool.rs`
  * `workdir.rs`
  * ...

* `src/core/plugins/`
  * `plugin.rs`
  * `plugin_instruction.rs`
  * `plugin_sandbox.rs`
  * `plugin_tool.rs`
  * `registry.rs`

* `src/providers/`
  * `openai.rs`
  * `openrouter.rs`

* `src/plugins/`
  * `compaction.rs`
  * `fork.rs`
  * `instruction_agents.rs`
  * `instruction_base.rs`
  * `mcp.rs`
  * `persistence.rs`
    * Save sessions in `.local/share/airicode/[projectname]-[projecthash]/[date:yyyy-mm-dd]/[sessionhash].db` (append-only json)
  * `revert.rs`
  * `sandbox_bubblewrap.rs`
  * `sandbox_container.rs`
  * `sidequery.rs`
  * `skills.rs`
  * `subagents.rs`
  * `tool_grep.rs`
  * `tool_patch.rs`
  * `tool_shell.rs`
  * `tool_todo.rs`
  * `tool_webfetch.rs`
  * `workdir_gitworktree.rs`

* `src/ui/`
  * Common fragment interface for plugins
    * Will be decorated with escape hatch (UIHost)
    * e.g. show a dialog -> using UIFragmentDialog::show
           but adding dialog contents -> should implement per each UIHost using UIFragmentDialog::decorate
  * `dialog.rs`
  * `editbar.rs`
    * Model selection, Variant selection, Mode selection
  * `editor.rs`
  * `statusbar.rs`

* `src/ui/terminal/`
  * Terminal implementation
  * `dialog.rs`
  * `editbar.rs`
  * `editor.rs`
  * `statusbar.rs`

## API
Core::new
Core::open_project
Project::open_session
Project::get_workdir
Session::get_message
Session::send_message
Context
Workdir::register
UIFragment::decorate { host }
PluginRegistry::dispatch_hook
PluginRegistry::register_hook
ToolRegistry::register_tool
Plugin::init { core, manager }
Provider::get_models
Provider::request_stream

enum UIHost {
  UIHostTerminal(...) # based on `ratatui`
  # UIHostWeb(...)      will be based on dioxus or other liveviews (will be implemented later)
}

enum HookEvent {
    SessionCreate {
        session: Arc<Session>,
    },

    UserMessage {
        message: Message,
    },

    ...
}

struct HookContext {
  core: Arc<Core>,
  project: Option<Arc<Project>>,
  session: Option<Arc<Session>>
}

enum HookResult<T> {
  Continue(T),
  Cancel { reason: String }
}

struct HookRegistration {
    pub priority: i32,
    pub plugin_id: PluginId,
    ...
}

enum MessagePart {
    Text(String),

    ToolCall {
        id: ToolCallId,
        name: String,
        arguments: serde_json::Value,
    },

    ToolResult {
        call_id: ToolCallId,
        result: ToolResultContent,
    },

    Reasoning {
        text: String,
    },
}

struct ContextPart {
    pub priority: ContextPriority,
    pub source: ContextSource,
}

enum ContextPriority {
    Persistent,
    High,
    Low,
}

struct Message {
    pub id: MessageId,
    pub role: Role,
    pub content: Vec<MessagePart>,
    pub created_at: DateTime<Utc>,
    pub metadata: MessageMetadata,
}

trait Workdir {
    fn root(&self) -> &Path;

    async fn read(
        &self,
        path: &Path,
    ) -> Result<Vec<u8>>;

    async fn write(
        &self,
        path: &Path,
        data: &[u8],
    ) -> Result<()>;

    async fn remove(
        &self,
        path: &Path,
    ) -> Result<()>;

    async fn execute(
        &self,
        command: CommandSpec,
    ) -> Result<CommandOutput>;
}

### Hooks
`session_create`
`user_message`
`agent_message`
`agent_message_chunk`
`tool_call`
`tool_call_callback`
`message_contextmenu`
`compaction`
`sidequery`
`skill_select`

### Config
`compaction.auto`
`compaction.reserved`
`modes.[name].instructions`
`modes.[name].overrides.sandbox`
`modes.[name].preferred_model`
`sandbox.files.[].include`
`sandbox.files.[].exclude`
`sandbox.files.[].verdict`
`sandbox.network.[].include`
`sandbox.network.[].exclude`
`sandbox.network.[].verdict`
`provider.openrouter.token`
...
