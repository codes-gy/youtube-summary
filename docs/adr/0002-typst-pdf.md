# PDF는 프로그램 안의 Typst로 조판한다

처음에는 HTML 템플릿을 headless Chrome으로 인쇄했지만, 사용자가 Chrome·Chromium·Edge를 따로 갖춰야 했다. 그래서 Typst 라이브러리를 프로그램에 넣고 `templates/document.typ`로 A4 PDF를 만든다(PR #5, 4d46bd5). 브라우저 없이 바이너리 하나로 돌아가고, 3000줄 받아쓰기도 1초 안에 조판된다.

## Consequences

- 글꼴은 시스템 글꼴과 Typst 내장 글꼴에서 찾는다. Linux에서 한중일 글꼴이 없으면 글자가 빠지므로 README가 `fonts-noto-cjk` 설치를 안내한다.
- 영상과 요약의 글은 JSON 데이터(`/data.json`)로만 템플릿에 넘어가서 Typst 코드로 실행되지 않는다. Typst는 템플릿, 데이터, 작업 폴더 안의 파일만 읽는다. 둘 다 테스트가 지킨다.
- 레이아웃을 바꾸려면 HTML/CSS가 아니라 Typst 템플릿을 고친다. 한국어 낱말이 줄바꿈에서 쪼개지지 않게 하는 처리(CSS `keep-all` 대체)도 템플릿에 있다.
