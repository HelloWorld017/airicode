네, 가능합니다. 그리고 **Airicode의 `shell` / `patch`에는 오히려 이 방식이 더 자연스럽습니다.**

현재 Airicode의 `ToolDefinition`은 `input_schema: Value`, `Tool::execute(input: Value, ...)`로 되어 있어서 모든 도구 입력을 JSON으로 가정하고 있습니다. ([GitHub][1]) OpenAI Responses API의 custom tool은 JSON Schema 대신 모델이 **임의의 문자열을 그대로 tool input으로 생성**할 수 있습니다. 실제 문서 예제도 `code_exec`에 Python 코드를 raw text로 넘기는 형태입니다. ([OpenAI Developers][2])

따라서 저는 `ToolDefinition`을 이런 방향으로 바꾸는 게 가장 깔끔하다고 봅니다.

```rust
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum ToolInputDefinition {
    JsonSchema(Value),
    Text,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolInput {
    Json(Value),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input: ToolInputDefinition,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn id(&self) -> ToolId;
    fn definition(&self) -> ToolDefinition;

    async fn execute(
        &self,
        input: ToolInput,
        context: ToolContext,
    ) -> Result<ToolOutput>;
}
```

그러면 예를 들어:

```rust
ToolDefinition {
    name: "shell".into(),
    description: "...".into(),
    input: ToolInputDefinition::Text,
}
```

```rust
ToolDefinition {
    name: "patch".into(),
    description: "...".into(),
    input: ToolInputDefinition::Text,
}
```

로 둘 수 있습니다.

OpenAI provider에서는 아주 직접적으로 매핑됩니다.

```text
ToolInputDefinition::JsonSchema(schema)
    -> { "type": "function", "parameters": schema, ... }

ToolInputDefinition::Text
    -> { "type": "custom", ... }
```

즉 `shell`이

```sh
cargo test
```

를 실행한다면 지금처럼 굳이

```json
{
  "command": "cargo test"
}
```

로 감쌀 필요가 없습니다. `patch`도 마찬가지로 diff 자체를 그대로 받을 수 있습니다.

```diff
*** Begin Patch
*** Update File: src/foo.rs
@@
-old
+new
*** End Patch
```

### Streaming도 잘 맞습니다

이 부분은 링크해주신 문서와 특히 잘 맞습니다. Function tool은 JSON argument를

```text
response.function_call_arguments.delta
```

로 스트리밍하고, ([OpenAI Developers][2]) custom tool은 별도로

```text
response.custom_tool_call_input.delta
response.custom_tool_call_input.done
```

를 제공합니다. `delta`도 그냥 `string`이고, `done`에는 완성된 `input: string`이 들어옵니다. ([OpenAI Developers][3])

그래서 OpenAI provider 내부에서도 대략:

```rust
match event {
    FunctionCallArgumentsDelta { delta, .. } => {
        // JSON string buffer에 append
    }

    CustomToolCallInputDelta { delta, .. } => {
        // plain text buffer에 그대로 append
    }

    CustomToolCallInputDone { input, .. } => {
        ToolInput::Text(input)
    }
}
```

처럼 나눌 수 있습니다.

특히 UI에서는 `shell` 명령이나 `patch` 내용을 생성되는 즉시 보여주되, **실제 tool 실행은 `*.done`을 받은 뒤에만** 하는 구조가 좋습니다.

### `input_schema: Option<Value>`보다는 enum을 추천합니다

최소 변경만 생각하면

```rust
pub input_schema: Option<Value>
```

로 두고 `None`이면 text라고 해석할 수도 있지만, 저는 피하는 편이 좋다고 봅니다.

`None`이

* plain text tool인지
* 입력이 없는 tool인지
* schema가 아직 설정되지 않은 것인지

의미가 모호해집니다.

그래서 현재의 `input_schema`를 `input` 또는 `input_definition`으로 바꾸고,

```rust
JsonSchema(Value)
Text
```

두 가지를 명시적으로 표현하는 게 Airicode의 provider-neutral 모델과도 더 잘 맞습니다.

그리고 나중에 필요하다면 OpenAI custom tool이 지원하는 grammar constraint도 확장할 수 있습니다. OpenAI는 custom tool에 Lark/regex grammar를 붙여 raw text 문법을 제한하는 것도 지원합니다. ([OpenAI Developers][2]) 예를 들면 장기적으로:

```rust
pub enum ToolInputDefinition {
    JsonSchema(Value),
    Text,
    // 나중에 정말 필요할 때
    Grammar {
        syntax: GrammarSyntax,
        definition: String,
    },
}
```

정도로 확장할 여지도 있습니다. 다만 **지금 당장은 `JsonSchema | Text` 두 개만 두는 게 맞아 보입니다.**

한 가지 중요한 점은 OpenAI 외 provider입니다. OpenAI에서는 `Text -> custom tool`로 정확히 대응되지만, custom tool을 지원하지 않는 provider에서는 필요하다면

```json
{"input": "..."}
```

형태의 일반 function tool로 provider adapter가 fallback할 수 있습니다. 즉 Airicode core에서 `Text`라는 의미를 보존하고, provider가 자기 API에 맞춰 표현하게 만드는 게 좋습니다.

결론적으로, **`shell`과 `patch` 둘 다 `Text` 입력 Tool로 바꾸는 것을 추천합니다.** OpenAI에서는 각각 custom tool로 내보내면 되고, 지금의 `Value` 중심 `Tool::execute`도 그에 맞춰 `ToolInput::{Json, Text}`로 일반화하는 것이 가장 깔끔합니다. ([OpenAI Developers][2])

[1]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/tool.rs "airicode/src/core/models/tool.rs at master · HelloWorld017/airicode · GitHub"
[2]: https://developers.openai.com/api/docs/guides/function-calling "Function calling | OpenAI API"
[3]: https://developers.openai.com/api/reference/resources/responses/streaming-events "Responses streaming events | OpenAI API Reference"
