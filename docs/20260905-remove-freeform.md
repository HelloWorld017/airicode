1. patch_hashline
  * lines 대신 content: String으로 받아.
  * operations: Vec<PatchHashlineOperation> 으로 받지 말고 그냥 operation 1개를 받아.
2. apply_patch같은 경우 다음과 같이 조금 스키마를 바꿔. (예시코드는 ts야. schemars에 맞게 바꿔)
    ```ts
    const applyPatchSchema = Type.Object({
      input: Type.String({
        description: "Patch content using the *** Begin Patch/End Patch format.",
      }),
    }); 
    ```
3. Freeform Tool의 지원을 제거해.
    * 관련 코드를 전부 삭제해.
    * 어차피 Model의 내부 Tool Call Convention에서 대부분의 경우에는 argument의 JSON escape 없이 호출할 수 있어.
