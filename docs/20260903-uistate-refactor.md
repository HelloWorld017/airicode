현재 구조를 기준으로 보면 이 리팩터링은 꽤 깔끔하게 정리할 수 있습니다. 핵심은 **`SessionState.ui`에는 재시작 후에도 복원되어야 하는 값만 남기고**, UI 동작 명령은 별도의 **Core-scoped UI Event Bus**로 분리하는 것입니다.

현재 `UIState`는 `DurableUIState + EphemeralUIState` 구조이고, durable 안에는 `selected_model / selected_mode / selected_variant / plugin_state`가 들어 있습니다. 반면 실제 터미널의 draft·cursor·scroll 등은 이미 상당 부분 `EditorState`, `scroll_offset`, `expanded`, `streaming` 같은 `TerminalApp` 내부 상태로 관리되고 있습니다. ([GitHub][1])

## 목표 구조

```rust
pub struct UIState {
    pub selected_model: Option<ModelRef>,
    pub selected_mode: Option<String>,
    pub selected_variant: Option<String>,
}

pub enum UIEvent {
    OpenSession {
        session_id: SessionId,
    },
}
```

`SessionId` 자체에 `SessionGroupId`가 포함되어 있고 `group_id()`로 복원 가능하므로 `OpenSession`에 둘 다 넣을 필요는 없습니다. ([GitHub][2])

그리고 개념적으로 다음처럼 분리합니다.

```text
SessionState
└── ui: UIState                # durable

ui/terminal/TerminalApp
├── editor / cursor
├── scroll
├── expanded
├── hover
├── streaming
└── 기타 UI-local state        # ephemeral

Core
└── UIEventBus
    └── UIEvent::OpenSession   # transient command/event
```

---

# 1. `UIState`를 완전히 durable state로 단순화

### `src/core/models/ui_state.rs`

`DurableUIState`, `EphemeralUIState`를 모두 제거합니다.

현재:

```rust
pub struct DurableUIState {
    pub selected_model: Option<ModelRef>,
    pub selected_mode: Option<String>,
    pub selected_variant: Option<String>,
    pub plugin_state: Metadata,
}

pub struct EphemeralUIState {
    pub draft: String,
    pub cursor: usize,
    pub scroll: u16,
}

pub struct UIState {
    pub durable: DurableUIState,
    pub ephemeral: EphemeralUIState,
}
```

변경:

```rust
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UIState {
    pub selected_model: Option<ModelRef>,
    pub selected_mode: Option<String>,
    pub selected_variant: Option<String>,
}
```

이렇게 하면 이름 자체가 정확해집니다.

> `UIState` = 세션과 함께 저장되는 UI preference/state

별도로 `Durable`이라는 수식어를 붙일 이유가 없어집니다.

---

# 2. `plugin_state` 완전 제거

현재 `update_plugin_state()`는 기존 `UIState`를 snapshot한 뒤 `ui.durable.plugin_state`를 수정하고 다시 전체 UI state를 commit합니다. ([GitHub][3])

이를 전부 제거합니다.

### 삭제 대상

```rust
Operations::update_plugin_state(...)
```

그리고:

```rust
use super::message::Metadata;
```

및

```rust
UIState::plugin_state
```

도 제거합니다.

플러그인별 durable state가 앞으로 정말 필요해진다면 그것은 `UIState`가 아니라 별도의 plugin persistence abstraction으로 설계하는 것이 맞습니다. 이번 리팩터링에서는 별도 대체물을 만들지 않는 편이 좋습니다.

---

# 3. Session mutation도 `UIStateUpdated`로 평탄화

현재 persistence mutation 역시 이름에 durable 개념이 노출되어 있습니다.

```rust
SessionMutation::DurableUIStateUpdated {
    state: DurableUIState,
}
```

그리고 replay 시:

```rust
self.ui.durable = state.clone()
```

형태입니다. ([GitHub][4])

이를 다음처럼 변경합니다.

```rust
SessionMutation::UIStateUpdated {
    state: UIState,
}
```

apply:

```rust
SessionMutation::UIStateUpdated { state } => {
    self.ui = state.clone();
}
```

`Operations`도:

```rust
pub async fn update_ui_state(
    &self,
    state: UIState,
) -> Result<SessionCommit>
```

으로 단순화합니다.

즉 persistence 관점에서도 이제:

```text
SessionCommit
  -> UIStateUpdated
      -> SessionState.ui
```

한 단계만 존재합니다.

---

# 4. Ephemeral state는 전부 `ui/`로 이동

별도의 `EphemeralUIState` replacement struct를 core에 만들 필요는 없습니다.

현재 `TerminalApp` 자체가 이미 다음과 같은 상태를 소유하고 있습니다.

```rust
editor
statusbar
editbar

streaming
reasoning
tool_streaming

scroll_offset
max_scroll

expanded
hovered
hit_regions
message_area
transcript
content_height
```

([GitHub][5])

따라서 기존 `EphemeralUIState`의:

```rust
draft
cursor
scroll
```

은 사실상 각각:

```text
draft/cursor -> EditorState
scroll       -> TerminalApp::scroll_offset
```

에 해당하게 만들면 됩니다.

중요한 규칙은 하나만 두면 됩니다.

> `src/core/models/ui_state.rs`에는 TUI 구현 세부사항이 절대로 들어가지 않는다.

예를 들어 앞으로:

* command palette 열림 여부
* hover
* selected transcript item
* modal
* terminal width
* scroll position
* editor cursor

등은 전부 `src/ui/**` 내부에 둡니다.

---

# 5. 실제 UI에서도 durable `UIState`를 source of truth로 사용

이 부분이 같이 들어가는 것이 좋습니다.

현재 `TerminalApp::new()`는 session UI state를 읽지 않고 전달받은 model과 `"build"`, `"default"`를 이용해 별도의 UI 상태를 초기화하고 있습니다. ([GitHub][5])

리팩터링 후에는 시작 시:

```rust
let ui_state = session.snapshot().ui;
```

를 읽어서:

```text
selected_model
selected_mode
selected_variant
```

를 `StatusBarState` / `EditBarState`에 적용합니다.

값이 없는 경우에만 기존 default를 사용합니다.

반대로 사용자가 model/mode/variant를 변경하면:

```rust
operations.update_ui_state(...)
```

를 통해 저장합니다.

따라서 상태 흐름은:

```text
session restore
    ↓
SessionState.ui
    ↓
TerminalApp initialization
```

그리고:

```text
model/mode/variant 변경
    ↓
Operations::update_ui_state
    ↓
SessionCommit
```

이 됩니다.

draft나 scroll 변화에는 commit을 발생시키지 않습니다.

---

# 6. `RuntimeEvent`와 별도의 `UIEvent` 추가

현재 `RuntimeEvent`는 `TurnStarted`, provider streaming, tool execution, `SessionSnapshotChanged` 등 **특정 SessionHost의 실행 상태**를 전달하는 broadcast입니다. `SessionHost`마다 별도의 channel이 생성됩니다. ([GitHub][6])

여기에 `OpenSession`을 넣는 것은 피하는 편이 좋습니다.

예를 들어:

```rust
RuntimeEvent::OpenSession(...)
```

은 의미상 이상합니다.

`RuntimeEvent`는:

> 이 session runtime에서 무슨 일이 일어났는가

이고,

`UIEvent`는:

> UI가 무엇을 해주기를 원하는가

이기 때문입니다.

새 파일을 추가하는 편이 명확합니다.

```text
src/core/models/ui_event.rs
```

초기에는 아주 작게:

```rust
#[derive(Clone, Debug)]
pub enum UIEvent {
    OpenSession {
        session_id: SessionId,
    },
}
```

정도만 둡니다.

미래에 필요한 경우:

```rust
ShowNotification { ... }
OpenEditor { ... }
FocusInput
```

등을 추가할 수 있지만 지금 미리 만들지는 않습니다.

---

# 7. UI Event Bus는 `SessionHost`가 아니라 `Core` scope로

이 부분이 가장 중요합니다.

현재 `RuntimeEvent` broadcast는 `SessionHost`마다 하나씩 만들어집니다. ([GitHub][7])

하지만 UI Event Bus를 같은 방식으로 만들지는 않는 것을 권합니다.

예를 들어:

```text
Session A의 plugin command
    ↓
새 Session B 생성
    ↓
UIEvent::OpenSession(B)
```

라는 이벤트는 **Session A의 runtime event가 아니라 application-level UI event**입니다.

따라서:

```rust
pub struct Core {
    ...
    ui_events: broadcast::Sender<UIEvent>,
}
```

처럼 `Core`당 하나만 존재하게 합니다.

`CoreBuilder::build()` / `Core::new()`에서:

```rust
let (ui_events, _) = broadcast::channel(...);
```

을 생성합니다.

그리고:

```rust
impl Core {
    pub fn subscribe_ui_events(
        &self,
    ) -> broadcast::Receiver<UIEvent> {
        self.ui_events.subscribe()
    }
}
```

를 제공합니다.

---

# 8. bus를 operation 에서 expose

```rust
impl Operations {
    pub fn emit_ui_event(&self, event: UIEvent) -> Result<()> {
    }
}
```

정도만 공개합니다.

기존 `RuntimeEvent`용 `emit()`은 그대로 internal API로 둡니다. 현재도 `Operations::emit(RuntimeEvent)`은 `pub(crate)`입니다. ([GitHub][3])

결과적으로 plugin command에서는:

```rust
context.operations.emit_ui_event(
    UIEvent::OpenSession {
        session_id,
    },
)?;
```

처럼 사용할 수 있습니다.

---

# 9. Terminal UI가 두 event stream을 독립적으로 처리

현재 `TerminalApp`은:

```rust
let mut events = self.session.subscribe();
```

하여 `RuntimeEvent`만 받고 있습니다. ([GitHub][5])

이를 개념적으로:

```rust
runtime_events
ui_events
```

두 개로 나눕니다.

```text
RuntimeEvent
→ streaming/tool/status/transcript 갱신

UIEvent
→ session 전환, 화면 navigation 등
```

`OpenSession` 수신 시:

```rust
UIEvent::OpenSession { session_id } => {
    let session = core
        .load_session(session_id, session_id.group_id())
        .await?;

    self.open_session(session)?;
}
```

같은 경로를 거칩니다.

이를 위해 `TerminalApp`이 지금의 `SessionHandle`만 갖는 대신 `Core` clone도 하나 들고 있는 편이 가장 단순합니다. `Core` 자체가 `Clone`이므로 별도의 `SessionManager` abstraction까지 만들 필요는 없습니다. ([GitHub][9])

세션 전환 로직은 하나의 메서드로 모읍니다.

```rust
fn set_session(&mut self, session: SessionHandle) {
    self.engine = session.turn_engine();
    self.session = session;

    // snapshot에서 durable UI state hydrate
    // transcript refresh
    // session-local ephemeral UI reset
}
```

새 세션으로 넘어갈 때 editor/scroll/hover 같은 ephemeral 상태가 자연스럽게 초기화되게 합니다.

---

# 구현 순서

1. **UI state model 평탄화**

   * `DurableUIState` 삭제
   * `EphemeralUIState` 삭제
   * `plugin_state` 삭제
   * `UIState`에 3개 durable field 직접 배치

2. **Persistence mutation 정리**

   * `DurableUIStateUpdated` → `UIStateUpdated`
   * replay/apply 수정
   * `Operations::update_ui_state(UIState)` 수정
   * `update_plugin_state()` 삭제

3. **Terminal state 연결**

   * `session.snapshot().ui`로 model/mode/variant 초기화
   * ephemeral 값은 `TerminalApp`/`EditorState`에서만 관리
   * model/mode/variant 변경 시에만 UI state commit

4. **UI Event 모델 추가**

   * `models/ui_event.rs`
   * 최초 이벤트는 `OpenSession { session_id }` 하나

5. **Core-scoped UI Event Bus 추가**

   * `Core`에 `broadcast::Sender<UIEvent>`
   * `Core::subscribe_ui_events()`
   * `Operations::emit_ui_event()` 제공

6. **Terminal UI 이벤트 처리**

   * `RuntimeEvent` receiver와 `UIEvent` receiver 분리
   * `OpenSession` 수신 시 Core를 통해 target session load/open
   * engine/session/runtime receiver 교체
   * session-local ephemeral 상태 reset

7. **테스트/fixture 수정**

   * `plugin_state` 관련 테스트 삭제

## 최종적으로 원하는 경계

```text
                  durable
                    │
                    ▼
              SessionState.ui
                    │
          ┌─────────┴─────────┐
          │                   │
      persistence          Terminal UI
                              │
                              │ ephemeral
                              ▼
                       TerminalApp state


plugin / operation
        │
        │ UIEvent
        ▼
 Core UI Event Bus
        │
        ▼
   Terminal UI
        │
        └── OpenSession / navigation
```

이 구조의 장점은 `UIState`가 더 이상 “UI에서 쓰는 아무 상태나 넣는 곳”이 아니라 **세션에 영속되는 UI 선택값**이라는 아주 좁은 의미를 갖게 된다는 점입니다. `RuntimeEvent`도 실행 이벤트라는 역할을 유지하고, `UIEvent`는 UI navigation/command라는 완전히 별개의 역할을 갖습니다.

기존 persisted `DurableUIStateUpdated`와의 backward compatibility는 현재 프로젝트 단계라면 별도의 migration/serde alias를 만들지 않고 **breaking schema change로 정리하는 것**을 권합니다. 호환 레이어를 추가하면 이번 리팩터링에서 제거하려는 durable/ephemeral 이중 구조가 코드에 계속 흔적으로 남게 됩니다.

[1]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/ui_state.rs "airicode/src/core/models/ui_state.rs at master · HelloWorld017/airicode · GitHub"
[2]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/id.rs "airicode/src/core/models/id.rs at master · HelloWorld017/airicode · GitHub"
[3]: https://github.com/HelloWorld017/airicode/blob/master/src/core/operations/request.rs "airicode/src/core/operations/request.rs at master · HelloWorld017/airicode · GitHub"
[4]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/session.rs "airicode/src/core/models/session.rs at master · HelloWorld017/airicode · GitHub"
[5]: https://github.com/HelloWorld017/airicode/blob/master/src/ui/terminal/app.rs "airicode/src/ui/terminal/app.rs at master · HelloWorld017/airicode · GitHub"
[6]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/events.rs "airicode/src/core/models/events.rs at master · HelloWorld017/airicode · GitHub"
[7]: https://github.com/HelloWorld017/airicode/blob/master/src/core/session.rs "airicode/src/core/session.rs at master · HelloWorld017/airicode · GitHub"
[8]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/command.rs "airicode/src/core/models/command.rs at master · HelloWorld017/airicode · GitHub"
[9]: https://github.com/HelloWorld017/airicode/blob/master/src/core/core.rs "airicode/src/core/core.rs at master · HelloWorld017/airicode · GitHub"

