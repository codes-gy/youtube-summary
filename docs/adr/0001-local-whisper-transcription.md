# 받아쓰기는 프로그램 안의 whisper.cpp로 내 컴퓨터에서 한다

받아쓰기는 음성 인식 API나 YouTube 자막을 쓰지 않고, `whisper-rs`로 whisper.cpp를 프로그램에 함께 빌드해 내 컴퓨터에서 돌린다. 영상 길이에 따라 늘어나는 비용이 없고, YouTube 영상과 내 컴퓨터의 파일이 같은 길로 받아쓰기되며, 원어도 같은 방식으로 알아낸다.

## Consequences

- 소스 빌드에 CMake, C/C++ 컴파일러, libclang(bindgen)이 필요하고 첫 빌드가 몇 분 걸린다. 다른 CPU로 옮길 빌드(CI, 릴리스)는 `GGML_NATIVE=OFF`로 만든다. 켜 두면 다른 러너에서 SIGILL이 났다(826305a).
- 첫 실행 때 Hugging Face에서 모델(기본 `large-v3-turbo`, 약 1.6GB)을 받는다. Mac이 아닌 컴퓨터는 CPU로 돌아 느리다.
- 받아쓰기 정확도가 Whisper 모델에 묶인다. 잘못 들은 낱말은 뒤에 받아쓰기 다듬기 단계(df84e67)가 Claude로 고친다.
