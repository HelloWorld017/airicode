# Tool Rework
1. tool_fs_rename.rs, tool_fs_write.rs, tool_fs_delete.rs 의 추가
2. tool_patch.rs 제거 및 다양화
    * GPT용 -> apply_patch
    * 소형 모델용 -> patch
    * 대형 모델용 -> patch_hashline
3. hashline 의 opt-in 가능
    * `tool.enable_hashline` config로 hashline을 opt-in 가능하게 만듬
    * read, grep 툴도 `tool.enable_hashline` 유무에 따라 맞게 변경
4. freeform의 opt-in 가능
    * 기존에 ToolInput의 Text / Json 분리 구조에서 변경
    * ToolInputDefinition를 다음과 같이 변경
        * JSON Schema는 기본
        *  Freeform Tool이 가능한 모델에서 사용할 수 있는 optional Text -> JSON 파서
    * Provider에서 `tool.freeform` config 가 켜져있고 지원하는 provider라면 freeform으로 골라 쓰게끔

그 외:
* 기존의 `add_tool_note`, `add_output_note` 등은 `utils/note.rs` 로 옮기기

## tool_patch의 변경
* plugins/tool_patch를 tool_patch_hashline 으로 바꾸고
* tool_patch.rs / tool_patch_hashline.rs / tool_patch_apply_patch.rs 이렇게 3개의 구조로 정리

### `plugins/tool_patch.rs`

```ts
const replaceEditSchema = Type.Object(
	{
		oldText: Type.String({
			description:
				"Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call.",
		}),
		newText: Type.String({ description: "Replacement text for this targeted edit." }),
	},
	{},
);

const patchSchema = Type.Object(
	{
		path: Type.String({ description: "Path to the file to edit (relative or absolute)" }),
		edits: Type.Array(replaceEditSchema, {
			description:
				"One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
		}),
	},
	{},
);
```

위 스키마에 맞는 `patch` tool을 지원
* 활성화 조건: `tool.enable_hashline === false`

### `plugins/tool_patch_hashline.rs`
1. 기존 `INSERT` 문법 등은 파서로 제공
    * Freeform 일 시 기존 문법 사용
    * 아닐 시 다음과 같은 스키마 제공
    ```
    {
        "operations": [
            { "kind": "insert_before", "path": "./src/aaa.rs", "anchor": "(anchor)", "lines": [ "hello", "world" ] },
            { "kind": "replace", "path": "./src/aaa.rs", "anchor_start": "(anchor)", "anchor_end": "(anchor)", "lines": [ "hello", "world" ] },
        ]
    }
    ```
2. 기존 `ADD path` `DELETE path` 문법은 제거 (`fs_*` 툴 사용)
    * 대신 `DELETE path FROM line:hash TO line:hash` 문법 추가

* 활성화 조건: `tool.enable_hashline === true`

### `plugins/tool_patch_apply_patch.rs`
* GPT용 `apply_patch` 툴의 문법 그대로 구현

## Freeform 변경
Freeform 에 해당하는 툴
* `fs_write`
* `apply_patch`
* `patch_hashline`
* `shell`

=======

현재 `master` 기준으로 보면, 이번 변경은 **툴 구현 추가 → 입력 모델 통합 → Provider 표현 선택 → hashline 기반 툴 활성화 분기** 순서로 진행하는 것이 가장 깔끔합니다. 현재 `ToolInputDefinition`은 `JsonSchema | Text`, `ToolInput`도 `Json | Text`로 나뉘어 있고, OpenAI provider가 이를 그대로 function/custom tool로 매핑하고 있습니다. ([GitHub][1])

## 1. 목표 구조

최종적으로는 Core 입장에서 모든 툴 입력을 **JSON이라는 하나의 canonical representation**으로 통일하는 것을 권장합니다.

```rust
pub struct ToolInputDefinition {
    pub schema: Value,
    pub freeform: Option<FreeformInputDefinition>,
}

pub struct FreeformInputDefinition {
    pub parser: ToolFreeformParser,
}

pub type ToolInput = Value;
```

개념적으로는 다음과 같습니다.

```text
                   ┌─ JSON function call ───────────┐
Model / Provider ──┤                                ├─> canonical JSON -> Tool::execute()
                   └─ freeform text -> text_parser ─┘
```

즉 기존의:

```rust
ToolInputDefinition::JsonSchema(...)
ToolInputDefinition::Text

ToolInput::Json(...)
ToolInput::Text(...)
```

구조를 없앱니다. 현재 이 분기가 `tool.rs`와 OpenAI Responses 변환 로직에 직접 박혀 있기 때문에, 먼저 이 부분을 바꾸는 것이 이후 툴 작업을 단순하게 만듭니다. ([GitHub][1])

`freeform`은 툴의 **유일한 입력 형식**이 아니라 JSON schema의 optional alternative가 됩니다.

---

# 2. Config 추가

전역 툴 동작이므로 plugin별 config보다는 top-level `tool` 설정으로 두는 것을 권장합니다.

```toml
[tool]
enable_hashline = false
freeform = false
```

내부적으로:

```rust
pub struct ToolConfig {
    pub enable_hashline: bool,
    pub freeform: bool,
}
```

기본값:

```text
enable_hashline = false
freeform = false
```

현재 `Config`는 raw config를 그대로 가지고 있으면서 plugin namespace만 별도로 aggregate하고 있으므로, `core/config.rs`에서 `tool`을 typed config로 읽게 만드는 것이 자연스럽습니다. ([GitHub][2])

중요한 점은 두 설정의 의미를 분리하는 것입니다.

* `tool.enable_hashline`

  * read / grep 출력 변경
  * `patch` ↔ `patch_hashline` 선택
* `tool.freeform`

  * provider가 지원할 경우 freeform representation 사용
  * 실제 툴 semantics에는 영향 없음

---

# 3. Freeform 입력 구조 변경

### `src/core/models/tool.rs`

가장 먼저 변경합니다.

추천 형태는:

```rust
pub type ToolFreeformParser = fn(&str) -> Result<Value>;

pub struct ToolInputDefinition {
    pub schema: Value,
    pub freeform_parser: Option<ToolFreeformParser>,
}
```

혹은 builder를 추가해서 툴 정의부를 읽기 좋게 만듭니다.

```rust
ToolInputDefinition::new(schema)

ToolInputDefinition::new(schema)
    .with_freeform_parser(parse_shell_freeform)
```

여기서 중요한 원칙은:

> parser는 `Text -> JSON`만 담당하고 실제 작업은 JSON executor 하나만 사용한다.

예를 들어 `shell`도:

```json
{
  "command": "cargo test"
}
```

를 canonical input으로 정합니다.

freeform일 때:

```text
cargo test
```

가 들어오면 parser가 위 JSON으로 바꿉니다.

따라서 `execute()`에서는 더 이상:

```rust
let ToolInput::Text(command) = input else ...
```

같은 분기가 필요 없습니다. 현재 shell은 정확히 이 구조에 의존하고 있으므로 JSON execution으로 옮깁니다. ([GitHub][3])

### Freeform 대상

요청하신 네 개만 parser를 제공합니다.

```text
fs_write
apply_patch
patch_hashline
shell
```

`patch`는 JSON-only입니다.

---

# 4. Provider의 freeform 선택

현재 OpenAI provider는 `Text`이면 무조건 Responses API `custom`, JSON이면 `function`으로 바꿉니다. ([GitHub][4])

이를 다음 로직으로 바꿉니다.

```rust
use_freeform =
    tool_config.freeform
    && provider_supports_freeform
    && tool.input.freeform_parser.is_some();
```

그리고:

```text
use_freeform = false
  -> type=function
  -> parameters=schema

use_freeform = true
  -> type=custom
```

즉 **freeform 여부를 ToolDefinition 자체가 결정하지 않고 Provider가 최종 결정**합니다.

OpenAI Responses API의 custom tool handling 자체는 이미 구현되어 있으므로 큰 틀은 유지할 수 있습니다. custom tool call streaming과 replay도 현재 별도 처리되고 있습니다. ([GitHub][4])

다만 수신 시점에는:

```text
custom_tool_call.input
        ↓
freeform_parser
        ↓
Value
        ↓
MessagePartContent::ToolCall.arguments
```

으로 변환합니다.

여기서 `MessagePartContent::ToolCall.arguments`에는 반드시 **canonical JSON**을 저장하는 것이 좋습니다.

OpenAI의 원래 `custom_tool_call` 표현은 기존처럼 `provider_data`에 보존하면 됩니다. 그러면 persistence/context replay에서도 semantic representation과 provider-specific representation이 뒤섞이지 않습니다.

---

# 5. FS 툴 추가

새 파일:

```text
src/plugins/
  tool_fs_write.rs
  tool_fs_rename.rs
  tool_fs_delete.rs
```

## `fs_write`

JSON schema:

```json
{
  "type": "object",
  "properties": {
    "path": { "type": "string" },
    "content": { "type": "string" }
  },
  "required": ["path", "content"]
}
```

Freeform parser를 가집니다.

따라서:

```text
JSON provider     -> {"path":"...", "content":"..."}
freeform provider -> textual fs_write syntax
```

가 최종적으로 동일한 JSON executor로 들어갑니다.

파일 생성 및 전체 overwrite를 담당합니다.

## `fs_rename`

JSON-only:

```json
{
  "from": "...",
  "to": "..."
}
```

rename/move만 담당합니다.

## `fs_delete`

JSON-only:

```json
{
  "path": "..."
}
```

파일 삭제만 담당합니다.

이 세 툴이 생기면서 patch 도구에서는 **파일 lifecycle을 완전히 제거**합니다.

```text
create    -> fs_write
overwrite -> fs_write
rename    -> fs_rename
delete    -> fs_delete
edit      -> patch / patch_hashline / apply_patch
```

이렇게 책임을 나누는 것이 좋습니다.

---

# 6. `tool_patch.rs` — 기본 patch

기존 hashline patch 구현을 이 파일에서 제거하고 요청하신 replacement schema 기반으로 새로 구현합니다.

canonical schema:

```json
{
  "path": "./src/foo.rs",
  "edits": [
    {
      "oldText": "old",
      "newText": "new"
    }
  ]
}
```

동작 규칙:

1. 파일을 최초 한 번 읽음.
2. 모든 `oldText`는 **원본 snapshot 기준으로 matching**.
3. 각 `oldText`는 정확히 1회 나타나야 함.
4. edits 간 원본 범위 overlap 금지.
5. 모든 edit 검증 후 한 번에 적용.
6. 하나라도 invalid면 전체 작업 실패.
7. 파일 생성/삭제는 지원하지 않음.

활성화 조건:

```text
tool.enable_hashline == false
```

`patch`에는 freeform parser를 넣지 않습니다.

따라서 작은 모델이나 custom tool을 지원하지 않는 provider에서도 JSON schema를 안정적으로 사용할 수 있습니다.

---

# 7. `tool_patch_hashline.rs`

현재 `tool_patch.rs`의 hashline 기반 엔진을 이쪽으로 이동합니다. 현재 구현이 `hashline::HashLine` snapshot과 text parser에 상당히 결합되어 있으므로 단순 rename 후 정리하는 방식이 안전합니다. ([GitHub][5])

## JSON canonical schema

대략 다음 enum으로 만드는 것이 좋습니다.

```rust
enum PatchHashlineOperation {
    InsertBefore {
        path: String,
        anchor: String,
        lines: Vec<String>,
    },

    InsertAfter {
        path: String,
        anchor: String,
        lines: Vec<String>,
    },

    Replace {
        path: String,
        anchor_start: String,
        anchor_end: String,
        lines: Vec<String>,
    },

    Delete {
        path: String,
        anchor_start: String,
        anchor_end: String,
    },
}
```

JSON에서는:

```json
{
  "operations": [
    {
      "kind": "insert_before",
      "path": "./src/aaa.rs",
      "anchor": "10:abc",
      "lines": ["hello", "world"]
    },
    {
      "kind": "replace",
      "path": "./src/aaa.rs",
      "anchor_start": "20:def",
      "anchor_end": "23:ghi",
      "lines": ["hello", "world"]
    }
  ]
}
```

`delete`는:

```json
{
  "kind": "delete",
  "path": "./src/aaa.rs",
  "anchor_start": "20:def",
  "anchor_end": "23:ghi"
}
```

로 대응시키면 됩니다.

### 기존 text parser

Freeform일 때만 기존 DSL parser를 사용합니다.

유지:

```text
INSERT ...
REPLACE ...
DELETE path FROM line:hash TO line:hash
```

제거:

```text
ADD path
DELETE path
```

특히 기존 patch가 실제로 `ADD path`를 포함하는 문법을 갖고 있으므로, 이 부분은 `fs_write`로 이관합니다. 현재 prompt/parser에도 `ADD path`가 들어가 있습니다. ([GitHub][5])

Freeform parser의 결과도 바로 operation을 실행하지 않고:

```text
DSL
 ↓
parse
 ↓
PatchHashlineInput
 ↓
serde_json::Value
 ↓
공통 JSON executor
```

로 가도록 합니다.

활성화:

```text
tool.enable_hashline == true
```

그리고 freeform은 추가적으로:

```text
tool.freeform == true
&& provider supports freeform
```

일 때만 사용합니다.

---

# 8. `tool_patch_apply_patch.rs`

GPT/OpenAI 계열용 `apply_patch`.

외부 노출 이름:

```text
apply_patch
```

JSON fallback schema는 예를 들면:

```json
{
  "type": "object",
  "properties": {
    "patch": {
      "type": "string"
    }
  },
  "required": ["patch"]
}
```

freeform에서는 GPT가 알고 있는 표준:

```text
*** Begin Patch
*** Update File: ...
@@
...
*** End Patch
```

형식을 그대로 받습니다.

즉 canonical executor는:

```rust
{
    "patch": "..."
}
```

만 처리합니다.

freeform parser는 사실상:

```rust
fn parse_apply_patch(text: &str) -> Result<Value> {
    Ok(json!({ "patch": text }))
}
```

정도로 단순하게 유지할 수 있습니다.

parser/executor 자체는 OpenAI provider와 독립시켜야 합니다. **OpenAI API-specific 코드는 provider에, patch format 구현은 tool에** 둡니다.

---

# 9. Patch variant 선택

여기는 별도의 “variant 선택 layer”를 두는 것을 권장합니다.

단순히 세 plugin을 모두 registry에 넣으면 모델에게:

```text
patch
patch_hashline
apply_patch
```

세 개가 동시에 노출될 수 있기 때문입니다.

최종적으로는 provider/model에 대해 하나의 edit tool만 선택합니다.

```text
enable_hashline=true
        -> patch_hashline

prefers_apply_patch=true
        -> apply_patch

그 외
        -> patch
```

---

# 10. read / grep hashline opt-in

현재 두 툴 모두 사실상 **항상 hashline을 생성합니다**.

`read` description 자체가 hashline 출력임을 전제로 하고 실제 결과도 `hashline::render()`를 무조건 거칩니다. ([GitHub][6])

`grep` 역시 matched file을 다시 읽어서 hashline을 계산하고:

```text
path:line:hash|text
```

로 반환합니다. ([GitHub][7])

이를 다음처럼 바꿉니다.

### `read`

`enable_hashline = true`:

```text
12:a8f|let foo = ...
13:03c|...
```

false:

```text
12|let foo = ...
13|...
```

혹은 기존 plain-read convention이 있다면 그것을 사용합니다.

description도 config에 따라 달라져야 합니다.

```rust
ToolRead {
    enable_hashline: bool,
}
```

처럼 instance가 최종 설정을 가지고 `definition()`과 `execute()`가 동일한 상태를 참조하도록 하는 것이 안전합니다.

### `grep`

true:

```text
src/a.rs:12:a8f|...
```

false:

```text
src/a.rs:12|...
```

false일 때는 matched 파일 전체를 다시 읽어 hashline을 계산할 이유가 없으므로 현재 `rendered_files` cache 자체를 건너뛸 수 있습니다.

---

# 11. 활성화 시점 변경

현재 `ToolReadPlugin`, `ToolGrepPlugin`, `ToolShellPlugin` 등은 `Plugin::init()`에서 곧바로 `register_tool()`을 호출합니다. ([GitHub][6])

하지만 이제:

```text
tool.enable_hashline
tool.freeform
```

을 보고 인스턴스를 구성해야 합니다.

따라서 config-dependent tool plugin은 provider처럼 `ConfigReadHook`에서 최종 tool을 만들어 등록하는 방향이 좋습니다. OpenAI provider도 이미 config를 읽은 뒤 provider를 등록하는 동일한 패턴을 사용합니다. ([GitHub][4])

예:

```text
Plugin::init()
  -> ConfigReadHook 등록

ConfigReadHook::config_read()
  -> ToolConfig 읽기
  -> 적절한 Tool instance 생성
  -> registry.register_tool()
```

특히 `patch`와 `patch_hashline`이 동시에 등록되는 문제를 여기서 해결할 수 있습니다.

---

# 12. note 유틸 이동

현재 `plugins/mod.rs`에:

```rust
add_tool_note()
add_output_note()
```

가 직접 들어 있습니다. ([GitHub][8])

새로:

```text
src/utils/note.rs
```

를 만들고:

```rust
pub(crate) async fn add_tool_note(...)
pub(crate) async fn add_output_note(...)
```

를 이동합니다.

그리고:

```rust
src/utils/mod.rs
```

에서 export한 뒤 각 tool은:

```rust
use crate::utils::note::{add_output_note, add_tool_note};
```

로 통일합니다.

`plugins/mod.rs`는 plugin module/export만 담당하게 만들 수 있습니다.

---

# 13. 최종 파일 구조

이번 작업 이후에는 대략 다음 형태가 좋습니다.

```text
src/
├─ core/
│  ├─ config.rs
│  └─ models/
│     └─ tool.rs
│
├─ plugins/
│  ├─ mod.rs
│  ├─ provider_openai.rs
│  │
│  ├─ tool_read.rs
│  ├─ tool_grep.rs
│  ├─ tool_shell.rs
│  │
│  ├─ tool_fs_write.rs
│  ├─ tool_fs_rename.rs
│  ├─ tool_fs_delete.rs
│  │
│  ├─ tool_patch.rs
│  ├─ tool_patch_hashline.rs
│  └─ tool_patch_apply_patch.rs
│
├─ prompts/
│  ├─ tool_patch.txt
│  ├─ tool_patch_hashline.txt
│  └─ tool_patch_apply_patch.txt
│
└─ utils/
   ├─ hashline.rs
   └─ note.rs
```

현재 `plugins/mod.rs`와 `main.rs`의 export/등록 목록도 새 구조에 맞게 변경해야 합니다. 현재 main에서는 `ToolPatchPlugin` 하나를 직접 등록하고 있습니다. ([GitHub][9])

---

# 14. 구현 순서

의존성을 고려하면 다음 순서가 가장 안전합니다.

1. **Tool input canonicalization**

   * `ToolInput::Text/Json` 제거
   * `ToolInputDefinition` → schema + optional freeform parser
   * 모든 기존 JSON tool migration
   * shell을 JSON canonical input으로 변경

2. **OpenAI freeform negotiation**

   * `tool.freeform`
   * provider capability
   * custom/function 선택
   * custom input → parser → JSON
   * replay/persistence test 갱신

3. **Note utility 이동**

   * `utils/note.rs`
   * 기존 tool import 일괄 수정

4. **FS tool 도입**

   * `fs_write`
   * `fs_rename`
   * `fs_delete`
   * fs_write freeform 지원

5. **기존 patch → hashline 이동**

   * `tool_patch.rs` → `tool_patch_hashline.rs`
   * 기존 테스트 같이 이동
   * `ADD`, old `DELETE path` 제거
   * range DELETE 추가
   * JSON operations schema 추가

6. **새 `patch` 구현**

   * `oldText/newText`
   * snapshot-based matching
   * uniqueness/overlap validation
   * JSON-only

7. **`apply_patch` 구현**

   * 표준 GPT apply_patch grammar
   * JSON fallback + freeform parser

8. **hashline opt-in**

   * `ToolConfig`
   * read
   * grep
   * patch/hashline registration switching

9. **patch variant routing**

   * GPT → apply_patch
   * hashline-enabled 모델 → patch_hashline
   * 기본 → patch

10. **main/module exports 및 통합 테스트**

---

## 테스트에서 반드시 잡아야 할 부분

특히 아래 케이스는 regression test를 두는 것이 좋습니다.

* `tool.freeform=false`이면 `shell`, `fs_write`, `apply_patch`, `patch_hashline` 모두 JSON function tool로 노출
* `tool.freeform=true` + OpenAI이면 네 툴만 custom tool
* `patch`는 어떤 경우에도 function tool
* custom `patch_hashline` 입력과 JSON `operations` 입력이 동일한 canonical JSON/결과 생성
* `enable_hashline=false`에서 read/grep에 hash가 절대 출력되지 않음
* `enable_hashline=true`에서 read/grep anchor가 patch_hashline과 호환
* 기본 patch의 모든 edit이 original snapshot 기준
* 중복 `oldText` 실패
* overlapping edit 실패
* `patch_hashline`의 `ADD` 실패
* file create/delete는 각각 `fs_write`/`fs_delete`만 담당
* persisted freeform tool call도 semantic `arguments`에는 JSON이 저장됨

전체적으로는 **`ToolInput`에서 Text라는 개념 자체를 제거하는 것**이 이번 리워크에서 가장 중요한 설계 포인트라고 봅니다. 그렇게 해야 freeform이 “툴 종류”가 아니라 정말로 **provider가 선택할 수 있는 transport/representation**이 되고, 이후 Anthropic/OpenRouter 같은 provider를 추가해도 툴 구현을 다시 분기할 필요가 없습니다. 현재 OpenAI provider의 `Text -> custom`, `JsonSchema -> function` 직접 매핑도 이 방향으로 가장 깔끔하게 일반화할 수 있습니다. ([GitHub][1])

[1]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/tool.rs "airicode/src/core/models/tool.rs at master · HelloWorld017/airicode · GitHub"
[2]: https://github.com/HelloWorld017/airicode/blob/master/src/core/config.rs "airicode/src/core/config.rs at master · HelloWorld017/airicode · GitHub"
[3]: https://github.com/HelloWorld017/airicode/blob/master/src/plugins/tool_shell.rs "airicode/src/plugins/tool_shell.rs at master · HelloWorld017/airicode · GitHub"
[4]: https://github.com/HelloWorld017/airicode/blob/master/src/plugins/provider_openai.rs "airicode/src/plugins/provider_openai.rs at master · HelloWorld017/airicode · GitHub"
[5]: https://github.com/HelloWorld017/airicode/blob/master/src/plugins/tool_patch.rs "airicode/src/plugins/tool_patch.rs at master · HelloWorld017/airicode · GitHub"
[6]: https://github.com/HelloWorld017/airicode/blob/master/src/plugins/tool_read.rs "airicode/src/plugins/tool_read.rs at master · HelloWorld017/airicode · GitHub"
[7]: https://github.com/HelloWorld017/airicode/blob/master/src/plugins/tool_grep.rs "airicode/src/plugins/tool_grep.rs at master · HelloWorld017/airicode · GitHub"
[8]: https://github.com/HelloWorld017/airicode/blob/master/src/plugins/mod.rs "airicode/src/plugins/mod.rs at master · HelloWorld017/airicode · GitHub"
[9]: https://github.com/HelloWorld017/airicode/blob/master/src/main.rs "airicode/src/main.rs at master · HelloWorld017/airicode · GitHub"
