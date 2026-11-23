//! The plain Rust rules: links, email addresses, phone numbers and dates in the languages of the
//! app's catalogs (English, Turkish, German, Spanish and Russian month names, and the 年月日 of
//! Japanese and Chinese). They run on every platform; on macOS they add what NSDataDetector
//! missed.
//!
//! regex-lite knows ASCII only: its `\b` and `(?i)` stop at "Kasım" or "ноября". The patterns
//! therefore match letters by code point ranges, and the edges of a piece and the month and
//! weekday names are checked in Rust, with Unicode case folding.

use std::ops::Range;
use std::sync::OnceLock;

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};
use regex_lite::{Captures, Regex};

use super::{Candidate, EntityDate, EntityKind};
use crate::link_for;

/// Every piece the rules find in `text`, without overlaps, sorted by position.
pub(super) fn find(text: &str, now: NaiveDateTime) -> Vec<Candidate> {
    let mut found: Vec<Candidate> = Vec::new();
    // The order settles nested pieces: an address holds a domain, and a date's digits could pass
    // for a phone number.
    let all = emails(text).into_iter().chain(links(text)).chain(dates(text, now)).chain(phones(text));
    for candidate in all {
        if !found.iter().any(|c| c.overlaps(&candidate)) {
            found.push(candidate);
        }
    }
    found.sort_by_key(|c| c.range.start);
    found
}

fn compiled(cell: &'static OnceLock<Regex>, pattern: impl FnOnce() -> String) -> &'static Regex {
    cell.get_or_init(|| Regex::new(&pattern()).expect("the rule's pattern compiles"))
}

/// A letter of the scripts the date rules read: Latin with its accents and Turkish letters, and
/// Cyrillic.
const LETTER: &str = "[A-Za-zÀ-ÖØ-öø-ÿĀ-ſА-яЁё]";

fn char_before(text: &str, at: usize) -> Option<char> {
    text[..at].chars().next_back()
}

fn char_after(text: &str, at: usize) -> Option<char> {
    text[at..].chars().next()
}

fn word_char(c: Option<char>) -> bool {
    c.is_some_and(char::is_alphanumeric)
}

// ---------------------------------------------------------------------------- addresses

/// Whether `text` is one plain address: ASCII letters, digits and `._+-` before the `@`, and a
/// domain whose last label is two or more letters. `%`, `?`, `#`, `&` and `,` never appear, so a
/// `mailto:` made of it carries no header and no second recipient.
pub(super) fn is_address(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else { return false };
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && labels.last().is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
        && labels.last().is_some_and(|tld| !FILE_TYPES.contains(&tld.to_ascii_lowercase().as_str()));
    local_ok && domain_ok
}

/// The file types a name such as `icon@2x.png` or `logo@3x.webp` ends in. No address is written
/// in them; `.zip` and `.mov` are domains too, but a mailbox there is far rarer than a file.
const FILE_TYPES: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "avif", "heic", "heif", "svg", "bmp", "tif", "tiff", "ico", "psd", "pdf",
    "txt", "csv", "json", "xml", "yml", "yaml", "toml", "html", "htm", "css", "js", "jsx", "ts", "tsx", "vue", "swift",
    "kt", "java", "rb", "php", "lock", "log", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "zip", "gz", "tgz", "tar",
    "rar", "dmg", "exe", "dll", "mov", "mp4", "webm", "mp3", "m4a", "wav", "ttf", "otf", "woff", "woff2", "wasm",
    "ini", "cfg", "bak", "tmp",
];

fn emails(text: &str) -> Vec<Candidate> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = compiled(&RE, || r"[A-Za-z0-9._+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,24}".into());
    re.find_iter(text)
        .filter_map(|m| {
            let mut range = m.range();
            while text[range.clone()].starts_with('.') {
                range.start += 1;
            }
            let after = char_after(text, range.end);
            if word_char(char_before(text, range.start)) || word_char(after) || after == Some('@') {
                return None;
            }
            let piece = &text[range.clone()];
            is_address(piece).then(|| Candidate { kind: EntityKind::Email, range, value: piece.into(), date: None })
        })
        .collect()
}

// -------------------------------------------------------------------------------- links

/// What a link may hold: anything but spaces, angle brackets and quotes.
const LINK_CHARS: &str = r#"[^\s<>"'`“”‘’«»]"#;

/// The top-level domains a bare domain is taken with. File names ("main.rs", "README.md") and
/// abbreviations ("Tel.No", "Sign.in") end in letters that are also country domains, so only
/// these count.
const TLDS: &[&str] = &[
    "com", "net", "org", "edu", "gov", "info", "biz", "io", "app", "dev", "co", "me", "xyz", "site", "online", "store",
    "shop", "tech", "cloud", "page", "link", "tr", "de", "at", "ch", "es", "mx", "ar", "ru", "ua", "kz", "uk", "ie",
    "fr", "be", "nl", "it", "pt", "br", "se", "dk", "fi", "cz", "gr", "hu", "ro", "eu", "us", "ca", "au", "nz", "jp",
    "cn", "kr", "tw", "hk", "sg", "az",
];

fn links(text: &str) -> Vec<Candidate> {
    static SCHEME: OnceLock<Regex> = OnceLock::new();
    static WWW: OnceLock<Regex> = OnceLock::new();
    static BARE: OnceLock<Regex> = OnceLock::new();
    let label = r"[A-Za-z0-9](?:[A-Za-z0-9\-]*[A-Za-z0-9])?";
    let scheme = compiled(&SCHEME, || format!(r"(?i)\bhttps?://{LINK_CHARS}+"));
    let www = compiled(&WWW, || format!(r"(?i)\bwww\.{label}(?:\.{label})+(?:[/?#]{LINK_CHARS}*)?"));
    let bare = compiled(&BARE, || format!(r"\b{label}(?:\.{label})*\.([A-Za-z]{{2,12}})(?:[/?#]{LINK_CHARS}*)?"));

    let mut out: Vec<Candidate> = Vec::new();
    let add = |out: &mut Vec<Candidate>, range: Range<usize>, scheme_given: bool| {
        let range = trim_link(text, range);
        let piece = &text[range.clone()];
        let after = char_after(text, range.end);
        if piece.is_empty() || word_char(after) || after == Some('@') {
            return;
        }
        let value = if scheme_given { piece.to_string() } else { format!("https://{piece}") };
        let candidate = Candidate { kind: EntityKind::Link, range, value, date: None };
        if link_for(&candidate.value).is_some() && !out.iter().any(|c| c.overlaps(&candidate)) {
            out.push(candidate);
        }
    };
    for m in scheme.find_iter(text) {
        add(&mut out, m.range(), true);
    }
    for m in www.find_iter(text) {
        if !matches!(char_before(text, m.start()), Some('.' | '@' | '/')) {
            add(&mut out, m.range(), false);
        }
    }
    for caps in bare.captures_iter(text) {
        let (Some(whole), Some(tld)) = (caps.get(0), caps.get(1)) else { continue };
        let before = char_before(text, whole.start());
        if word_char(before) || matches!(before, Some('.' | '@' | '/' | ':' | '-' | '_')) {
            continue;
        }
        if TLDS.contains(&tld.as_str().to_ascii_lowercase().as_str()) {
            add(&mut out, whole.range(), false);
        }
    }
    out
}

/// Whether a link written without a scheme ends in a known domain ([`TLDS`]) or starts with
/// `www.`, so "main.rs" and "README.md" stay file names.
// Only the NSDataDetector pass (apple.rs, macOS) calls this and the five other items marked the
// same way; the rules keep them on every platform so their tests run everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn known_domain(piece: &str) -> bool {
    let host = piece.split(['/', '?', '#', ':']).next().unwrap_or_default();
    let tld = host.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    host.contains('.') && (host.to_ascii_lowercase().starts_with("www.") || TLDS.contains(&tld.as_str()))
}

/// A link without the punctuation that closes the sentence around it: a final `.`, `,`, `:` or
/// `!`, and a `)` or `]` that opens nowhere in the link.
pub(super) fn trim_link(text: &str, mut range: Range<usize>) -> Range<usize> {
    loop {
        let piece = &text[range.clone()];
        let Some(last) = piece.chars().next_back() else { break };
        let unbalanced =
            |open: char, close: char| last == close && piece.matches(open).count() < piece.matches(close).count();
        if ".,;:!?".contains(last) || unbalanced('(', ')') || unbalanced('[', ']') || unbalanced('{', '}') {
            range.end -= last.len_utf8();
        } else {
            break;
        }
    }
    range
}

// -------------------------------------------------------------------------------- phones

/// A number as `tel:` takes it: `+` and digits only.
/// An international number drops the national trunk prefix written as "(0)": "+49 (0)30 1234567"
/// dials as `+49301234567`.
pub(super) fn dial_value(number: &str) -> String {
    if number.trim_start_matches('(').trim_start().starts_with('+') {
        let digits: String = number.replace("(0)", "").chars().filter(char::is_ascii_digit).collect();
        format!("+{digits}")
    } else {
        number.chars().filter(char::is_ascii_digit).collect()
    }
}

fn phones(text: &str) -> Vec<Candidate> {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Digits with at most two separators between them ("0 (212) 555 01 47", "+90 532-418").
    let re = compiled(&RE, || r"[+(]?\+?\d(?:[ \-.()\x{2011}]{0,2}\d)*".into());
    re.find_iter(text)
        .filter_map(|m| {
            let mut range = m.range();
            let piece = &text[range.clone()];
            if piece.starts_with('(') && piece.matches('(').count() > piece.matches(')').count() {
                range.start += 1;
            }
            let glued = |c: Option<char>, beyond: Option<char>| {
                word_char(c) || (matches!(c, Some('-' | '.' | '/' | '_')) && word_char(beyond))
            };
            let before = char_before(text, range.start);
            let after = char_after(text, range.end);
            let before2 = text[..range.start].chars().rev().nth(1);
            let after2 = text[range.end..].chars().nth(1);
            if glued(before, before2) || glued(after, after2) {
                return None;
            }
            let piece = &text[range.clone()];
            (dialable(piece) && !labelled_id(text, range.start)).then(|| Candidate {
                kind: EntityKind::Phone,
                range,
                value: dial_value(piece),
                date: None,
            })
        })
        .collect()
}

/// Whether a run of digits reads as a phone number: an international number of 8 to 15 digits,
/// a national one with a leading 0 and 9 to 12 digits, or one of the national shapes without a
/// leading 0 (North America, Spain, Russia's 8, Chinese mobiles). Prices, years, IBANs and card
/// numbers fit none of them.
fn dialable(piece: &str) -> bool {
    static SHAPES: OnceLock<Regex> = OnceLock::new();
    let shapes = compiled(&SHAPES, || {
        [
            r"\(?\d{3}\)?[ .\-]\d{3}[ .\-]\d{4}",
            // Spain writes its numbers with spaces; with dots this would be a price.
            r"[6789]\d{1,2} \d{2,3} \d{2,3}(?: \d{2})?",
            r"8[ \-]?\(?\d{3}\)?[ \-]?\d{3}[ \-]?\d{2}[ \-]?\d{2}",
            r"1[3-9]\d[ \-]?\d{4}[ \-]?\d{4}",
        ]
        .map(|shape| format!("^(?:{shape})$"))
        .join("|")
    });
    let digits = piece.chars().filter(char::is_ascii_digit).count();
    if piece.trim_start_matches('(').starts_with('+') {
        return (8..=15).contains(&digits);
    }
    if piece.trim_start_matches('(').starts_with('0') {
        return (9..=12).contains(&digits);
    }
    (9..=11).contains(&digits) && shapes.is_match(piece)
}

// -------------------------------------------------------------------------------- labels

/// The words right before `at` on its line, lowercased, nearest first; at most `n`.
fn words_before(text: &str, at: usize, n: usize) -> Vec<String> {
    let line = &text[text[..at].rfind('\n').map_or(0, |i| i + 1)..at];
    line.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).rev().take(n).map(str::to_lowercase).collect()
}

/// Words that name the number right after them as something other than a phone number: an ID,
/// a tax, invoice, order, account or tracking number.
const ID_LABELS: &[&str] = &[
    "no",
    "nr",
    "nº",
    "num",
    "number",
    "numara",
    "numarası",
    "numarasi",
    "nummer",
    "número",
    "numero",
    "номер",
    "id",
    "kimlik",
    "tc",
    "tckn",
    "vkn",
    "vergi",
    "tax",
    "vat",
    "iban",
    "imei",
    "ref",
    "referans",
    "reference",
    "fatura",
    "invoice",
    "sipariş",
    "siparis",
    "order",
    "pedido",
    "factura",
    "rechnung",
    "bestellung",
    "bestellnummer",
    "rechnungsnummer",
    "kundennummer",
    "auftrag",
    "заказ",
    "счет",
    "счёт",
    "инн",
    "takip",
    "tracking",
    "kargo",
    "poliçe",
    "police",
    "policy",
    "dosya",
    "ticket",
    "pnr",
    "seri",
    "serial",
    "kod",
    "kodu",
    "code",
    "hesap",
    "account",
    "müşteri",
    "customer",
    "订单",
    "订单号",
    "编号",
    "注文",
    "番号",
];

/// Words that name a phone number; among the words before a number they win over an ID label
/// ("Tel. No: 0532 418 27 64").
const PHONE_LABELS: &[&str] = &[
    "tel",
    "telefon",
    "telefonu",
    "telefono",
    "teléfono",
    "phone",
    "mobile",
    "mobil",
    "mob",
    "gsm",
    "cep",
    "fax",
    "faks",
    "handy",
    "tél",
    "téléphone",
    "тел",
    "телефон",
    "моб",
    "电话",
    "電話",
    "手机",
    "携帯",
];

/// Whether the number at `at` is labelled as an ID, an invoice or an order number ("TC Kimlik
/// 13579246802", "Fatura No 0012345678", "Order #2026112345") and not as a phone number. Only
/// the word right before the number counts as the label, so "Müşteri Hizmetleri 0850 222 00 00"
/// stays a phone number.
pub(super) fn labelled_id(text: &str, at: usize) -> bool {
    let words = words_before(text, at, 3);
    if words.iter().any(|w| PHONE_LABELS.contains(&w.as_str())) {
        return false;
    }
    let mark = text[..at].trim_end_matches([' ', '\t', ':', '：']).chars().next_back();
    matches!(mark, Some('#' | '№')) || words.first().is_some_and(|w| ID_LABELS.contains(&w.as_str()))
}

/// Words before a card's expiry ("SKT 12/27", "Valid thru 12/27", "MM/YY").
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const EXPIRY_LABELS: &[&str] = &[
    "exp",
    "expiry",
    "expires",
    "expiration",
    "valid",
    "thru",
    "skt",
    "kul",
    "kullanma",
    "gültig",
    "ablauf",
    "vence",
    "vencimiento",
    "caducidad",
    "válida",
    "valida",
    "срок",
    "mm",
    "yy",
    "aa",
    "有效期",
    "有効期限",
];

/// Whether `range` holds a card's expiry, a month and a two-digit year ("12/27"), rather than a
/// day: two digits, a slash and two digits after an expiry label, or on the line of a card
/// number or the line under it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn card_expiry(text: &str, range: Range<usize>) -> bool {
    static SHAPE: OnceLock<Regex> = OnceLock::new();
    static CARD: OnceLock<Regex> = OnceLock::new();
    let shape = compiled(&SHAPE, || r"^\d{2}[ \t]*/[ \t]*\d{2}$".into());
    if !shape.is_match(text[range.clone()].trim()) {
        return false;
    }
    if words_before(text, range.start, 3).iter().any(|w| EXPIRY_LABELS.contains(&w.as_str())) {
        return true;
    }
    let line_start = text[..range.start].rfind('\n').unwrap_or(0);
    let above = text[..line_start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[range.end..].find('\n').map_or(text.len(), |i| range.end + i);
    let card = compiled(&CARD, || r"\d(?:[ \-]?\d){12,18}".into());
    card.is_match(&text[above..end])
}

/// Whether a date NSDataDetector read is the day the text writes. The detector rolls an
/// impossible day over ("31.02.2026" becomes 3 March): a named month must stay the date's month,
/// and a day and a month in figures must be the date's, in an order the separator allows (a dot
/// writes the day first; a slash or a hyphen either way, or a year first). A text without either
/// ("tomorrow at 9.30") is taken as the detector read it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn fits_written(piece: &str, date: NaiveDate) -> bool {
    static FIGURES: OnceLock<Regex> = OnceLock::new();
    let named: Vec<u32> = piece.split(|c: char| !c.is_alphabetic()).filter_map(month_named).collect();
    if !named.is_empty() {
        return named.contains(&date.month());
    }
    let figures = compiled(&FIGURES, || r"(\d{1,4})([./\-])(\d{1,2})(?:[./\-](\d{1,4}))?".into());
    let real = |day: u32, month: u32| (1..=31).contains(&day) && (1..=12).contains(&month);
    let mut written = false;
    for caps in figures.captures_iter(piece) {
        let (Some(whole), Some(a), Some(sep), Some(b)) = (caps.get(0), caps.get(1), caps.get(2), caps.get(3)) else {
            continue;
        };
        if char_before(piece, whole.start()).is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let (Ok(x), Ok(y)) = (a.as_str().parse::<u32>(), b.as_str().parse::<u32>()) else { continue };
        let readings: Vec<(u32, u32)> = if a.len() == 4 {
            number(&caps, 4).map(|day| vec![(day, y)]).unwrap_or_default()
        } else if sep.as_str() == "." {
            vec![(x, y)]
        } else {
            vec![(x, y), (y, x)]
        };
        let readings: Vec<(u32, u32)> = readings.into_iter().filter(|&(d, m)| real(d, m)).collect();
        if readings.is_empty() {
            continue;
        }
        written = true;
        if readings.contains(&(date.day(), date.month())) {
            return true;
        }
    }
    !written
}

// --------------------------------------------------------------------------------- dates

/// The months by name and common abbreviation, lower case. English "may" also needs a capital,
/// since the verb is far more common in a sentence.
const MONTHS: &[(&str, u32)] = &[
    // English
    ("january", 1),
    ("february", 2),
    ("march", 3),
    ("april", 4),
    ("may", 5),
    ("june", 6),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("october", 10),
    ("november", 11),
    ("december", 12),
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("sept", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
    // Turkish
    ("ocak", 1),
    ("şubat", 2),
    ("mart", 3),
    ("nisan", 4),
    ("mayıs", 5),
    ("haziran", 6),
    ("temmuz", 7),
    ("ağustos", 8),
    ("eylül", 9),
    ("ekim", 10),
    ("kasım", 11),
    ("aralık", 12),
    // German
    ("januar", 1),
    ("jänner", 1),
    ("februar", 2),
    ("märz", 3),
    ("mai", 5),
    ("juni", 6),
    ("juli", 7),
    ("oktober", 10),
    ("dezember", 12),
    ("mär", 3),
    ("okt", 10),
    ("dez", 12),
    // Spanish
    ("enero", 1),
    ("febrero", 2),
    ("marzo", 3),
    ("abril", 4),
    ("mayo", 5),
    ("junio", 6),
    ("julio", 7),
    ("agosto", 8),
    ("septiembre", 9),
    ("setiembre", 9),
    ("octubre", 10),
    ("noviembre", 11),
    ("diciembre", 12),
    ("ene", 1),
    ("abr", 4),
    ("dic", 12),
    // Russian, in the genitive a date uses
    ("января", 1),
    ("февраля", 2),
    ("марта", 3),
    ("апреля", 4),
    ("мая", 5),
    ("июня", 6),
    ("июля", 7),
    ("августа", 8),
    ("сентября", 9),
    ("октября", 10),
    ("ноября", 11),
    ("декабря", 12),
    ("янв", 1),
    ("фев", 2),
    ("мар", 3),
    ("апр", 4),
    ("июн", 6),
    ("июл", 7),
    ("авг", 8),
    ("сен", 9),
    ("сент", 9),
    ("окт", 10),
    ("ноя", 11),
    ("дек", 12),
];

/// The weekdays, lower case, which a date may carry before or after it.
const WEEKDAYS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "mon",
    "tue",
    "tues",
    "wed",
    "thu",
    "thur",
    "thurs",
    "fri",
    "sat",
    "sun",
    "pazartesi",
    "salı",
    "çarşamba",
    "perşembe",
    "cuma",
    "cumartesi",
    "pazar",
    "montag",
    "dienstag",
    "mittwoch",
    "donnerstag",
    "freitag",
    "samstag",
    "sonnabend",
    "sonntag",
    "mo",
    "di",
    "mi",
    "do",
    "fr",
    "sa",
    "so",
    "lunes",
    "martes",
    "miércoles",
    "jueves",
    "viernes",
    "sábado",
    "domingo",
    "lun",
    "mié",
    "jue",
    "vie",
    "sáb",
    "dom",
    "понедельник",
    "вторник",
    "среда",
    "среду",
    "четверг",
    "пятница",
    "пятницу",
    "суббота",
    "субботу",
    "воскресенье",
    "пн",
    "вт",
    "ср",
    "чт",
    "пт",
    "сб",
    "вс",
];

fn month_named(word: &str) -> Option<u32> {
    let lower = word.trim_end_matches('.').to_lowercase();
    if lower == "may" && !word.starts_with('M') {
        return None;
    }
    MONTHS.iter().find(|(name, _)| *name == lower).map(|&(_, month)| month)
}

/// Whether `word` names a weekday. A two-letter abbreviation counts only with its dot or a comma
/// after it (`dotted`), since "do" and "so" are words too.
fn weekday_named(word: &str, dotted: bool) -> bool {
    let lower = word.to_lowercase();
    (dotted || lower.chars().count() > 2) && WEEKDAYS.contains(&lower.as_str())
}

/// A date the text names, before its weekday and time are read.
struct Core {
    range: Range<usize>,
    date: NaiveDate,
    time: Option<(NaiveTime, Option<NaiveTime>)>,
}

fn dates(text: &str, now: NaiveDateTime) -> Vec<Candidate> {
    let mut cores = cores(text, now);
    // Where two rules read the same place, the longer reading wins.
    cores.sort_by_key(|c| (std::cmp::Reverse(c.range.len()), c.range.start));
    let mut kept: Vec<Core> = Vec::new();
    for core in cores {
        if !kept.iter().any(|k| k.range.start < core.range.end && core.range.start < k.range.end) {
            kept.push(core);
        }
    }
    kept.sort_by_key(|c| c.range.start);

    let mut out: Vec<Candidate> = Vec::new();
    for core in kept {
        // A weekday that the date before already took is not this one's ("27 Kasım Cuma, 28 Kasım").
        let taken = out.last().map_or(0, |c| c.range.end);
        let start = Some(weekday_before(text, core.range.start)).filter(|&s| s >= taken).unwrap_or(core.range.start);
        let mut end = weekday_after(text, core.range.end).unwrap_or(core.range.end);
        let mut time = core.time;
        if time.is_none() {
            if let Some((until, read)) = time_after(text, end) {
                end = until;
                time = Some(read);
            }
        }
        let date = Some(span(core.date, time));
        let candidate =
            Candidate { kind: EntityKind::Date, range: start..end, value: text[start..end].to_string(), date };
        // A date whose weekday or time runs into another keeps its own words rather than going.
        let bare = || Candidate {
            kind: EntityKind::Date,
            range: core.range.clone(),
            value: text[core.range.clone()].to_string(),
            date: Some(span(core.date, core.time)),
        };
        if !out.iter().any(|c| c.overlaps(&candidate)) {
            out.push(candidate);
        } else if !out.iter().any(|c| c.overlaps(&bare())) {
            out.push(bare());
        }
    }
    out
}

/// When an event on `date` at `time` starts and ends: all day without a time, an hour without an
/// end, and an end at or before the start is on the next day.
fn span(date: NaiveDate, time: Option<(NaiveTime, Option<NaiveTime>)>) -> EntityDate {
    match time {
        None => EntityDate { start: date.and_time(NaiveTime::MIN), end: None, all_day: true, end_assumed: false },
        Some((from, until)) => {
            let start = date.and_time(from);
            let end = until.map(|t| {
                let end = date.and_time(t);
                if end <= start {
                    end + Duration::days(1)
                } else {
                    end
                }
            });
            EntityDate {
                start,
                end: Some(end.unwrap_or(start + Duration::hours(1))),
                all_day: false,
                end_assumed: end.is_none(),
            }
        }
    }
}

/// The day of `month` and `day` in the year that puts it closest to `now`.
fn closest(month: u32, day: u32, now: NaiveDateTime) -> Option<NaiveDate> {
    let today = now.date();
    [today.year() - 1, today.year(), today.year() + 1]
        .into_iter()
        .filter_map(|year| NaiveDate::from_ymd_opt(year, month, day))
        .min_by_key(|d| (*d - today).num_days().abs())
}

fn day_of(year: Option<&str>, month: u32, day: u32, now: NaiveDateTime) -> Option<NaiveDate> {
    match year {
        Some(y) => {
            let y: i32 = y.parse().ok()?;
            NaiveDate::from_ymd_opt(if y < 100 { 2000 + y } else { y }, month, day)
        }
        None => closest(month, day, now),
    }
}

fn number(caps: &Captures<'_>, i: usize) -> Option<u32> {
    caps.get(i)?.as_str().parse().ok()
}

/// The dates the text names, read by every rule; they may overlap.
fn cores(text: &str, now: NaiveDateTime) -> Vec<Core> {
    static ISO: OnceLock<Regex> = OnceLock::new();
    static DOTTED: OnceLock<Regex> = OnceLock::new();
    static CJK: OnceLock<Regex> = OnceLock::new();
    static DAY_MONTH: OnceLock<Regex> = OnceLock::new();
    static MONTH_DAY: OnceLock<Regex> = OnceLock::new();
    let iso = compiled(&ISO, || r"(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2})(?::\d{2})?)?".into());
    let dotted = compiled(&DOTTED, || r"(\d{1,2})\.(\d{1,2})\.(\d{4}|\d{2})".into());
    // `[ \t]` and not `\s` everywhere: the lines are joined with line breaks, and no date runs
    // over one ("Saat 14:02" above "Mart ayında" is no 2 March).
    let cjk = compiled(&CJK, || r"(?:(\d{4})[ \t]*年[ \t]*)?(\d{1,2})[ \t]*月[ \t]*(\d{1,2})[ \t]*[日号號]".into());
    let day_month = compiled(&DAY_MONTH, || {
        format!(
            r"(\d{{1,2}})\.?(?:[ \t]+de)?[ \t]+({LETTER}+\.?)(?:,?[ \t]+(?:de[ \t]+)?(\d{{4}})(?:[ \t]*(?:года|г\.))?)?"
        )
    });
    let month_day =
        compiled(&MONTH_DAY, || format!(r"({LETTER}+\.?)[ \t]+(\d{{1,2}})(?:st|nd|rd|th)?(?:,?[ \t]+(\d{{4}}))?"));

    // A piece is a date only where no digit or letter touches it, and a Latin or Cyrillic one also
    // needs a space or punctuation before it ("A1.12.2026" is a code). A day glued to a clock time
    // is the time's minutes or hour ("14:02 Mart", "Mart 14:02").
    let alone = |range: &Range<usize>| {
        let after = char_after(text, range.end);
        let digit_after = after.is_some_and(|c| c.is_ascii_digit())
            || (matches!(after, Some('.' | ':')) && text[range.end + 1..].starts_with(|c: char| c.is_ascii_digit()));
        let before = char_before(text, range.start);
        let clock_before =
            before == Some(':') && char_before(text, range.start - 1).is_some_and(|c| c.is_ascii_digit());
        !word_char(before) && !clock_before && !digit_after && !after.is_some_and(char::is_alphabetic)
    };

    let mut out = Vec::new();
    for caps in iso.captures_iter(text) {
        let whole = caps.get(0).map(|m| m.range()).unwrap_or_default();
        let (Some(y), Some(m), Some(d)) = (number(&caps, 1), number(&caps, 2), number(&caps, 3)) else { continue };
        let Some(date) = NaiveDate::from_ymd_opt(y as i32, m, d) else { continue };
        let time = match (number(&caps, 4), number(&caps, 5)) {
            (Some(h), Some(min)) => NaiveTime::from_hms_opt(h, min, 0).map(|t| (t, None)),
            _ => None,
        };
        if alone(&whole) {
            out.push(Core { range: whole, date, time });
        }
    }
    for caps in dotted.captures_iter(text) {
        let whole = caps.get(0).map(|m| m.range()).unwrap_or_default();
        let (Some(d), Some(m), Some(year)) = (caps.get(1), caps.get(2), caps.get(3)) else { continue };
        // "1.2.10" is a version number; a two-digit year needs a two-digit day and month.
        if year.len() == 2 && (d.len() != 2 || m.len() != 2) {
            continue;
        }
        let (Ok(d), Ok(m)) = (d.as_str().parse(), m.as_str().parse()) else { continue };
        let Some(date) = day_of(Some(year.as_str()), m, d, now) else { continue };
        if alone(&whole) && char_before(text, whole.start) != Some('.') {
            out.push(Core { range: whole, date, time: None });
        }
    }
    for caps in cjk.captures_iter(text) {
        let whole = caps.get(0).map(|m| m.range()).unwrap_or_default();
        let (Some(m), Some(d)) = (number(&caps, 2), number(&caps, 3)) else { continue };
        let Some(date) = day_of(caps.get(1).map(|y| y.as_str()), m, d, now) else { continue };
        if !char_before(text, whole.start).is_some_and(|c| c.is_ascii_digit()) {
            out.push(Core { range: whole, date, time: None });
        }
    }
    for caps in day_month.captures_iter(text) {
        let whole = caps.get(0).map(|m| m.range()).unwrap_or_default();
        let (Some(d), Some(word)) = (number(&caps, 1), caps.get(2)) else { continue };
        let Some(month) = month_named(word.as_str()) else { continue };
        let Some(date) = day_of(caps.get(3).map(|y| y.as_str()), month, d, now) else { continue };
        if alone(&whole) {
            out.push(Core { range: whole, date, time: None });
        }
    }
    for caps in month_day.captures_iter(text) {
        let whole = caps.get(0).map(|m| m.range()).unwrap_or_default();
        let (Some(word), Some(d)) = (caps.get(1), number(&caps, 2)) else { continue };
        let Some(month) = month_named(word.as_str()) else { continue };
        let Some(date) = day_of(caps.get(3).map(|y| y.as_str()), month, d, now) else { continue };
        if alone(&whole) {
            out.push(Core { range: whole, date, time: None });
        }
    }
    out
}

/// Where a weekday right before `at` starts ("Pazartesi 30 Kasım", "Mo., 30. November"), or `at`.
fn weekday_before(text: &str, at: usize) -> usize {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = compiled(&RE, || format!(r"({LETTER}+)(\.)?(,)?[ \t]+$"));
    let head = &text[..at];
    let Some(caps) = re.captures(head) else { return at };
    let (Some(whole), Some(word)) = (caps.get(0), caps.get(1)) else { return at };
    let dotted = caps.get(2).is_some() || caps.get(3).is_some();
    if weekday_named(word.as_str(), dotted) && !word_char(char_before(text, whole.start())) {
        whole.start()
    } else {
        at
    }
}

/// Where a weekday right after `at` ends ("27 Kasım Cuma", "3 Aralık 2026 Perşembe", "11月27日
/// (金)", "11月27日 星期五"), or `None`.
fn weekday_after(text: &str, at: usize) -> Option<usize> {
    static WORD: OnceLock<Regex> = OnceLock::new();
    static CJK: OnceLock<Regex> = OnceLock::new();
    let word = compiled(&WORD, || format!(r"^,?[ \t]+({LETTER}+)(\.|,)?"));
    let cjk = compiled(&CJK, || {
        r"^[ \t]*(?:[（(](?:星期|周|週)?[一二三四五六日天月火水木金土](?:曜日?)?[）)]|(?:星期|周|週)[一二三四五六日天]|[月火水木金土日]曜日?)"
            .into()
    });
    let tail = &text[at..];
    if let Some(m) = cjk.find(tail) {
        return Some(at + m.end());
    }
    let caps = word.captures(tail)?;
    let name = caps.get(1)?;
    let mut end = name.end();
    let dotted = caps.get(2).is_some();
    if caps.get(2).is_some_and(|p| p.as_str() == ".") {
        end += 1;
    }
    let next = char_after(tail, end);
    (weekday_named(name.as_str(), dotted) && !word_char(next)).then_some(at + end)
}

/// A time, or a time range, right after `at` ("14:30", ", 09:00–12:00", "um 19 Uhr", "3:30 pm",
/// "下午3点"), with where it ends.
fn time_after(text: &str, at: usize) -> Option<(usize, (NaiveTime, Option<NaiveTime>))> {
    static LEAD: OnceLock<Regex> = OnceLock::new();
    static CLOCK: OnceLock<Regex> = OnceLock::new();
    static CJK_CLOCK: OnceLock<Regex> = OnceLock::new();
    static RANGE: OnceLock<Regex> = OnceLock::new();
    // A time is read on the date's own line only.
    let lead = compiled(&LEAD, || {
        r"^(?:[ \t]*[,·|/]|[ \t]*[-–—][ \t])?[ \t]*(?:(?:saat|at|um|ab|a las|a la|à|в|kl\.?)[ \t]+)?".into()
    });
    let clock = compiled(&CLOCK, || r"^(\d{1,2})(?:[:.](\d{2}))?(?:[ \t]*(a\.m\.|p\.m\.|am|pm|AM|PM|Uhr))?".into());
    let cjk_clock = compiled(&CJK_CLOCK, || {
        r"^(上午|下午|午前|午後|晚上|早上)?[ \t]*(\d{1,2})[ \t]*(?:[:：][ \t]*(\d{2})|[時时点點](?:[ \t]*(\d{1,2})[ \t]*分|(半))?)"
            .into()
    });
    let range = compiled(&RANGE, || r"^[ \t]*(?:[-–—~〜～至]|to|bis|до)[ \t]*".into());

    // One clock reading at `from`: where it ends and the time. The Latin reading goes first, so
    // "3:30 pm" keeps its "pm"; "15時" and "下午3点" fall through to the CJK one.
    let latin = |from: usize| -> Option<(usize, NaiveTime)> {
        let caps = clock.captures(&text[from..])?;
        let mut hour = number(&caps, 1)?;
        let minute = number(&caps, 2);
        let mark = caps.get(3).map(|m| m.as_str().to_ascii_lowercase());
        // A bare number is not a time: "27 Kasım 4 kişi".
        if minute.is_none() && mark.is_none() {
            return None;
        }
        match mark.as_deref() {
            Some("pm" | "p.m.") if hour < 12 => hour += 12,
            Some("am" | "a.m.") if hour == 12 => hour = 0,
            _ => {}
        }
        let end = from + caps.get(0)?.end();
        if text[end..].starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        NaiveTime::from_hms_opt(hour, minute.unwrap_or(0), 0).map(|t| (end, t))
    };
    let eastern = |from: usize| -> Option<(usize, NaiveTime)> {
        let caps = cjk_clock.captures(&text[from..])?;
        let mut hour = number(&caps, 2)?;
        let half = if caps.get(5).is_some() { 30 } else { 0 };
        let minute = number(&caps, 3).or_else(|| number(&caps, 4)).unwrap_or(half);
        if matches!(caps.get(1).map(|m| m.as_str()), Some("下午" | "午後" | "晚上")) && hour < 12 {
            hour += 12;
        }
        let end = from + caps.get(0)?.end();
        NaiveTime::from_hms_opt(hour, minute, 0).map(|t| (end, t))
    };
    let read = |from: usize| latin(from).or_else(|| eastern(from));

    let from = at + lead.find(&text[at..])?.end();
    let (end, start) = read(from)?;
    if let Some(dash) = range.find(&text[end..]) {
        if let Some((until, finish)) = read(end + dash.end()) {
            return Some((until, (start, Some(finish))));
        }
    }
    Some((end, (start, None)))
}

/// Whether a date NSDataDetector found names a clock time ("14:30", "3 pm", "19 Uhr", "15時"),
/// which decides between a timed and an all-day event. "27.11" is a day and a month, not a time.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn has_time(piece: &str) -> bool {
    static CLOCK: OnceLock<Regex> = OnceLock::new();
    static DOT: OnceLock<Regex> = OnceLock::new();
    let clock = compiled(&CLOCK, || r"(?i)\d{1,2}\s*(?:[:：]\s*\d{2}|a\.m\.|p\.m\.|am\b|pm\b|uhr\b|[時时点點])".into());
    let dot = compiled(&DOT, || r"\d{1,2}\.\d{2}".into());
    let near = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit() || c == '.');
    clock.is_match(piece)
        || dot.find_iter(piece).any(|m| !near(char_before(piece, m.start())) && !near(char_after(piece, m.end())))
}

/// Whether a date NSDataDetector found names a day and not only a clock time: "14:02" in a mail's
/// header or "9:00〜12:00" would otherwise become today's date.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn names_day(piece: &str) -> bool {
    static CLOCK: OnceLock<Regex> = OnceLock::new();
    let clock = compiled(&CLOCK, || {
        r"(?i)\d{1,2}\s*(?:[:：.]\s*\d{2}(?:\s*(?:a\.m\.|p\.m\.|am|pm|uhr))?|a\.m\.|p\.m\.|am|pm|uhr)".into()
    });
    clock.replace_all(piece, "").chars().any(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 23).and_then(|d| d.and_hms_opt(12, 0, 0)).expect("a valid time")
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("a valid day")
    }

    /// A date goes to the user's calendar as it was read, so the rules read every date as the text
    /// writes it and within its own line, or not at all.
    #[test]
    fn a_date_is_read_as_written_and_within_its_line() {
        let cases: &[(&str, &[(&str, &str)])] = &[
            (
                "27 Kasım Cuma\n28 Kasım Cumartesi\n29 Kasım Pazar",
                &[
                    ("27 Kasım Cuma", "2026-11-27 00:00"),
                    ("28 Kasım Cumartesi", "2026-11-28 00:00"),
                    ("29 Kasım Pazar", "2026-11-29 00:00"),
                ],
            ),
            (
                "27 Kasım Cuma, 28 Kasım Cumartesi",
                &[("27 Kasım Cuma", "2026-11-27 00:00"), ("28 Kasım Cumartesi", "2026-11-28 00:00")],
            ),
            ("Giriş: 27 Kasım 2026 Cuma, saat 14:30", &[("27 Kasım 2026 Cuma, saat 14:30", "2026-11-27 14:30")]),
            ("Toplantı 30 Kasım\n14:00 salonda", &[("30 Kasım", "2026-11-30 00:00")]),
            ("Saat 14:02\nMart ayında", &[]),
            ("Saat 14:02 Mart ayında", &[]),
            ("31.02.2026", &[]),
            ("31 Şubat 2026", &[]),
        ];
        for (text, expected) in cases {
            let read: Vec<(String, String)> = find(text, now())
                .into_iter()
                .filter_map(|c| Some((text[c.range].to_string(), c.date?.start.format("%Y-%m-%d %H:%M").to_string())))
                .collect();
            let expected: Vec<(String, String)> =
                expected.iter().map(|(p, d)| (p.to_string(), d.to_string())).collect();
            assert_eq!(read, expected, "{text:?}");
        }
    }

    /// NSDataDetector's dates are checked against the text before they reach the calendar.
    #[test]
    fn a_detected_date_must_be_the_written_one() {
        assert!(!fits_written("31.02.2026", day(2026, 3, 3)));
        assert!(!fits_written("February 31", day(2026, 3, 3)));
        assert!(!fits_written("31 Kasım", day(2026, 12, 1)));
        assert!(fits_written("15/01/2027", day(2027, 1, 15)));
        assert!(fits_written("1/15", day(2027, 1, 15)));
        assert!(fits_written("2026-11-27", day(2026, 11, 27)));
        assert!(fits_written("Nov 27 at 3pm", day(2026, 11, 27)));
        assert!(fits_written("tomorrow at 9.30", day(2026, 11, 24)));
        for (text, piece) in [("Kart 4111 1111 1111 1111\nSKT 12/27", "12/27"), ("Valid thru 12/27", "12/27")] {
            let at = text.find(piece).expect("the piece");
            assert!(card_expiry(text, at..at + piece.len()), "{text:?}");
        }
        let text = "Party on 12/27 at 8 pm";
        let at = text.find("12/27").expect("the piece");
        assert!(!card_expiry(text, at..at + 5));
    }
}
