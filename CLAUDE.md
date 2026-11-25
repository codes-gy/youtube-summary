# ytdoc

영상 하나를 요약 문서(PDF, 원하면 Obsidian 노트)로 만드는 Rust CLI이자 로컬 웹 서버. 사용법과 준비물은 README.md에 있다.

## 용어와 결정

- `GLOSSARY.md`: 사용자에게 보이는 글(README, 웹 화면, 오류·진행 메시지)을 쓰거나 타입·함수 이름을 지을 때 읽는다. 받아쓰기, 장면, 챕터, 원어, 문서 언어 같은 용어를 여기에 맞춘다.
- `docs/adr/`: 받아쓰기 엔진, PDF 조판, Claude 호출 경로, 단계 캐시, 웹 서버의 노출 범위를 바꾸려 할 때 먼저 읽는다.

## 빌드와 테스트

- 첫 빌드는 `whisper-rs`가 whisper.cpp를 CMake로 컴파일한다. CMake, C/C++ 컴파일러, libclang(LLVM, bindgen용)이 필요하다. CMake나 bindgen 오류가 나면 대개 이 준비물 문제다.
- whisper.cpp의 CMake 캐시는 컴파일러 절대 경로를 기억한다. 툴체인을 바꾼 뒤 빌드가 깨지면 `target/`에서 whisper 빌드 결과를 지운다.
- 다른 컴퓨터로 옮길 바이너리는 `GGML_NATIVE=OFF`로 빌드한다. 켜 두면 CPU가 다른 컴퓨터에서 SIGILL로 죽는다.
- 실행하려면 `ffmpeg`/`ffprobe`가 PATH에 있어야 한다. YouTube 입력에는 `yt-dlp`와 `deno`도 필요하다.
- 기본 `cargo test`는 외부 도구 없이 돈다. `#[ignore]` 테스트는 ffmpeg와 네트워크(Hugging Face의 `tiny` 모델)를 쓰고, CI는 `--include-ignored`로 모두 돌린다.
- Claude를 부르는 코드는 `claude::tests::Scripted`(정해 둔 답을 돌려주는 `Llm`)로 테스트한다. 테스트에서 실제 API나 `claude`를 부르지 않는다.

## 환경 변수

- `ANTHROPIC_API_KEY`: 있으면 `--summarizer auto`가 API를, 없으면 `claude -p`를 쓴다.
- `YTDOC_CLAUDE_TIMEOUT_SECS`: `claude -p` 한 번의 시간 제한(기본 900초).
- `YTDOC_MAX_UPLOAD_MB`, `YTDOC_OBSIDIAN_VAULT`: 같은 이름의 CLI 옵션 대신 쓴다.

## 지킬 것

- 단계 결과의 형식이나 뜻을 바꾸면 `src/pipeline.rs`에서 그 단계의 캐시 파일 이름 버전(`summary.v2`, `transcript-fixes.v1`)을 올린다. 버전이 없는 파일이면 새로 붙인다. 이름이 같으면 옛 캐시가 조용히 다시 쓰인다(ADR 0004).
- Claude가 돌려주는 타입(`Summary`, `TranscriptFixes`, `TranslatedBatch`)은 그대로 JSON 스키마가 되어 Claude에 간다. 필드의 doc comment가 곧 프롬프트이다. 스키마로 표현하지 못하는 규칙은 `Summary::check` 같은 검사 함수에 넣는다. 검사에 걸리면 `ask`가 이유를 붙여 한 번 더 묻는다.
- 영상과 요약의 글은 JSON 데이터로만 Typst에 넘긴다. 템플릿 문자열에 이어 붙이지 않는다.
- 웹 서버의 `local_host`/`same_origin` 검사와 `127.0.0.1` 바인딩은 보안 경계이다(ADR 0005).
- 사용자에게 보이는 메시지는 한국어로, 코드 주석과 Claude 프롬프트는 영어로 쓴다.

## 커밋

접두사(`feat:`, `fix:`) 없이 영어 명령형 제목 한 줄을 쓴다(예: `Retry a YouTube download that is refused for a moment`). 본문에는 무엇을 왜 바꿨고 어떻게 확인했는지 적는다.
