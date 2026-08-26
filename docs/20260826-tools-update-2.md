* find를 find_file로 바꾸고 one_of로 by_filename_keyword, by_filename_exact, by_glob_pattern 으로
* patch
  * snapshot에서 모든 anchor를 먼저 resolve
  * atomic으로 할 경우 문제가 같은 패치를 over-and-over로 작성할 우려가 있어
  * 결과를 돌려줄 때 변경된 라인을 같이 보여주게

수정하고 싶어

----
새로운 Patch 결과 형식:

Partial Failure: Applied 5 of 6 operations.
# Failure / Partial Failure / Success

[1] APPLIED "REPLACE src/a.rs FROM 20:AxY TO 24:asG"
Updated file:
19:v93|...
20:Ab1|...
21:Bc2|...
22:Axc|...
23:xG3|...
24:9b1|...
25:1kh|...

# 수정된 줄 전후 1줄씩 보여줌
# 만약 Replace가 6줄을 넘어선다면 3줄 / ... n line(s) omitted ... / 3줄 처럼 보여줌

[2] APPLIED "INSERT src/b.rs AFTER 17:Zxc"
Updated file:
17:xc3|...
18:Ke8|...
19:Cf0|...

# 삽입된 줄 전후 1줄까지 보여줌
# 마찬가지로 6줄을 넘어선다면 3줄 / ... n line(s) omitted ... / 3줄 처럼 보여줌

[3] FAILED "REPLACE src/c.rs FROM 42:Xyz TO 46:Gxs"
Reason: Anchor is stale (42:Xyz).
Current file:
41:bg3|...
42:Xk8|...
43:g13|...
... 1 line(s) omitted ...
45:hqb|...
46:HSR|...
47:0zB|...

# Start anchor, end anchor 전부 전후 1줄씩 보여줌

[4] APPLIED "REPLACE src/d.rs FROM 7:xz3 TO 10:Cd9"
Updated file:
6:9lP|...
7:b19|...
8:Hh4|...
9:9hK|...

[5] APPLIED "ADD src/e.rs"
[6] APPLIED "DELETE src/f.rs"

# Add, Delete 는 결과를 보여주지 않음


정리하자면
```
Patch result

Header:
- Success: Applied N operations.
- Partial Failure: Applied X of N operations.
- Failure: Applied 0 of N operations.

APPLIED REPLACE / INSERT:
- Show the updated region from the final file.
- Include one unchanged context line before and after.
- If the changed region exceeds 6 lines, show its first 3 and last 3 lines,
  separated by "... N line(s) omitted ...".
- Returned hashlines MUST be freshly calculated from the final file.

FAILED anchored operation:
- State every stale/invalid anchor in Reason.
- Show ±1 line around each relevant anchor from the original snapshot.
- Merge overlapping/adjacent context windows.
- Separate non-adjacent windows with "... N line(s) omitted ...".

APPLIED ADD / DELETE:
- Do not show file contents.
```

====

* Goals
    * 기존 find를 find_file로 교체한다.
    * find_file의 검색 방법을 mutually exclusive한 세 종류의 query로 정의한다.
    * patch의 모든 anchor를 patch 시작 시점의 filesystem snapshot에서 먼저 resolve한다.
    * patch 전체는 atomic하게 만들지 않고, 적용 가능한 operation은 적용할 수 있도록 한다.
    * patch 결과에 operation별 성공/실패 상태를 명확하게 반환한다.
    * 성공한 REPLACE / INSERT 결과에는 final file 기준 fresh hashline을 반환한다.
    * 실패한 anchored operation에는 original snapshot 기준 현재 anchor 주변 context를 반환한다.
    * patch syntax/parser error가 발생하면 예상 형식과 실패 원인을 구체적으로 알려준다.


### find_file
#### Rename

기존:

find

를 다음으로 변경한다.

find_file

내부 구현 이름도 가능한 범위에서 함께 변경한다.

예:

ToolFind
→ ToolFindFile

ToolFindPlugin
→ ToolFindFilePlugin

tool_find.rs
→ tool_find_file.rs

외부 tool name과 내부 구현 명칭이 불필요하게 어긋나지 않도록 한다.

#### find_file Input Schema

검색 방법은 oneOf semantics를 사용한다.

동시에 여러 검색 방법을 지정할 수 없어야 한다.

개념적으로 다음 세 variant를 제공한다.

by_filename_keyword
by_filename_exact
by_glob_pattern

예상 구조:

enum FindFileQuery {
    ByFilenameKeyword {
        keyword: String,
    },

    ByFilenameExact {
        filename: String,
    },

    ByGlobPattern {
        pattern: String,
    },
}

공통 option:

path?: string
max_results?: integer
#### find_file.by_filename_keyword

파일의 basename에 특정 keyword가 포함된 파일을 검색한다.

예:

{
  "query": {
    "kind": "by_filename_keyword",
    "keyword": "message"
  },
  "path": "src",
  "max_results": 50
}
Semantics
directory path 전체가 아니라 filename을 기준으로 검색한다.
literal substring 검색으로 동작한다.
기본적으로 case-insensitive 검색을 사용한다.
directory는 반환 대상에서 제외하고 파일만 반환한다.
path가 지정되면 해당 directory subtree로 검색 범위를 제한한다.

예:

keyword = "message"

가능한 결과:

src/core/message.rs
src/core/models/message.rs
tests/message_roundtrip.rs

#### find_file.by_filename_exact

basename이 정확히 일치하는 파일을 검색한다.

예:

{
  "query": {
    "kind": "by_filename_exact",
    "filename": "AGENTS.md"
  },
  "max_results": 20
}
Semantics

다음과 같은 검색 용도에 사용한다.

AGENTS.md
Cargo.toml
package.json
README.md

filename만 비교하며 directory path는 비교 대상이 아니다.

#### find_file.by_glob_pattern

filesystem relative path에 glob pattern을 적용한다.

예:

{
  "query": {
    "kind": "by_glob_pattern",
    "pattern": "src/ui/**/*.rs"
  },
  "max_results": 50
}

지원해야 하는 대표적인 형태:

src/ui/**/*.rs
**/AGENTS.md
tests/**/*.rs
src/plugins/tool_*.rs
Semantics

path가 없으면 workdir을 glob root로 사용한다.

path가 존재하면 해당 path를 root로 사용한다.

예:

{
  "query": {
    "kind": "by_glob_pattern",
    "pattern": "**/*.rs"
  },
  "path": "src/ui"
}
10. find_file Result

검색 결과가 없을 때 빈 문자열을 반환하지 않는다.

기존과 같은:

Success:
""

형태는 사용하지 않는다.

대신 명시적으로:

No files matched.

를 반환한다.

결과가 max_results에 의해 잘렸다면 다음 정보를 포함한다.

Showing 50 of 127 matching files.

정확한 total count 계산 비용이 지나치게 크다면 최소한:

Showing the first 50 matching files. More matches exist.

형태로 truncation 여부는 반드시 알려준다.


### Patch Execution Model
#### Snapshot Semantics

patch tool call이 시작되면 해당 patch가 참조하는 기존 파일들의 snapshot을 먼저 확보한다.

모든 hashline anchor는 이 snapshot을 기준으로 resolve한다.

핵심 규칙:

All anchors in one patch refer to the filesystem state
at the beginning of that patch call.

기존의 다음 semantics는 제거한다.

Later anchors see changes made by earlier operations.

#### Patch Processing Pipeline

patch는 다음 순서로 처리한다.

Parse patch
    ↓
Collect required filesystem snapshots
    ↓
Resolve every anchor against those snapshots
    ↓
Validate operations
    ↓
Detect conflicts
    ↓
Apply all applicable operations
    ↓
Write resulting files
    ↓
Calculate fresh hashlines from final files
    ↓
Render operation-by-operation result

중요한 점은 anchor resolution 과정에서는 filesystem을 변경하지 않는다는 것이다.

#### Patch Atomicity

patch 전체는 atomic transaction으로 만들지 않는다.

예를 들어 6개의 operation 중 5개가 유효하고 1개가 stale이면:

5개 적용
1개 실패

가 되어야 한다.

다음처럼 동작하지 않는다.

5개 성공 가능
1개 실패
→ 전체 rollback
→ 6개 operation 모두 다시 요청

이유는 일부 anchor 실패 때문에 동일한 대규모 patch를 모델이 반복 생성하는 상황을 피하기 위해서다.

따라서 patch는:

snapshot-consistent
but not globally atomic

이어야 한다.
