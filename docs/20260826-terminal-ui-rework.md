전체 디자인을 갈아엎고 싶어. 디자인을 구현하기 위한 코드 변경의 플랜을 짜줘 (src/ui/terminal 참고)

1. 메세지 영역은 PgUp / PgDown / 마우스 스크롤로 스크롤 되어야함.
2. 메세지 영역의 Frame 없이 Margin을 갖게 함
   1. 메세지 영역에는 Text와 Box가 있음
   2. Box는 패딩과 배경색을 가짐
3. 각 Message는 다음과 같이 렌더함
   1. Agent가 보낸 메시지일 경우 Text로 렌더
   2. User가 보낸 메시지일 경우 Box로 렌더
      1. 내용은 Mode\n\nContents 형식이 돼야함
      2. Mode는 현재 Mode에 따라 색이 달라야함 (Build -> 파란색, Plan -> 노란색, 그 외의 Mode는 알파벳순으로 색상 할당)
4. tool call 과 tool result는 기본적으로 보이지 않음
   1. 대신, 각 tool\_\* plugin에서 그 결과를 Note로 쏴줌
   2. NoteContent에 Subtle을 추가해서 메세지박스처럼 렌더되는 노트 (Info)와, secondary text 한 줄로 렌더되는 노트를 구분
   3. find, grep, read, webfetch등은 그냥 subtle 노트로 무엇을 했는지만 렌더
   4. patch는 현행처럼 diff 렌더
   5. question, shell, todo 는 메세지박스로 렌더
5. 내용이 많은 메세지박스는 collapse 후 메세지 박스 하단에 Click to expand를 Secondary text로 놔두기 (hover시 배경색도 변경돼야 함)

전체적인 디자인을 보내준 이미지를 참고해

======
좋습니다. 보내주신 이미지처럼 **“프레임이 있는 채팅 UI”가 아니라, 넓은 여백 위에 텍스트와 배경 블록이 자연스럽게 섞이는 transcript UI**로 바꾸는 방향이 잘 맞습니다.

현재 Airicode 구조를 기준으로 보면, UI가 `src/ui/terminal.rs`에 상당 부분 집중되어 있고, `Message`에는 `Text / ToolCall / ToolResult / Reasoning`이 함께 들어가며, `Note`는 별도 컬렉션으로 관리되고 있습니다. 따라서 이번 변경은 **core의 대화 모델을 크게 흔들기보다, UI에 표시용 transcript layer를 하나 두고 Message + Note를 합쳐 렌더링하는 방식**이 가장 깔끔합니다. ([GitHub][1])

### 목표 렌더링 모델

| 원본 데이터                   | 화면 표시                             |
| ------------------------ | --------------------------------- |
| Assistant `Text`         | 일반 Text                           |
| Assistant `Reasoning`    | `Thought: ...` 형태의 secondary Text |
| User `Text`              | 배경 + padding이 있는 Box              |
| `NoteContent::Info`      | Box                               |
| `NoteContent::Alert`     | 경고 스타일 Box                        |
| 신규 `NoteContent::Subtle` | secondary color 한 줄 Text          |
| `NoteContent::Diff`      | 현재 diff renderer                  |
| `ToolCall`               | 숨김                                |
| `ToolResult`             | 숨김                                |

`ToolCall`/`ToolResult`를 데이터에서 제거하는 것은 권하지 않습니다. 모델에게 전달되는 conversation history와 UI transcript는 별개의 문제이므로, **tool protocol 데이터는 그대로 유지하고 화면에서만 숨기는 것**이 좋습니다. 지금 `MessagePart` 구조도 이 분리를 하기에 충분합니다. ([GitHub][2])

구현은 다음 순서로 진행하는 것을 권합니다.

1. **UI용 `TranscriptItem` 계층을 먼저 만듭니다.**
   `terminal.rs`에서 곧바로 `Message`를 `Paragraph`로 변환하는 방식 대신, 먼저 세션 상태를 화면용 데이터로 정규화합니다. 예를 들면 개념적으로 `AgentText`, `Reasoning`, `UserBox`, `InfoBox`, `AlertBox`, `SubtleNote`, `Diff` 정도면 충분합니다.

   핵심은 `SessionState.messages`와 `SessionState.notes`를 각각 `created_at` 기준으로 합쳐 하나의 chronological transcript를 만드는 것입니다. 현재 세션 모델이 messages와 notes를 별도 `BTreeMap`으로 가지고 있으므로 이 합성은 UI 계층에서 하는 편이 자연스럽습니다. ([GitHub][3])

   대략적인 책임 분리는 이렇게 두는 편이 좋습니다.

   ```text
   SessionState
       │
       ├── visible messages
       └── notes
             │
             ▼
      build_transcript()
             │
             ▼
      Vec<TranscriptItem>
             │
             ├── measure()
             └── render()
   ```

   `Role::Tool`, `MessagePart::ToolCall`, `MessagePart::ToolResult`는 `build_transcript()`에서 제외합니다. 기존 `show_tool_calls` 옵션을 유지할 필요가 있다면 debug용 별도 branch로만 남기고, 기본 UI에서는 완전히 빠지게 하는 것이 좋습니다. 현재 terminal 옵션에도 `show_reasoning`, `show_tool_calls`가 이미 존재합니다. ([GitHub][1])

   파일은 규모가 커질 것이므로 가능하면 `terminal.rs` 하나에 계속 넣기보다 다음 정도로 자르는 것을 권합니다.

   ```text
   src/ui/
     terminal.rs        # terminal lifecycle + event loop
     transcript.rs      # SessionState -> TranscriptItem
     render.rs          # Text / Box / Diff 렌더러
     state.rs           # scroll / hover / expanded state
     theme.rs           # 색, spacing, padding
   ```

   현재 작업 트리에 이미 `state.rs`나 fragment 계층이 있다면 그 구조를 활용하고, 굳이 이름을 위와 똑같이 맞출 필요는 없습니다.

2. **`NoteContent::Subtle`을 추가하고 tool plugin의 UI 출력을 Note로 이동합니다.**
   현재 `NoteContent`는 `Info`, `Alert`, `Diff` 세 종류이므로 여기에 아래 의미의 variant를 추가하면 됩니다. ([GitHub][4])

   ```rust
   NoteContent::Subtle {
       content: String,
   }
   ```

   각 tool은 기존 `ToolOutput`을 그대로 반환하면서 **추가 side effect로 Note를 생성**하도록 바꿉니다. 즉:

   ```text
   tool executes
        ├── ToolOutput → model / conversation
        └── Note       → human-facing transcript
   ```

   이 구분이 중요합니다. `ToolOutput` 자체를 Note로 대체하면 provider tool protocol과 context가 깨집니다.

   plugin별 정책은 다음처럼 고정하면 됩니다.

   | Plugin          | Note                                              |
   | --------------- | ------------------------------------------------- |
   | `grep`          | `Subtle("Searched \"foo\" in src/ · 12 matches")` |
   | `find`          | `Subtle("Found files matching ...")`              |
   | `read`          | `Subtle("Read src/core/session.rs:120-240")`      |
   | `webfetch`      | `Subtle("Fetched https://...")`                   |
   | 기타 조회성 tool     | `Subtle`                                          |
   | `patch`         | 기존 `Diff`                                         |
   | `shell`         | `Info`                                            |
   | `todo`          | `Info`                                            |
   | `question`      | `Info`                                            |
   | tool 실패 중 강조할 것 | `Alert`                                           |

   특히 `grep`은 현재 결과를 `ToolOutput`으로 만들지만 Note를 생성하지 않고, `read`, `webfetch`, `shell`, `todo`, `question` 쪽도 같은 작업이 필요합니다. 반면 `patch`는 이미 `add_note(NoteContent::Diff { ... })` 패턴을 사용하고 있어 이번 설계의 기준 구현으로 삼기 좋습니다. ([GitHub][5])

   여기서 shell의 경우 raw ToolResult를 UI에서 다시 꺼내 렌더하는 게 아니라 plugin 자체가 표시용 문자열을 만들게 하는 편이 좋습니다. 예를 들어 이미지처럼:

   ```text
   # Run grep integration tests in .

   $ nix develop -c cargo test --test workdir_tools_and_runtime

   warning: ...
   Compiling ...
   error[E0308]: ...
   ```

   TODO도 마찬가지로 tool의 JSON/raw 결과보다 plugin이 사람이 읽기 좋은 markdown/text를 만들어 `Info`로 보내도록 합니다.

3. **User Message에 “그 메시지를 보낼 당시 Mode”를 저장합니다.**
   User box가 항상 `Mode\n\nContents`여야 하므로 현재 선택된 mode를 렌더 시점에 읽으면 안 됩니다. 사용자가 나중에 Plan → Build로 바꾸면 이전 메시지의 색까지 바뀌기 때문입니다.

   `Message` 자체에 새 필드를 넣기보다 이미 있는 `metadata`를 활용하는 것이 최소 변경입니다. 현재 `Message`에는 `Metadata = BTreeMap<String, Value>`가 존재합니다. ([GitHub][2])

   사용자 메시지를 commit할 때:

   ```text
   metadata["mode"] = selected_mode
   ```

   를 저장하고, 렌더러에서는:

   ```text
   Plan

   이 기능의 구조를 먼저 설계해줘
   ```

   처럼 만듭니다.

   기존 세션처럼 `mode` metadata가 없는 메시지는 호환성을 위해 현재 `selected_mode`를 fallback으로 사용할 수 있지만, 이는 과거 mode를 정확히 복원하는 값은 아닙니다. 장기적으로 정확성이 중요하다면 legacy message에는 `"Unknown"` 또는 별도 fallback label을 쓰는 편이 더 정직합니다.

   Mode 색상 resolver는 UI theme에 두겠습니다.

   ```text
   Build → Blue
   Plan  → Yellow

   나머지:
   등록된 mode 이름을 case-insensitive alphabetic sort
       ↓
   미리 정의된 palette에 순서대로 할당
   ```

   예를 들어 `[Cyan, Magenta, Green, Red, LightCyan, ...]`처럼 Build/Plan이 사용하는 blue/yellow는 palette에서 제외합니다. 동일한 mode set에서는 실행할 때마다 색이 동일해집니다.

4. **메시지 영역을 “frame”이 아니라 margin이 있는 자유 레이아웃으로 교체합니다.**
   보내주신 이미지의 핵심은 border를 없애는 것보다도 **viewport → margin → item spacing → box padding**이 분리되어 있다는 점입니다.

   구조를 아래처럼 잡는 것이 좋습니다.

   ```text
   ┌──────────────── terminal ────────────────┐
   │                                          │
   │   ← message viewport margin →            │
   │                                          │
   │   Agent text                             │
   │   여러 줄 텍스트...                     │
   │                                          │
   │   ┌ conceptual only ─────────────────┐   │
   │   │  Plan                            │   │
   │   │                                  │   │
   │   │  User contents                   │   │
   │   └──────────────────────────────────┘   │
   │                                          │
   │   Read src/core/session.rs               │
   │                                          │
   ├──────────────────────────────────────────┤
   │ input area                               │
   └──────────────────────────────────────────┘
   ```

   실제 Box에는 border를 넣지 않습니다. `Block`을 쓰더라도 `Borders::NONE` 상태에서 **background style + padding 용도만** 사용합니다.

   개념적으로:

   ```rust
   Box {
       background: BOX_BG,
       padding_left: 2,
       padding_right: 2,
       padding_top: 1,
       padding_bottom: 1,
   }
   ```

   Agent message와 `Subtle`은 box가 아니므로 동일한 outer margin 안에 바로 Text로 그립니다.

   색/spacing 값은 전부 `theme.rs` 같은 한 곳에 모아 두는 것을 권합니다. 적어도 `primary`, `secondary`, `box_bg`, `box_hover_bg`, `alert_bg`, mode colors, diff colors, horizontal margin, box padding, item gap 정도는 token화해 두는 편이 이후 디자인 수정이 훨씬 쉽습니다.

5. **스크롤은 `Paragraph::scroll` 수준이 아니라 transcript layout 자체에서 처리합니다.**
   이번 디자인에서는 item마다 높이가 다르고, box가 collapse/expand되며, diff도 가변 높이이므로 전체 메시지를 하나의 거대한 `Paragraph`로 만든 뒤 scroll시키는 방식은 금방 한계에 부딪힙니다.

   먼저 각 item에 대해 현재 terminal width 기준으로 실제 높이를 계산합니다.

   ```text
   TranscriptItem
       ↓
   measure_item(width, expansion_state)
       ↓
   height
   ```

   그 다음 전체 visual height와 viewport height를 구해서 visible item만 render합니다.

   스크롤 state는 **bottom-relative**가 채팅 UI에 가장 편합니다.

   ```text
   scroll_offset = 0
       → 최신 메시지가 항상 화면 아래쪽

   PgUp
       → scroll_offset 증가

   PgDown
       → scroll_offset 감소
   ```

   이 방식이면 새 메시지가 들어왔을 때 동작도 자연스럽습니다.

   ```text
   사용자가 맨 아래에 있음
       → 새 메시지가 오면 계속 bottom follow

   사용자가 위로 스크롤한 상태
       → 새 메시지가 와도 현재 위치 유지
   ```

   현재 ephemeral UI state에도 `scroll: u16`이 있으므로 이를 재정의해 사용할 수 있지만, 이제 의미가 중요해지므로 `scroll_offset`처럼 이름까지 바꾸는 편을 권합니다. ([GitHub][6])

   입력은 다음처럼 처리합니다.

   ```text
   PgUp        → viewport 높이의 약 80~100% 위로
   PgDown      → viewport 높이의 약 80~100% 아래로
   Mouse wheel → 3~5 visual rows씩
   ```

   mouse wheel을 받으려면 terminal lifecycle에서 `EnableMouseCapture` / `DisableMouseCapture`를 추가하고, 종료/에러 시에도 반드시 disable되게 기존 terminal guard/drop 경로에 넣습니다.

   중요한 점은 **메시지 viewport 내부에서 발생한 wheel만 transcript scroll로 소비**하고, input 영역 이벤트와 섞지 않는 것입니다.

6. **collapse/hover/click을 위해 render 결과에 hit region을 남깁니다.**
   단순 ratatui 렌더링만 하던 구조에서 가장 큰 변화가 이 부분입니다.

   UI state에는 최소한 다음 상태가 필요합니다.

   ```text
   expanded: HashSet<TranscriptItemId>
   hovered: Option<TranscriptItemId>
   hit_regions: Vec<HitRegion>
   ```

   `TranscriptItemId`는 index를 사용하면 안 되고:

   ```text
   Message → MessageId
   Note    → NoteId
   ```

   처럼 stable ID로 만듭니다.

   draw 시 실제로 box가 그려진 영역을:

   ```text
   HitRegion {
       id,
       rect,
       action: Expand,
   }
   ```

   형태로 기록하고, 다음 mouse event에서 hit-test합니다.

   긴 **Box 계열만** collapse하면 됩니다. Agent Text나 Subtle Note까지 접을 필요는 없습니다.

   예를 들면:

   ```text
   실제 wrapped height > COLLAPSE_THRESHOLD
       ↓
   처음 12~16 visual rows만 렌더
       ↓
   빈 줄
   Click to expand
   ```

   정확한 12/16 값은 UI constant로 만들어 실제 터미널에서 조정하면 됩니다.

   footer의 `Click to expand`는 secondary text로 렌더하고, mouse가 해당 box/footer 위에 있으면:

   ```text
   BOX_BG
       ↓ hover
   BOX_BG_HOVER
   ```

   로 box 전체 background를 약간 밝게 합니다. 보내주신 이미지처럼 border를 추가하는 것보다 background 변화만 주는 쪽이 디자인에 더 잘 맞습니다.

   클릭 후 box의 높이가 증가할 때 화면이 갑자기 튀지 않도록 **클릭한 item의 top position을 anchor로 잡고 높이 delta만큼 scroll offset을 보정**하는 것도 같이 넣는 것이 좋습니다. 이 처리가 없으면 화면 중간의 shell output을 펼쳤을 때 사용자가 보고 있던 위치가 크게 움직입니다.

7. **`terminal.rs`의 event/render 흐름을 reducer 중심으로 정리합니다.**
   최종적으로는 대략 이 데이터 흐름이면 충분합니다.

   ```text
   Runtime / Session snapshot
             │
             ▼
      build_transcript()
             │
             ▼
       TranscriptItem[]
             │
      ┌──────┴────────┐
      ▼               ▼
   measure          UI State
      │         scroll / hover /
      │             expanded
      └──────┬────────┘
             ▼
           render
             │
             ▼
        hit_regions
   ```

   그리고 입력은:

   ```text
   Key PgUp/PgDown ──┐
   Mouse Wheel ──────┼→ UI Action → reducer
   Mouse Move ───────┤
   Mouse Click ──────┘
   ```

   로 보내는 편이 좋습니다.

   특히 `RuntimeEvent::ToolExecutionStarted/Finished`를 직접 화면용 line으로 만드는 코드가 있다면 제거하고, **Note가 세션 snapshot에 반영된 결과만 transcript가 렌더하도록** 바꾸겠습니다. Runtime event 모델에는 tool execution 이벤트가 이미 따로 있으므로, 이것을 그대로 UI에 표시하면 Note와 중복 표시될 가능성이 있습니다. ([GitHub][7])

8. **마지막으로 테스트를 visual-layout 중심으로 다시 잡습니다.**
   이번 변경은 문자열 unit test만으로는 회귀를 잡기 어렵습니다. `ratatui::backend::TestBackend` 기반 renderer test를 중심으로 두는 게 좋습니다.

   우선 반드시 검증할 것은 `Build = blue`, `Plan = yellow`, 기타 mode alphabetic assignment의 결정성, user message의 `Mode\n\nContents`, ToolCall/ToolResult 비표시, Message/Note의 시간순 병합, `Subtle`의 한 줄 렌더, Info의 padding/background, 좁은 화면에서 wrapping된 높이 계산입니다.

   여기에 PgUp/PgDown clamp, mouse wheel, bottom-follow 여부, scroll 중 새 메시지가 들어와도 viewport가 유지되는지, 긴 box collapse, hover hit-test, click expand 후 viewport anchor 유지까지 테스트합니다.

   그리고 이미지와 가장 가까운 회귀 테스트로는 80열/120열 정도의 TestBackend snapshot을 각각 두고 다음 형태의 transcript 하나를 통째로 렌더해 비교하는 것이 좋습니다.

   ```text
   agent text
   → user Plan box
   → subtle grep
   → patch diff
   → subtle read
   → shell box (collapsed)
   → todo box
   → long agent text
   ```

   한글, 긴 path, 긴 shell output도 fixture에 반드시 포함시키는 편이 좋습니다.

구현 순서는 **① `NoteContent::Subtle` + user mode metadata → ② 각 tool의 Note 생성 → ③ `TranscriptItem`/theme/measurement → ④ frame 없는 Text/Box 렌더링 → ⑤ PgUp/PgDown + mouse scroll → ⑥ collapse/hover/click → ⑦ snapshot/regression tests** 순서가 가장 안전합니다. 중간 단계마다 컴파일 가능한 상태를 유지할 수 있고, 특히 plugin 변경과 UI 변경을 서로 독립적으로 검증할 수 있습니다.

이번 리디자인에서 가장 중요한 설계 결정은 **“Message/Note는 domain model, TranscriptItem은 presentation model”로 확실히 분리하는 것**이라고 봅니다. 그렇게 하면 앞으로 `sidequery`, `subagent`, compaction 같은 plugin이 별도 UI 표현을 추가하더라도 `MessagePart`를 계속 늘리지 않고 Note → Transcript 변환만 확장할 수 있습니다. 현재 `Note`가 이미 세션 mutation/replay의 일부이기 때문에 이 방향은 기존 persistence 구조와도 잘 맞습니다. ([GitHub][3])

그리고 보내주신 이미지 기준으로는 **outer frame 제거, item 간 넉넉한 세로 간격, 아주 약한 box background, muted secondary text, diff만 강한 색상**을 유지하는 것이 전체 인상을 좌우할 것 같습니다. 기능적으로는 상당한 변경이지만, core 변경은 `Subtle`과 mode metadata 정도로 억제하고 대부분을 `src/ui/terminal` 계층과 `tool_*` plugin에 한정할 수 있습니다. [Airicode repository](https://github.com/HelloWorld017/airicode?utm_source=chatgpt.com)

[1]: https://github.com/HelloWorld017/airicode/blob/master/src/ui/terminal.rs "airicode/src/ui/terminal.rs at master · HelloWorld017/airicode · GitHub"
[2]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/message.rs "airicode/src/core/models/message.rs at master · HelloWorld017/airicode · GitHub"
[3]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/session.rs "airicode/src/core/models/session.rs at master · HelloWorld017/airicode · GitHub"
[4]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/note.rs "airicode/src/core/models/note.rs at master · HelloWorld017/airicode · GitHub"
[5]: https://github.com/HelloWorld017/airicode/blob/master/src/plugins/tool_grep.rs "airicode/src/plugins/tool_grep.rs at master · HelloWorld017/airicode · GitHub"
[6]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/ui_state.rs "airicode/src/core/models/ui_state.rs at master · HelloWorld017/airicode · GitHub"
[7]: https://github.com/HelloWorld017/airicode/blob/master/src/core/models/events.rs "airicode/src/core/models/events.rs at master · HelloWorld017/airicode · GitHub"

