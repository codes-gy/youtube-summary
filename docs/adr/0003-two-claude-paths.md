# Claude는 Anthropic API와 `claude -p` 두 길로 부른다

받아쓰기 다듬기, 번역, 요약은 Claude를 부른다. API 키로 사용량만큼 내는 사용자와 Claude Code 구독을 이미 쓰는 사용자가 모두 추가 준비 없이 쓰도록, `Llm` 트레이트 뒤에 두 경로를 둔다(`src/claude.rs`). `--summarizer auto`는 `ANTHROPIC_API_KEY`가 있으면 API를, 없으면 `claude` 명령을 쓴다. 두 경로 모두 같은 JSON 스키마의 구조화 출력을 요구하고, 답은 같은 검사(`ask`)를 거친다.

## Consequences

- 경로마다 따로 다룰 실패가 있다. API는 429·529·5xx를 백오프로 다시 시도하고, `claude -p`는 멈추면 시간 제한(기본 15분, `YTDOC_CLAUDE_TIMEOUT_SECS`)으로 프로세스 트리를 끝낸다(6a9fa0f).
- Windows에서 `claude`는 `claude.cmd`일 수 있고 Rust는 배치 파일에 여러 줄 인자를 넘기지 않는다. 그래서 시스템 프롬프트를 파일로 넘긴다(826305a).
- `claude -p`는 도구, 세션 저장, 슬래시 명령, MCP 설정을 끄고 부른다. 요약에 사용자의 Claude Code 환경이 섞이지 않게 하려는 것이다.
