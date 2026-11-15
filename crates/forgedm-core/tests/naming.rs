//! S1.1 naming tests (THREAT_MODEL FS-01, ARCHITECTURE 4.14).

use forgedm_core::naming::content_disposition::parse_filename;
use forgedm_core::naming::{
    COLLISION_MAX, FALLBACK_NAME, MAX_CONTENT_DISPOSITION_BYTES, NAME_MAX_UTF16, PART_SUFFIX_UTF16,
    collision_candidate, sanitize, suggest,
};
use proptest::prelude::*;
use url::Url;
use uuid::Uuid;

const FORBIDDEN: &str = "<>:\"/\\|?*";
const RESERVED: [&str; 8 + 20] = [
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "CONIN$",
    "CONOUT$",
    "COM0",
    "LPT0",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "COM\u{b9}",
    "LPT\u{b3}",
];

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn fixed_id() -> Uuid {
    Uuid::from_u128(0x1234_5678_9abc_7def_8123_4567_89ab_cdef)
}

/// Parses a literal test URL (the unwrap expands inside the calling `#[test]`).
macro_rules! url {
    ($text:expr) => {
        Url::parse($text).unwrap()
    };
}

/// Independent oracle for the reserved-name rule (ARCHITECTURE 4.14 step 5).
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    let upper = stem.to_uppercase();
    let superscript = |prefix: &str| {
        ["\u{b9}", "\u{b2}", "\u{b3}"]
            .iter()
            .any(|s| stem == format!("{prefix}{s}"))
    };
    RESERVED.contains(&upper.as_str()) || superscript("COM") || superscript("LPT")
}

fn assert_safe(name: &str) {
    assert!(!name.is_empty(), "empty");
    assert!(name != "." && name != "..", "{name:?}");
    for c in name.chars() {
        assert!(!FORBIDDEN.contains(c), "forbidden {c:?} in {name:?}");
        assert!(!c.is_control(), "control {c:?} in {name:?}");
        assert!(
            !matches!(c, '\u{ad}' | '\u{61c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'),
            "format control {c:?} in {name:?}"
        );
    }
    assert!(!name.ends_with('.') && !name.ends_with(' '), "{name:?}");
    assert!(!name.starts_with(' '), "{name:?}");
    assert!(!is_reserved(name), "reserved {name:?}");
    assert!(
        utf16_len(name) <= NAME_MAX_UTF16,
        "too long: {}",
        utf16_len(name)
    );
}

#[test]
fn constants_match_appendix_a9() {
    assert_eq!(NAME_MAX_UTF16, 233);
    assert_eq!(PART_SUFFIX_UTF16, 22);
    assert_eq!(NAME_MAX_UTF16 + PART_SUFFIX_UTF16, 255);
    assert_eq!(COLLISION_MAX, 999);
    assert_eq!(MAX_CONTENT_DISPOSITION_BYTES, 4096);
    assert_eq!(FALLBACK_NAME, "download");
}

#[test]
fn cd_prefers_filename_star_rfc6266() {
    let star_first = "attachment; filename*=UTF-8''b%20c.txt; filename=\"a.txt\"";
    let star_last = "attachment; filename=\"a.txt\"; filename*=UTF-8''b%20c.txt";
    assert_eq!(parse_filename(star_first).as_deref(), Some("b c.txt"));
    assert_eq!(parse_filename(star_last).as_deref(), Some("b c.txt"));
    // Parameter names are case-insensitive; quoted strings may contain `;` and escapes.
    assert_eq!(
        parse_filename("attachment; FILENAME=\"a;b \\\"q\\\".txt\"").as_deref(),
        Some("a;b \"q\".txt")
    );
    // Token form, and no filename at all.
    assert_eq!(
        parse_filename("attachment; filename=plain.bin").as_deref(),
        Some("plain.bin")
    );
    assert_eq!(parse_filename("attachment"), None);
    assert_eq!(parse_filename("inline; foo=bar"), None);
    assert_eq!(parse_filename(""), None);
    // Anything above 4 KiB is ignored; exactly 4 KiB is still parsed.
    let pad = |n: usize| format!("attachment; filename=\"{}\"", "a".repeat(n));
    let base = pad(0).len();
    assert!(parse_filename(&pad(MAX_CONTENT_DISPOSITION_BYTES - base)).is_some());
    assert_eq!(
        parse_filename(&pad(MAX_CONTENT_DISPOSITION_BYTES - base + 1)),
        None
    );
}

#[test]
fn rfc5987_decode_and_invalid_fallback() {
    let with_fallback =
        |star: &str| format!("attachment; filename*={star}; filename=\"fallback.txt\"");
    // Valid UTF-8, with and without a language tag; Latin-1.
    assert_eq!(
        parse_filename(&with_fallback("UTF-8''%E2%82%AC%20rates.txt")).as_deref(),
        Some("\u{20ac} rates.txt")
    );
    assert_eq!(
        parse_filename(&with_fallback("utf-8'en'caf%C3%A9.txt")).as_deref(),
        Some("caf\u{e9}.txt")
    );
    assert_eq!(
        parse_filename(&with_fallback("ISO-8859-1''caf%E9.txt")).as_deref(),
        Some("caf\u{e9}.txt")
    );
    // Invalid forms fall back to `filename`.
    for bad in [
        "UTF-8''%ZZ.txt",
        "UTF-8''%E2%82",
        "UTF-8''%FF%FE.txt",
        "UTF-8''trunc%",
        "KOI8-R''x.txt",
        "no-quotes.txt",
        "UTF-8'only-one-quote.txt",
    ] {
        assert_eq!(
            parse_filename(&with_fallback(bad)).as_deref(),
            Some("fallback.txt"),
            "{bad}"
        );
    }
    // Invalid with nothing to fall back to.
    assert_eq!(parse_filename("attachment; filename*=UTF-8''%ZZ"), None);
}

#[test]
fn cd_path_components_reduced_to_basename() {
    let id = fixed_id();
    let final_url = url!("https://example.com/download");
    let plain = |value: &str| format!("attachment; filename=\"{value}\"");
    let star = |value: &str| format!("attachment; filename*=UTF-8''{value}");
    let cases: [(String, &str); 8] = [
        (plain("../../x.exe"), "x.exe"),
        (plain("..\\x"), "x"),
        (plain("C:\\x"), "x"),
        (plain("\\\\srv\\x"), "x"),
        (star("%2e%2e%2fx"), "x"),
        (star("..%5Cx"), "x"),
        (star("C%3A%5Cwindows%5Csystem32%5Cx"), "x"),
        // A plain filename is not percent-decoded, so the literal text is a safe name.
        (plain("%2e%2e%2fx"), "%2e%2e%2fx"),
    ];
    for (header, expected) in &cases {
        let suggested = suggest(Some(header), &final_url, id);
        assert_eq!(suggested.file_name, *expected, "{header}");
        assert_eq!(suggested.display_name, *expected, "{header}");
        assert_safe(&suggested.file_name);
    }
}

#[test]
fn ads_colon_never_survives() {
    assert_eq!(sanitize("file.txt:stream:$DATA"), "file.txtstream$DATA");
    assert_eq!(sanitize("a:b"), "ab");
    assert_eq!(sanitize("C:evil.exe"), "Cevil.exe");
    assert_eq!(sanitize(":"), FALLBACK_NAME);
    for input in ["x:y", "::$DATA", "a.txt::$DATA", "con:", "\u{ff1a}"] {
        assert!(!sanitize(input).contains(':'), "{input}");
    }
}

#[test]
fn reserved_names_union_rejected() {
    for name in [
        "CON",
        "con",
        "Prn",
        "AUX",
        "NUL",
        "NUL.tar.gz",
        "nul .txt",
        "nul  .txt",
        "con.txt",
        "COM0",
        "COM1",
        "com9.txt",
        "LPT0",
        "lpt9",
        "CONIN$",
        "conout$.log",
        "COM\u{b9}",
        "COM\u{b2}.txt",
        "COM\u{b3}",
        "LPT\u{b9}",
        "lpt\u{b2}",
        "LPT\u{b3}.dat",
        "NUL. ",
        "con ",
    ] {
        let cleaned = sanitize(name);
        assert!(cleaned.starts_with('_'), "{name:?} -> {cleaned:?}");
        assert_safe(&cleaned);
    }
    // Specific outputs.
    assert_eq!(sanitize("NUL.tar.gz"), "_NUL.tar.gz");
    assert_eq!(sanitize("nul .txt"), "_nul .txt");
    // Near misses are left alone.
    for name in [
        "COM10",
        "CONSOLE",
        "COMA",
        "NULL",
        "comma.txt",
        "LPT",
        "CON1",
        "xcon",
        "COM\u{b4}",
    ] {
        assert_eq!(sanitize(name), name);
    }
}

#[test]
fn trailing_dots_spaces_trimmed() {
    assert_eq!(sanitize("a.txt. . ."), "a.txt");
    assert_eq!(sanitize("  lead.txt"), "lead.txt");
    assert_eq!(sanitize("name  "), "name");
    assert_eq!(sanitize("a. "), "a");
    assert_eq!(sanitize(" . a . "), ". a");
    assert_eq!(sanitize(".hidden"), ".hidden");
    assert_eq!(sanitize("in side.txt"), "in side.txt");
}

#[test]
fn bidi_controls_removed() {
    assert_eq!(sanitize("a\u{202e}txt.exe"), "atxt.exe");
    assert_eq!(sanitize("\u{feff}x"), "x");
    assert_eq!(sanitize("\u{200b}x\u{2066}y\u{2069}z"), "xyz");
    let all: String = ('\u{200b}'..='\u{200f}')
        .chain('\u{202a}'..='\u{202e}')
        .chain('\u{2066}'..='\u{2069}')
        .chain(std::iter::once('\u{feff}'))
        .chain("ok.txt".chars())
        .collect();
    assert_eq!(sanitize(&all), "ok.txt");
    assert_eq!(sanitize("a\u{0}b\u{1f}c\u{7f}d.txt"), "abcd.txt");
}

#[test]
fn c1_and_invisible_format_characters_removed() {
    assert_eq!(sanitize("a\u{85}b\u{61c}.txt"), "ab.txt");
    assert_eq!(sanitize("a\u{80}\u{9f}b\u{ad}c.txt"), "abc.txt");
    assert_eq!(
        sanitize("x\u{2028}y\u{2029}z\u{2060}\u{2064}.bin"),
        "xyz.bin"
    );
    // Reaches the sanitiser through a percent-decoded Content-Disposition value.
    let raw = parse_filename("attachment; filename*=UTF-8''a%C2%85b.txt").unwrap_or_default();
    assert_eq!(sanitize(&raw), "ab.txt");
}

#[test]
fn cd_filename_without_disposition_type_is_found() {
    assert_eq!(
        parse_filename("filename=\"a.zip\"").as_deref(),
        Some("a.zip")
    );
    assert_eq!(
        parse_filename("; filename=\"b.zip\"").as_deref(),
        Some("b.zip")
    );
}

#[test]
fn cd_legacy_backslash_paths_keep_later_params() {
    // Only `\"` and `\` are escapes; other backslashes are literal, in splitting and in
    // unquoting alike.
    let got = parse_filename(r#"attachment; filename="C:\dir\file.txt"; x=1"#);
    assert_eq!(got.as_deref(), Some(r"C:\dir\file.txt"));
    let got = parse_filename(r#"attachment; x="a\q"; filename="k.txt""#);
    assert_eq!(got.as_deref(), Some("k.txt"));
    // A properly escaped trailing backslash closes the quote.
    let got = parse_filename(r#"attachment; filename="C:\dir\\"; filename*=UTF-8''z.txt"#);
    assert_eq!(got.as_deref(), Some("z.txt"));
}

#[test]
fn cd_unusable_filename_star_falls_back_to_plain() {
    for star in ["UTF-8''", "UTF-8''%20%20"] {
        let header = format!("attachment; filename*={star}; filename=\"real.txt\"");
        assert_eq!(
            parse_filename(&header).as_deref(),
            Some("real.txt"),
            "{star}"
        );
    }
}

#[test]
fn empty_dot_dotdot_fallback() {
    for input in [
        "", ".", "..", "...", "  ", " . ", "/", "a/..", "a\\..", "<>", "?*", "\u{202e}", ":",
    ] {
        assert_eq!(sanitize(input), FALLBACK_NAME, "{input:?}");
    }
}

#[test]
fn length_capped_at_233_preserving_extension() {
    let long = format!("{}.tar.gz", "a".repeat(300));
    let cleaned = sanitize(&long);
    assert_eq!(utf16_len(&cleaned), NAME_MAX_UTF16);
    assert!(cleaned.ends_with(".gz"), "{cleaned}");

    let txt = sanitize(&format!("{}.txt", "b".repeat(250)));
    assert_eq!(utf16_len(&txt), NAME_MAX_UTF16);
    assert!(txt.ends_with(".txt"));

    // 233 exactly is untouched; 234 is cut.
    assert_eq!(sanitize(&"c".repeat(233)).len(), 233);
    assert_eq!(utf16_len(&sanitize(&"c".repeat(234))), NAME_MAX_UTF16);

    // An extension longer than 16 units is not preserved: the whole name is cut.
    let odd = sanitize(&format!("{}.{}", "d".repeat(300), "e".repeat(20)));
    assert_eq!(utf16_len(&odd), NAME_MAX_UTF16);
    assert!(odd.starts_with("ddd"));

    // Surrogate pairs are never split.
    let emoji = sanitize(&"\u{1f600}".repeat(200));
    assert!(utf16_len(&emoji) <= NAME_MAX_UTF16);
    assert!(!emoji.is_empty() && emoji.chars().all(|c| c == '\u{1f600}'));
    let emoji_ext = sanitize(&format!("{}.\u{1f600}", "f".repeat(300)));
    assert!(emoji_ext.ends_with(".\u{1f600}"));
    assert!(utf16_len(&emoji_ext) <= NAME_MAX_UTF16);

    // A cut that exposes trailing spaces or dots is trimmed again.
    let spaced = sanitize(&format!("con{}x", " ".repeat(300)));
    assert_safe(&spaced);
}

#[test]
fn cd_name_255_units_fits_part_suffix() {
    let name = format!("{}.exe", "a".repeat(251));
    assert_eq!(utf16_len(&name), 255);
    let header = format!("attachment; filename=\"{name}\"");
    let suggested = suggest(Some(&header), &url!("https://example.com/x"), fixed_id());
    assert!(utf16_len(&suggested.file_name) <= NAME_MAX_UTF16);
    assert!(suggested.file_name.ends_with(".exe"));
    let part = format!("{}.{}.forgedm.part", suggested.file_name, "12345678");
    assert!(utf16_len(&part) <= 255, "{}", utf16_len(&part));
    assert_eq!(utf16_len(".12345678.forgedm.part"), PART_SUFFIX_UTF16);
}

fn arb_name() -> impl Strategy<Value = String> {
    let alphabet: Vec<&'static str> = vec![
        "a",
        "b",
        "c",
        "o",
        "n",
        "u",
        "l",
        "C",
        "O",
        "N",
        "M",
        "P",
        "R",
        "X",
        "1",
        "9",
        "$",
        "_",
        ".",
        ".",
        " ",
        " ",
        "/",
        "\\",
        ":",
        "<",
        ">",
        "\"",
        "|",
        "?",
        "*",
        "\0",
        "\u{1f}",
        "\u{7f}",
        "\u{85}",
        "\u{9f}",
        "\u{ad}",
        "\u{61c}",
        "\u{2028}",
        "\u{2029}",
        "\u{2060}",
        "\u{2064}",
        "\u{b9}",
        "\u{b2}",
        "\u{b3}",
        "\u{200b}",
        "\u{202e}",
        "\u{2066}",
        "\u{feff}",
        "\u{1f600}",
        "\u{20ac}",
        "con",
        "nul",
        "COM",
        "LPT",
        "AUX",
        "PRN",
        "CONIN$",
        "CONOUT$",
        ".tar.gz",
    ];
    prop_oneof![
        proptest::collection::vec(proptest::sample::select(alphabet), 0..400)
            .prop_map(|parts| parts.concat()),
        proptest::collection::vec(
            proptest::sample::select(vec!["a", " ", ".", "con", "x"]),
            200..400
        )
        .prop_map(|parts| parts.concat()),
        any::<String>(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn proptest_sanitized_always_safe_and_idempotent(input in arb_name()) {
        let once = sanitize(&input);
        assert_safe(&once);
        prop_assert_eq!(sanitize(&once), once.clone());
        let part = format!("{once}.12345678.forgedm.part");
        prop_assert!(utf16_len(&part) <= 255);
    }

    #[test]
    fn proptest_collision_candidates_are_safe_and_stable(input in arb_name(), n in 1u32..=COLLISION_MAX) {
        let base = sanitize(&input);
        let candidate = collision_candidate(&base, n);
        assert_safe(&candidate);
        prop_assert_eq!(sanitize(&candidate), candidate.clone());
        let suffix = format!(" ({n})");
        prop_assert!(candidate.contains(&suffix));
    }

    #[test]
    fn proptest_parse_filename_never_panics(header in any::<String>()) {
        let _ = parse_filename(&header);
    }
}

#[test]
fn name_from_url_path_when_no_cd() {
    let id = fixed_id();
    let named = suggest(
        None,
        &url!("https://example.com/files/My%20Report.pdf?x=1#f"),
        id,
    );
    assert_eq!(named.file_name, "My Report.pdf");
    assert_eq!(named.display_name, "My Report.pdf");

    // Encoded separators cannot smuggle a path.
    assert_eq!(
        suggest(None, &url!("https://example.com/a/..%5Cevil.exe"), id).file_name,
        "evil.exe"
    );
    assert_eq!(
        suggest(None, &url!("https://example.com/a%2Fb.bin"), id).file_name,
        "b.bin"
    );
    // Non-UTF-8 percent bytes do not panic and stay safe.
    assert_safe(&suggest(None, &url!("https://example.com/%FF%FE.bin"), id).file_name);

    // A CD that carries no usable name falls through to the URL path.
    for header in [
        "attachment",
        "attachment; filename*=UTF-8''%ZZ",
        "attachment; filename=\"..\"",
    ] {
        assert_eq!(
            suggest(Some(header), &url!("https://example.com/x/real.iso"), id).file_name,
            "real.iso",
            "{header}"
        );
    }
    // CD wins over the URL when it has a name.
    assert_eq!(
        suggest(
            Some("attachment; filename=\"cd.zip\""),
            &url!("https://example.com/x/real.iso"),
            id
        )
        .file_name,
        "cd.zip"
    );
    // Nothing usable anywhere: a stable fallback built from the id.
    for text in [
        "https://example.com/",
        "https://example.com/dir/",
        "https://example.com",
    ] {
        let fallback = suggest(None, &url!(text), id);
        assert_eq!(fallback.file_name, "download-12345678", "{text}");
    }
}

#[test]
fn collision_candidate_keeps_compound_extension() {
    let cases: [(&str, u32, &str); 12] = [
        ("a.tar.gz", 1, "a (1).tar.gz"),
        ("A.TAR.GZ", 2, "A (2).TAR.GZ"),
        ("x.tar.xz", 999, "x (999).tar.xz"),
        ("x.tar.bz2", 3, "x (3).tar.bz2"),
        ("x.tar.zst", 4, "x (4).tar.zst"),
        ("report.pdf", 2, "report (2).pdf"),
        ("a.b.zip", 1, "a.b (1).zip"),
        ("noext", 3, "noext (3)"),
        (".bashrc", 1, ".bashrc (1)"),
        ("a.gz", 1, "a (1).gz"),
        ("tar.gz", 1, "tar (1).gz"),
        ("report.pdf", 0, "report.pdf"),
    ];
    for (name, n, expected) in cases {
        assert_eq!(collision_candidate(name, n), expected, "{name} {n}");
    }
    // A candidate for a maximum-length name is cut to fit and keeps its extension.
    let long = sanitize(&format!("{}.tar.gz", "a".repeat(300)));
    let candidate = collision_candidate(&long, 1);
    assert!(utf16_len(&candidate) <= NAME_MAX_UTF16);
    assert!(candidate.ends_with(" (1).tar.gz"), "{candidate}");
    assert_eq!(sanitize(&candidate), candidate);
}
