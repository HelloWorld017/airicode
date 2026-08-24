# OpenAI Responses API Migration PRD

## 1. 목표

Airicode의 OpenAI provider를 Chat Completions 계열 구현에서 **OpenAI Responses API** 기반으로 전환한다.

이번 변경의 핵심은 다음 두 가지다.

1. Responses API의 request / streaming response / function calling을 지원한다.
2. OpenAI가 반환하는 encrypted reasoning 등 provider-specific output을 `MessagePart`에 함께 보존하고, 이후 동일 provider 요청에 다시 전달할 수 있게 한다.

Airicode가 context의 source of truth를 계속 소유하며, OpenAI의 server-managed conversation 또는 server-side compaction은 사용하지 않는다.

---

## 2. 범위

### 포함

- `MessagePart` 데이터 모델 변경
- provider-specific message data 저장
- OpenAI Responses API request 생성
- Responses API streaming event 처리
- encrypted reasoning 저장 및 재전송
- Responses function call / function call output 변환
- persistence 호환성 및 관련 테스트

### 제외

- Server-side compaction
- `/responses/compact`
- `previous_response_id` 기반 conversation state
- OpenAI server-side conversation 관리
- `ProviderFeature::ServerSideCompaction`
- provider data와 동적 `ContextContribution` 사이의 dependency/hash 추적
- provider data의 별도 state machine

---

## 3. Core Message 모델 변경

현재 `MessagePart` enum을 `MessagePartContent`로 변경하고, `MessagePart`를 semantic content와 provider-specific data를 함께 가지는 struct로 변경한다.

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessagePart {
    pub content: Option<MessagePartContent>,

    #[serde(default)]
    pub provider_data: Option<ProviderData>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MessagePartContent {
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderData {
    pub provider_id: ProviderId,
    pub data: Value,
}
```

### 규칙

- `content`는 Airicode가 이해하는 provider-independent semantic representation이다.
- `provider_data`는 해당 part를 생성한 provider가 이해하는 opaque representation이다.
- `provider_data`는 Core, plugin, UI에서 해석하지 않는다.
- 동일 provider에 다시 요청할 때만 해당 `provider_data`를 사용할 수 있다.
- 다른 provider로 전환하면 `provider_data`를 무시하고 `content`를 사용한다.
- `content == None && provider_data == None`인 `MessagePart`는 허용하지 않는다.
- provider-specific output 중 generic representation이 없는 item은 `content: None`으로 저장할 수 있다.

### 기존 생성 코드 변경

기존:

```rust
MessagePart::Text { text }
```

변경:

```rust
MessagePart {
    content: Some(MessagePartContent::Text { text }),
    provider_data: None,
}
```

반복되는 생성을 줄이기 위해 다음 helper를 제공한다.

```rust
impl MessagePart {
    pub fn text(text: impl Into<String>) -> Self;
    pub fn reasoning(text: impl Into<String>) -> Self;
    pub fn tool_call(id: ToolCallId, name: String, arguments: Value) -> Self;
    pub fn tool_result(call_id: ToolCallId, summary: String, result: ToolOutput) -> Self;
    pub fn provider_only(provider_id: ProviderId, data: Value) -> Self;
    pub fn with_provider_data(self, provider_id: ProviderId, data: Value) -> Self;
}
```

`Message::text()` 및 runtime/tool-result 생성 코드는 이 helper를 사용하도록 변경한다.

---

## 4. Provider output 수집 변경

현재 runtime은 `TextDelta`, `ReasoningDelta`, `ToolCallDelta`를 직접 조립해서 최종 `MessagePart`를 생성한다.

Responses API에서는 최종 output item에 encrypted reasoning, response item id 등 streaming delta만으로 복원할 수 없는 정보가 있으므로, **provider가 완료된 logical output item에 대한 `MessagePart`와 `provider_data`를 runtime에 전달할 수 있어야 한다.**

요구사항:

- streaming delta는 기존처럼 실시간 UI/runtime event 용도로 유지한다.
- provider는 output item 완료 시 최종 semantic `MessagePartContent`와 optional `ProviderData`를 전달할 수 있어야 한다.
- runtime이 commit하는 최종 `Message.content`에는 이 완료된 part가 사용되어야 한다.
- provider-only output item도 `content: None`인 `MessagePart`로 commit할 수 있어야 한다.
- output item 순서는 provider response의 원래 순서를 보존한다.
- tool execution에 사용할 `ToolCall` 정보는 최종 part의 `MessagePartContent::ToolCall`에서 얻는다.

구체적인 `ProviderEvent` variant 이름은 구현 시 결정할 수 있으나, 최종 output item을 `MessagePart` 단위로 전달할 수 있어야 한다.

---

## 5. OpenAI Provider: Responses API 전환

OpenAI provider의 generation endpoint를 Responses API로 변경한다.

기본 request 정책:

```json
{
  "stream": true,
  "store": false,
  "include": ["reasoning.encrypted_content"]
}
```

### 사용하지 않는 항목

- `previous_response_id`
- server-side conversation
- server-side compaction
- `context_management` compaction

Airicode가 현재 active context에서 매 request마다 `ProviderRequest.messages`를 구성하는 기존 흐름을 유지한다.

---

## 6. Message -> Responses input 변환

각 `MessagePart`에 대해 다음 규칙을 적용한다.

### 6.1 동일 OpenAI `provider_data`가 있는 경우

```text
provider_data.provider_id == openai
```

이면 해당 native representation을 Responses input item으로 재사용한다.

이 경우 같은 part의 `content`를 추가로 encode하지 않는다.

즉:

```text
matching provider_data
    -> native replay

otherwise
    -> semantic content encode
```

### 6.2 다른 provider의 `provider_data`

무시하고 `content`를 generic하게 Responses input으로 변환한다.

### 6.3 `content: None`

- 동일 OpenAI `provider_data`가 있으면 native item을 replay한다.
- OpenAI용 provider data가 없으면 해당 part를 input에서 생략한다.

### 6.4 `MessagePartContent` mapping

- `Text` -> Responses message text
- `ToolCall` -> Responses `function_call`
- `ToolResult` -> Responses `function_call_output`
- `Reasoning` -> generic reasoning text 자체는 임의의 Responses reasoning item으로 재구성하지 않는다.
  - OpenAI `provider_data`가 있으면 native reasoning item을 replay한다.
  - 없으면 input에서 reasoning part를 생략한다.

---

## 7. Responses output -> MessagePart 변환

OpenAI Responses output item을 semantic content와 native provider data로 변환한다.

### Reasoning item

```rust
MessagePart {
    content: reasoning_summary.map(|text| {
        MessagePartContent::Reasoning { text }
    }),
    provider_data: Some(ProviderData {
        provider_id: OPENAI_PROVIDER_ID,
        data: raw_reasoning_item,
    }),
}
```

- encrypted reasoning은 `provider_data.data`에 포함된 raw Responses item으로 보존한다.
- reasoning summary가 없으면 `content: None`을 허용한다.

### Assistant message item

```rust
MessagePart {
    content: Some(MessagePartContent::Text { text }),
    provider_data: Some(ProviderData {
        provider_id: OPENAI_PROVIDER_ID,
        data: raw_message_item,
    }),
}
```

Responses의 native message item을 보존하여 stateless continuation 시 그대로 재사용할 수 있게 한다.

### Function call item

```rust
MessagePart {
    content: Some(MessagePartContent::ToolCall {
        id: ToolCallId::from_external(call_id),
        name,
        arguments,
    }),
    provider_data: Some(ProviderData {
        provider_id: OPENAI_PROVIDER_ID,
        data: raw_function_call_item,
    }),
}
```

Airicode의 `ToolCallId`에는 Responses의 `call_id`를 사용한다.

Responses item의 `id` (`fc_*`) 등 OpenAI-specific identity는 `provider_data`에 보존한다.

### Generic representation이 없는 output item

필요한 Responses output item인데 Airicode semantic type이 없으면 다음과 같이 저장한다.

```rust
MessagePart {
    content: None,
    provider_data: Some(ProviderData {
        provider_id: OPENAI_PROVIDER_ID,
        data: raw_item,
    }),
}
```

---

## 8. Tool loop 변경

Responses function calling에 맞게 다음 규칙을 적용한다.

- `function_call.call_id`를 Airicode `ToolCallId`로 사용한다.
- tool 실행 결과는 기존 `MessagePartContent::ToolResult`로 저장한다.
- OpenAI request 생성 시 `ToolResult.call_id`를 Responses `function_call_output.call_id`로 전달한다.
- 이전 assistant `function_call`에 OpenAI `provider_data`가 있으면 raw native item을 replay한다.
- parallel function calls를 지원하며 각 `call_id`를 독립적으로 유지한다.

---

## 9. Persistence

`MessagePart.provider_data`는 session persistence 대상이다.

요구사항:

- serialize / deserialize 후 `ProviderData.data`가 손실 없이 보존되어야 한다.
- encrypted reasoning을 로그용 문자열로 변환하거나 별도 필드로 추출하지 않는다.
- 기존 persisted message를 읽을 수 있도록 migration 또는 backward-compatible deserialization을 제공한다.
- 기존 `MessagePart` enum 형식의 persisted data가 이미 실사용되고 있다면 명시적 migration을 추가한다.
- persistence round-trip 이후에도 다음 OpenAI request에 native provider data가 동일하게 전달되어야 한다.

---

## 10. Context / Compaction 동작

Context 구조는 변경하지 않는다.

```text
ContextPart
  -> MessageId
  -> Message
  -> MessagePart[]
       -> content
       -> provider_data
```

따라서 기존 client-side context selection / invalidation / compaction이 Message를 context에서 제외하면 그 Message 내부의 provider data도 자동으로 다음 request에서 제외된다.

이번 PR에서는 provider data와 동적 `ContextContribution` 사이의 drift를 별도로 추적하지 않는다.

---

## 11. 변경하지 않는 Core API

다음 구조는 유지한다.

```rust
pub struct ProviderRequest {
    pub model: String,
    pub messages: Vec<Arc<Message>>,
    pub tools: Vec<ToolDefinition>,
    pub cancellation: CancellationToken,
}
```

또한 다음은 변경하지 않는다.

- `Context`
- `ContextPart`
- `ContextPriority`
- `ContextContribution`
- provider registry 구조
- client-side compaction 정책

---

## 12. 테스트 요구사항

### Message model

- `MessagePartContent`의 모든 기존 variant serialize / deserialize
- `provider_data: None` round-trip
- `provider_data: Some(...)` round-trip
- `content: None + provider_data: Some(...)` round-trip
- `content: None + provider_data: None` 생성 방지

### OpenAI request mapping

- user text -> Responses input message
- assistant text with OpenAI provider data -> native item replay
- 다른 provider data -> 무시 후 generic content encode
- OpenAI encrypted reasoning -> native reasoning item replay
- generic reasoning without OpenAI provider data -> 임의 reasoning item을 생성하지 않음
- `ToolResult` -> `function_call_output`

### OpenAI response mapping

- output text -> `MessagePartContent::Text`
- reasoning summary + encrypted content -> `Reasoning + ProviderData`
- encrypted reasoning only -> `content: None + ProviderData`
- function call -> `ToolCall + ProviderData`
- unknown/opaque output item -> provider-only MessagePart

### Multi-round integration

1. OpenAI reasoning model 호출
2. encrypted reasoning을 포함한 response 수신
3. Message persistence
4. deserialize
5. 다음 user/tool message 추가
6. 다음 `/responses` request 생성
7. 이전 OpenAI native output item이 input에 재전송되는지 확인

### Tool integration

- single function call
- parallel function calls
- `call_id` 유지
- function result 후 다음 provider round

### Context integration

- context에서 Message가 invalidated되면 해당 Message의 provider data도 request에서 빠짐
- client-side compaction으로 이전 Message가 context에서 제거되면 encrypted reasoning도 함께 빠짐
- provider switch 시 OpenAI provider data가 다른 provider에 전달되지 않음

---

## 13. 완료 조건

다음 조건을 모두 만족하면 migration을 완료한 것으로 본다.

- OpenAI provider가 `/responses` streaming API를 사용한다.
- 기존 text / reasoning summary / function calling 기능이 동작한다.
- `MessagePart`가 `content + provider_data` 구조를 사용한다.
- OpenAI response output의 native representation을 Message에 persistence할 수 있다.
- encrypted reasoning이 `store: false` 환경에서 다음 Responses request로 재전송된다.
- tool call의 `call_id`가 round-trip 동안 유지된다.
- 다른 provider는 OpenAI `provider_data`에 의존하지 않는다.
- Context / compaction 구조에는 새로운 provider-specific state가 추가되지 않는다.
- server-side compaction 및 `previous_response_id`를 사용하지 않는다.

---

## 14. 구현 순서

1. `MessagePart` -> `MessagePartContent + MessagePart` 모델 변경
2. `ProviderData` 추가 및 persistence 대응
3. runtime/provider stream에서 finalized `MessagePart`를 보존할 수 있게 변경
4. OpenAI request encoder를 Responses API 형식으로 교체
5. Responses SSE parser 구현
6. output item -> `MessagePart` + `ProviderData` mapping
7. function call / function call output 연결
8. encrypted reasoning stateless round-trip 구현
9. persistence / provider switch / client-side compaction integration test 추가

