use std::fmt;
use std::str::FromStr;

use serde::Serialize;

/// A Whisper-style language code such as "ko" or "en".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LangCode(String);

impl FromStr for LangCode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let code = s.trim().to_ascii_lowercase();
        if (2..=3).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_lowercase()) {
            Ok(LangCode(code))
        } else {
            Err(format!("'{s}'은(는) 언어 코드가 아닙니다. 예: ko, en, ja"))
        }
    }
}

impl fmt::Display for LangCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl LangCode {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name Claude gets in prompts.
    pub fn english_name(&self) -> String {
        LANGUAGES
            .iter()
            .find(|l| l.code == self.0)
            .map_or_else(|| self.0.clone(), |l| format!("{} ({})", l.english, l.native))
    }

    /// Fixed words of the PDF (headings, cover labels), English when there is no table for the language.
    pub fn labels(&self) -> &'static Labels {
        LANGUAGES
            .iter()
            .find(|l| l.code == self.0)
            .unwrap_or(&LANGUAGES[1])
            .labels
    }
}

pub struct Language {
    pub code: &'static str,
    pub english: &'static str,
    pub native: &'static str,
    pub labels: &'static Labels,
}

#[derive(Serialize)]
pub struct Labels {
    pub kicker: &'static str,
    pub kicker_local: &'static str,
    pub original: &'static str,
    pub channel: &'static str,
    pub duration: &'static str,
    pub uploaded: &'static str,
    pub url: &'static str,
    pub file: &'static str,
    pub generated: &'static str,
    pub overview: &'static str,
    pub contents: &'static str,
    pub key_points: &'static str,
    pub transcript: &'static str,
    pub translated_from: &'static str,
    pub concept_map: &'static str,
    pub comparison: &'static str,
    pub glossary: &'static str,
    /// Title of the folded box that holds a chapter's details in the Obsidian note.
    pub details: &'static str,
}

/// The languages the web page offers, in its dropdown order.
pub const LANGUAGES: &[Language] = &[
    Language {
        code: "ko",
        english: "Korean",
        native: "한국어",
        labels: &Labels {
            kicker: "YOUTUBE 영상 요약",
            kicker_local: "영상 요약",
            original: "원제",
            channel: "채널",
            duration: "길이",
            uploaded: "게시일",
            url: "주소",
            file: "파일",
            generated: "생성일",
            overview: "전체 요약",
            contents: "목차",
            key_points: "핵심 포인트",
            transcript: "부록: 전체 받아쓰기",
            translated_from: "번역 원문 언어",
            concept_map: "개념 관계도",
            comparison: "비교",
            glossary: "용어 정리",
            details: "자세히",
        },
    },
    Language {
        code: "en",
        english: "English",
        native: "English",
        labels: &Labels {
            kicker: "YOUTUBE VIDEO SUMMARY",
            kicker_local: "VIDEO SUMMARY",
            original: "Original title",
            channel: "Channel",
            duration: "Length",
            uploaded: "Published",
            url: "URL",
            file: "File",
            generated: "Generated",
            overview: "Overview",
            contents: "Contents",
            key_points: "Key points",
            transcript: "Appendix: full transcript",
            translated_from: "Translated from",
            concept_map: "Concept map",
            comparison: "Comparison",
            glossary: "Glossary",
            details: "Details",
        },
    },
    Language {
        code: "ja",
        english: "Japanese",
        native: "日本語",
        labels: &Labels {
            kicker: "YOUTUBE 動画の要約",
            kicker_local: "動画の要約",
            original: "元のタイトル",
            channel: "チャンネル",
            duration: "長さ",
            uploaded: "公開日",
            url: "URL",
            file: "ファイル",
            generated: "作成日",
            overview: "全体の要約",
            contents: "目次",
            key_points: "要点",
            transcript: "付録: 全文書き起こし",
            translated_from: "翻訳元の言語",
            concept_map: "概念マップ",
            comparison: "比較",
            glossary: "用語集",
            details: "詳細",
        },
    },
    Language {
        code: "zh",
        english: "Chinese",
        native: "中文",
        labels: &Labels {
            kicker: "YOUTUBE 视频摘要",
            kicker_local: "视频摘要",
            original: "原标题",
            channel: "频道",
            duration: "时长",
            uploaded: "发布日期",
            url: "网址",
            file: "文件",
            generated: "生成日期",
            overview: "总体摘要",
            contents: "目录",
            key_points: "要点",
            transcript: "附录：完整文字记录",
            translated_from: "原文语言",
            concept_map: "概念图",
            comparison: "对比",
            glossary: "术语表",
            details: "详情",
        },
    },
    Language {
        code: "es",
        english: "Spanish",
        native: "Español",
        labels: &Labels {
            kicker: "RESUMEN DE VÍDEO DE YOUTUBE",
            kicker_local: "RESUMEN DE VÍDEO",
            original: "Título original",
            channel: "Canal",
            duration: "Duración",
            uploaded: "Publicado",
            url: "URL",
            file: "Archivo",
            generated: "Generado",
            overview: "Resumen general",
            contents: "Contenido",
            key_points: "Puntos clave",
            transcript: "Apéndice: transcripción completa",
            translated_from: "Traducido del",
            concept_map: "Mapa de conceptos",
            comparison: "Comparación",
            glossary: "Glosario",
            details: "Detalles",
        },
    },
    Language {
        code: "fr",
        english: "French",
        native: "Français",
        labels: &Labels {
            kicker: "RÉSUMÉ DE VIDÉO YOUTUBE",
            kicker_local: "RÉSUMÉ DE VIDÉO",
            original: "Titre original",
            channel: "Chaîne",
            duration: "Durée",
            uploaded: "Publiée le",
            url: "URL",
            file: "Fichier",
            generated: "Générée le",
            overview: "Vue d'ensemble",
            contents: "Sommaire",
            key_points: "Points clés",
            transcript: "Annexe : transcription complète",
            translated_from: "Traduit de",
            concept_map: "Carte des concepts",
            comparison: "Comparaison",
            glossary: "Glossaire",
            details: "Détails",
        },
    },
    Language {
        code: "de",
        english: "German",
        native: "Deutsch",
        labels: &Labels {
            kicker: "YOUTUBE-VIDEO-ZUSAMMENFASSUNG",
            kicker_local: "VIDEO-ZUSAMMENFASSUNG",
            original: "Originaltitel",
            channel: "Kanal",
            duration: "Länge",
            uploaded: "Veröffentlicht",
            url: "URL",
            file: "Datei",
            generated: "Erstellt",
            overview: "Überblick",
            contents: "Inhalt",
            key_points: "Kernpunkte",
            transcript: "Anhang: vollständiges Transkript",
            translated_from: "Übersetzt aus",
            concept_map: "Begriffskarte",
            comparison: "Vergleich",
            glossary: "Glossar",
            details: "Details",
        },
    },
    Language {
        code: "vi",
        english: "Vietnamese",
        native: "Tiếng Việt",
        labels: &Labels {
            kicker: "TÓM TẮT VIDEO YOUTUBE",
            kicker_local: "TÓM TẮT VIDEO",
            original: "Tiêu đề gốc",
            channel: "Kênh",
            duration: "Thời lượng",
            uploaded: "Ngày đăng",
            url: "Địa chỉ",
            file: "Tệp",
            generated: "Ngày tạo",
            overview: "Tổng quan",
            contents: "Mục lục",
            key_points: "Ý chính",
            transcript: "Phụ lục: toàn bộ lời thoại",
            translated_from: "Dịch từ",
            concept_map: "Sơ đồ khái niệm",
            comparison: "So sánh",
            glossary: "Thuật ngữ",
            details: "Chi tiết",
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes_codes() {
        assert_eq!("EN".parse::<LangCode>().unwrap().as_str(), "en");
        assert_eq!(" ko ".parse::<LangCode>().unwrap().as_str(), "ko");
        assert!("english".parse::<LangCode>().is_err());
        assert!("e1".parse::<LangCode>().is_err());
    }

    #[test]
    fn unknown_language_falls_back_to_english_labels_and_raw_name() {
        let sw: LangCode = "sw".parse().unwrap();
        assert_eq!(sw.labels().overview, "Overview");
        assert_eq!(sw.english_name(), "sw");
        let ko: LangCode = "ko".parse().unwrap();
        assert_eq!(ko.english_name(), "Korean (한국어)");
    }
}
