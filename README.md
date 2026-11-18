# youtube-summary

YouTube 영상 주소나 내 컴퓨터의 영상 파일을 넣으면 요약 문서를 PDF로 만들어 주는 프로그램입니다. 프로그램 이름은 `ytdoc`이고 Rust로 만들었습니다. 브라우저 화면(`ytdoc serve`)으로 쓸 수도 있고, 명령줄로 쓸 수도 있습니다. 원하는 언어로 번역한 문서도 만들 수 있습니다.

PDF에 들어가는 내용은 다음과 같습니다.

- 표지: 제목, 채널, 영상 길이, 주소, 한 줄 요약
- 전체 요약, 핵심 포인트, 목차
- 챕터별 페이지: 시간 구간(YouTube 영상이면 누를 때 해당 시점으로 이동), 핵심 장면 이미지, 요약, 핵심 포인트
- 부록: 시간이 표시된 전체 받아쓰기 (번역을 켜면 번역문만 들어갑니다)

예시 PDF는 [`examples/`](examples/) 폴더에 있습니다.

## 동작 방식

1. **영상 준비.** [yt-dlp](https://github.com/yt-dlp/yt-dlp)로 720p 이하 영상을 받습니다. 내 컴퓨터의 파일이면 내려받지 않고 그 파일을 씁니다.
2. **소리 꺼내기.** ffmpeg로 영상에서 16kHz 모노 음성을 꺼냅니다.
3. **받아쓰기.** [whisper.cpp](https://github.com/ggml-org/whisper.cpp)(OpenAI Whisper 모델)로 음성을 글자로 바꿉니다. 프로그램 안에서 직접 실행되고, Apple Silicon Mac에서는 GPU(Metal)를 씁니다. 내 컴퓨터에서 실행되므로 비용이 들지 않습니다. 언어는 자동으로 알아냅니다. YouTube 자막은 쓰지 않습니다.
4. **장면 뽑기.** ffmpeg로 화면이 크게 바뀌는 순간을 찾고, 화면 변화가 적은 영상은 30초마다 한 장씩 보충합니다. 거의 같은 장면은 perceptual hash로 걸러 냅니다.
5. **번역 (번역을 켰을 때만).** Claude가 받아쓰기를 나누어 번역합니다. 각 줄의 시간은 원래 받아쓰기의 시간을 그대로 씁니다. 고른 언어가 영상에서 말하는 언어와 같으면 이 단계를 건너뜁니다.
6. **요약.** Claude가 받아쓰기를 읽고, 문서 언어로 챕터, 요약, 핵심 포인트를 씁니다.
7. **PDF 만들기.** HTML 문서를 만든 뒤 Google Chrome의 headless 인쇄 기능으로 A4 PDF를 만듭니다.

각 단계의 결과는 `.ytdoc_cache/<영상ID>/`에 저장됩니다. 내 컴퓨터의 파일은 파일 내용으로 만든 이름(`file-<해시>`)의 폴더를 씁니다. 같은 영상으로 다시 실행하면 끝난 단계는 건너뜁니다. 실행이 중간에 멈췄다면 그대로 다시 실행하면 이어서 진행합니다.

## 준비물

macOS, Linux, Windows에서 쓸 수 있습니다. 필요한 프로그램은 다음과 같습니다.

- 빌드할 때만 필요: [Rust](https://rustup.rs/), [CMake](https://cmake.org/), C/C++ 컴파일러, LLVM(clang). whisper.cpp를 함께 빌드하기 때문입니다.
- [yt-dlp](https://github.com/yt-dlp/yt-dlp): YouTube 영상을 받습니다. 내 컴퓨터의 파일만 쓴다면 필요 없습니다.
- [deno](https://deno.com/): yt-dlp가 YouTube 영상을 받을 때 필요한 JavaScript 실행기입니다. 없으면 `HTTP Error 403`으로 내려받기가 실패합니다.
- [ffmpeg](https://ffmpeg.org/)
- Google Chrome, Chromium, Microsoft Edge 중 하나
- 요약에 쓸 Claude: 아래 [요약 방식 고르기](#요약-방식-고르기) 참고

### macOS

```sh
xcode-select --install
brew install rustup cmake yt-dlp deno ffmpeg
rustup-init
```

### Windows

[Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)를 설치하고 **C++를 사용한 데스크톱 개발**을 고릅니다. 그다음 PowerShell에서 실행합니다.

```powershell
winget install Rustlang.Rustup Kitware.CMake LLVM.LLVM yt-dlp.yt-dlp DenoLand.Deno Gyan.FFmpeg
```

설치한 뒤 PowerShell을 새로 열어야 명령을 찾을 수 있습니다. Windows에는 Microsoft Edge가 기본으로 있어서 Chrome이 없어도 됩니다.

### Linux (Ubuntu, Debian)

```sh
sudo apt install build-essential cmake clang ffmpeg
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

yt-dlp는 배포판 패키지가 오래된 경우가 많으니 [yt-dlp 설치 안내](https://github.com/yt-dlp/yt-dlp/wiki/Installation)를 따르고, [deno](https://docs.deno.com/runtime/getting_started/installation/)와 Chrome(또는 Chromium)도 설치합니다.

## 설치

저장소를 내려받은 폴더에서 실행합니다.

```sh
cargo install --path .
```

`ytdoc` 명령이 `~/.cargo/bin/`(Windows는 `%USERPROFILE%\.cargo\bin\`)에 설치됩니다. 처음 빌드는 whisper.cpp까지 컴파일하므로 몇 분 걸립니다.

Whisper 모델 파일은 처음 받아쓰기를 할 때 홈 폴더의 `.cache/ytdoc/models/`에 내려받습니다. 기본 모델 `large-v3-turbo`는 약 1.6GB입니다. 받아쓰기는 Apple Silicon Mac에서는 GPU(Metal)를, 다른 컴퓨터에서는 CPU를 씁니다. 그래서 Mac이 아닌 컴퓨터에서는 더 오래 걸릴 수 있습니다. 느리면 `--whisper-model small`을 써 보세요.

## 웹 화면으로 쓰기

```sh
ytdoc serve
```

브라우저에서 `http://127.0.0.1:8000/`을 엽니다. 화면은 내 컴퓨터에서만 열립니다.

1. **유튜브 주소** 탭에 영상 주소를 붙여 넣거나, **파일 업로드** 탭에 영상 파일을 끌어다 놓습니다. 영상 파일과 mp3, m4a, wav 같은 음성 파일을 쓸 수 있습니다.
2. 번역이 필요하면 **번역하기**를 켜고 언어를 고릅니다.
3. **PDF 만들기**를 누릅니다. 단계마다 진행 상태와 걸린 시간이 보입니다.
4. 끝나면 **PDF 내려받기**를 누릅니다.

작업은 한 번에 하나씩 처리합니다. 진행 중에 다른 작업을 넣으면 앞 작업이 끝난 뒤 시작합니다. 서버를 끄려면 터미널에서 `Ctrl+C`를 누릅니다.

포트를 바꾸려면 `--port`를 씁니다(기본값 `8000`). `--whisper-model`, `--max-images`, `--summarizer`, `--cache-dir`도 아래 명령줄과 같은 뜻으로 쓸 수 있습니다.

## 명령줄로 쓰기

```sh
ytdoc "https://www.youtube.com/watch?v=영상ID"
```

현재 폴더에 `<영상ID>.pdf`가 생깁니다. 내 컴퓨터의 파일도 넣을 수 있습니다. 이때는 `<파일 이름>.pdf`가 생깁니다.

```sh
ytdoc ./강의.mp4
```

자주 쓰는 옵션은 다음과 같습니다.

```sh
# 저장 위치 지정
ytdoc "https://youtu.be/영상ID" -o 요약.pdf

# 영어로 번역한 문서 만들기
ytdoc "https://youtu.be/영상ID" --translate en

# 더 빠른 받아쓰기 (정확도는 조금 낮음)
ytdoc "https://youtu.be/영상ID" --whisper-model small

# 챕터마다 이미지를 최대 4장까지
ytdoc "https://youtu.be/영상ID" --max-images 4
```

| 옵션 | 기본값 | 설명 |
| --- | --- | --- |
| `-o`, `--output` | `<영상ID>.pdf` 또는 `<파일 이름>.pdf` | PDF 저장 경로 |
| `--translate` | 없음 | 문서를 이 언어로 번역합니다: `ko`(한국어), `en`(English), `ja`(日本語), `zh`(中文), `es`(Español), `fr`(Français), `de`(Deutsch), `vi`(Tiếng Việt). 생략하면 영상에서 말하는 언어 그대로 만듭니다. |
| `--whisper-model` | `large-v3-turbo` | `tiny`, `base`, `small`, `medium`, `large-v3`, `large-v3-turbo` 또는 ggml 모델 파일 경로. 클수록 정확하지만 느립니다. |
| `--max-images` | `2` | 챕터당 최대 이미지 수 |
| `--summarizer` | `auto` | `auto`, `api`, `claude-cli` |
| `--cache-dir` | `.ytdoc_cache` | 중간 결과 저장 폴더 |

## 요약 방식 고르기

| 방식 | 필요한 것 | 비용 |
| --- | --- | --- |
| `claude-cli` | [Claude Code](https://claude.com/claude-code)가 설치되어 있고 로그인되어 있어야 합니다. | 사용 중인 Claude 구독 사용량에서 차감됩니다. |
| `api` | `ANTHROPIC_API_KEY` 환경 변수 | Anthropic API 사용량만큼 요금이 나옵니다. |

`auto`는 `ANTHROPIC_API_KEY`가 있으면 `api`를, 없으면 `claude-cli`를 씁니다. 번역도 요약과 같은 방식으로 Claude를 씁니다.

`api` 방식은 `claude-opus-5-5` 모델을 씁니다. 정확한 금액은 [Claude Console](https://console.anthropic.com/)의 사용량에서 확인하세요. 같은 영상을 다시 실행하면 저장된 요약을 쓰므로 추가 요금이 없습니다.

## 시간이 얼마나 걸리나요

Apple M1에서 6분 10초짜리 한국어 영상([예시](examples/))을 처리할 때 측정한 값입니다. 여러 번 측정한 값은 범위로 적었습니다.

| 단계 | 시간 |
| --- | --- |
| 영상 준비 (13MB 내려받기) | 7초. 인터넷 속도에 따라 다릅니다. |
| 소리 꺼내기 | 1초 안쪽 |
| 받아쓰기 (`large-v3-turbo`) | 87초~124초 |
| 장면 뽑기 | 8초 |
| 번역 (`claude-cli`, 영어로) | 48초 |
| 요약 (`claude-cli`) | 17초~30초 |
| PDF 만들기 | 2초~8초 |

처음 실행할 때는 Whisper 모델(약 1.6GB)을 내려받는 시간이 더 걸립니다. 같은 영상을 다시 실행하면 끝난 단계는 건너뛰므로 PDF 만들기만 다시 합니다.

## 알려진 한계

- 말이 없는 영상(음악, 무성 영상)은 요약할 내용이 없습니다.
- 내 컴퓨터의 파일로 만든 PDF에는 YouTube 링크가 없습니다. 시간은 그대로 표시됩니다.
- 번역은 Claude를 한 번 더 쓰므로 `api` 방식에서는 요금이 더 나옵니다.
- 웹 화면의 작업 목록은 서버를 끄면 사라집니다. 만든 PDF와 중간 결과는 `.ytdoc_cache/`에 남습니다.
- 받아쓰기는 Whisper 모델의 정확도를 따릅니다. 고유 명사는 틀릴 수 있습니다.
- 로그인이 필요한 영상, 비공개 영상은 받을 수 없습니다.
- 영상 내려받기는 YouTube 이용 약관과 저작권을 지키는 범위에서만 하세요. 만든 PDF를 다른 사람에게 나눠 줄 때는 원본 영상의 라이선스를 확인하세요.

## 개발

```sh
cargo test
# ffmpeg, Whisper, Chrome을 실제로 돌리는 테스트까지 (tiny 모델을 내려받습니다)
cargo test -- --include-ignored
cargo run -- "https://youtu.be/영상ID"
```

## 라이선스

MIT
