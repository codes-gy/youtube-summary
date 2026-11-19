mod cache;
mod claude;
mod domain;
mod frames;
mod lang;
mod media;
mod obsidian;
mod pipeline;
mod platform;
mod render;
mod source;
mod summarize;
mod transcribe;
mod translate;
mod web;

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};

use crate::claude::BackendChoice;
use crate::lang::LangCode;
use crate::pipeline::{Event, Options, Outcome, Progress, Stage, run_pipeline};
use crate::source::Source;

#[derive(Parser)]
#[command(
    name = "ytdoc",
    version,
    about = "YouTube 영상이나 영상 파일을 받아쓰기, 핵심 장면, 요약이 담긴 PDF 문서로 만듭니다.",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// YouTube 영상 주소 또는 영상 파일 경로
    #[arg(required = true)]
    source: Option<String>,
    /// PDF 저장 경로 (기본값: <영상ID>.pdf 또는 <파일이름>.pdf)
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[command(flatten)]
    common: Common,
}

#[derive(Subcommand)]
enum Command {
    /// 브라우저에서 쓰는 로컬 웹 페이지를 엽니다 (127.0.0.1 전용)
    Serve {
        #[arg(long, default_value_t = 8000)]
        port: u16,
        #[command(flatten)]
        common: Common,
    },
}

#[derive(Args, Clone)]
struct Common {
    /// 문서와 받아쓰기를 이 언어로 번역 (예: en, ja). 없으면 영상에서 말한 언어 그대로
    #[arg(long, value_name = "LANG")]
    translate: Option<LangCode>,
    /// Whisper 모델 이름(tiny, base, small, medium, large-v3, large-v3-turbo) 또는 ggml 모델 파일 경로
    #[arg(long, default_value = "large-v3-turbo")]
    whisper_model: String,
    /// 챕터당 최대 이미지 수
    #[arg(long, default_value_t = 2)]
    max_images: usize,
    /// 요약에 쓸 Claude 연결 방식
    #[arg(long, value_enum, default_value_t = BackendChoice::Auto)]
    summarizer: BackendChoice,
    /// 중간 결과 저장 폴더
    #[arg(long, default_value = ".ytdoc_cache")]
    cache_dir: PathBuf,
    /// 요약을 Obsidian 노트로도 저장 (Obsidian이 마지막으로 연 보관함의 "YouTube 요약" 폴더)
    #[arg(long)]
    obsidian: bool,
    /// 노트를 저장할 Obsidian 보관함 폴더 (지정하면 --obsidian이 켜집니다)
    #[arg(long, value_name = "FOLDER", env = "YTDOC_OBSIDIAN_VAULT")]
    obsidian_vault: Option<PathBuf>,
}

impl Common {
    fn options(self, output: Option<PathBuf>) -> anyhow::Result<Options> {
        // Find the vault before the long work starts, so a mistake shows up at once.
        let obsidian = match (self.obsidian_vault, self.obsidian) {
            (Some(vault), _) => Some(obsidian::check_vault(&vault)?),
            (None, true) => Some(obsidian::find_vault()?),
            (None, false) => None,
        };
        Ok(Options {
            cache_dir: self.cache_dir,
            whisper_model: self.whisper_model,
            max_images: self.max_images,
            summarizer: self.summarizer,
            translate: self.translate,
            output,
            obsidian,
        })
    }
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Serve { port, common }) => common.options(None).and_then(|opts| web::serve(port, opts)),
        None => cli.common.options(cli.output).and_then(|opts| run_cli(&cli.source.expect("clap requires it"), opts)),
    };
    if let Err(err) = result {
        eprintln!("\n오류: {err:#}");
        std::process::exit(1);
    }
}

fn run_cli(arg: &str, opts: Options) -> anyhow::Result<()> {
    let source = Source::parse(arg)?;
    let printer = Printer { tty: std::io::stderr().is_terminal(), began: Instant::now(), rows: Mutex::new(vec![]) };
    let done = run_pipeline(&source, &opts, &printer)?;
    printer.report();
    let pdf = std::path::absolute(&done.pdf).unwrap_or(done.pdf);
    println!("\nPDF: {}", pdf.display());
    if let Some(note) = done.note {
        println!("옵시디언 노트: {}", note.display());
    }
    Ok(())
}

struct Printer {
    tty: bool,
    began: Instant,
    rows: Mutex<Vec<(Stage, Duration, Outcome)>>,
}

impl Progress for Printer {
    fn event(&self, event: Event) {
        let mut err = std::io::stderr().lock();
        let _ = match event {
            Event::Started(stage) => writeln!(err, "▶ {}...", stage.label()),
            // Only a terminal can overwrite a line; logs get the final state alone.
            Event::Detail(_, msg) if self.tty => write!(err, "\r\x1b[2K  {msg}"),
            Event::Detail(..) => Ok(()),
            Event::Finished(stage, elapsed, outcome) => {
                self.rows.lock().unwrap().push((stage, elapsed, outcome));
                let word = match outcome {
                    Outcome::Done => "완료",
                    Outcome::Cached => "캐시 사용",
                    Outcome::Skipped => "건너뜀",
                };
                let clear = if self.tty { "\r\x1b[2K" } else { "" };
                writeln!(err, "{clear}  {} {word} ({:.1}s)", stage.label(), elapsed.as_secs_f64())
            }
        };
    }
}

impl Printer {
    fn report(&self) {
        let rows = self.rows.lock().unwrap();
        eprintln!("\n단계별 소요 시간");
        for (stage, elapsed, outcome) in rows.iter() {
            let note = match outcome {
                Outcome::Done => "",
                Outcome::Cached => "  (cached)",
                Outcome::Skipped => "  (skipped)",
            };
            eprintln!("  {:<12} {:7.1}s{note}", format!("{stage:?}").to_lowercase(), elapsed.as_secs_f64());
        }
        // Some stages overlap, so the total is wall-clock time, not their sum.
        eprintln!("  {:<12} {:7.1}s", "total", self.began.elapsed().as_secs_f64());
    }
}
