//! The kinds smart redaction (D28) offers to hide: payment card numbers, IBANs, API keys, access
//! tokens and the password inside a link. Each kind is checked beyond its shape where a check
//! exists, so an order number, a commit hash, a UUID or a plain word is not offered.
//!
//! - A card has 13 to 19 digits, an issuer's prefix (Visa, Mastercard, Amex, Discover, JCB,
//!   Diners, UnionPay, Mir, Troy, Maestro) and a valid Luhn digit.
//! - An IBAN has its country's length and passes the mod-97 check.
//! - A key or a token starts with a prefix its service gives it (`sk_live_`, `ghp_`, `AKIA` and
//!   the like, or a JWT's `eyJ`), or is the value of an assignment whose name holds KEY, SECRET,
//!   TOKEN or PASS and looks random: 16 characters or more, two classes of characters and at
//!   least 3.5 bits of entropy per character.
//! - A password is only the part of a link between the user's colon and the last `@` before the
//!   host (`postgres://app:…@db`, `redis://:…@cache`).
//!
//! A card's or an IBAN's digits are no phone number, and the `secret@host` of a link is no email
//! address; [`super::find_entities`] drops the phone numbers and addresses these overlap.

use std::ops::Range;
use std::sync::OnceLock;

use regex_lite::Regex;

use super::{Candidate, EntityKind};

/// Every sensitive piece in `text`, without overlaps, sorted by position.
pub(super) fn find(text: &str) -> Vec<Candidate> {
    let mut found: Vec<Candidate> = Vec::new();
    // A prefixed key comes before the assignment that holds it, so it keeps its own kind.
    let all =
        passwords(text).into_iter().chain(prefixed(text)).chain(ibans(text)).chain(cards(text)).chain(assigned(text));
    for candidate in all {
        if !found.iter().any(|c| c.overlaps(&candidate)) {
            found.push(candidate);
        }
    }
    found.sort_by_key(|c| c.range.start);
    found
}

/// Whether a found piece of another kind is one of these in disguise: a phone number on a card's
/// or an IBAN's digits, or an address on a link's password. A phone number read in digits grouped
/// as a card is (four by four, 13 to 19 of them) is dropped even when they failed the card's
/// check, because no phone number is written that way.
pub(super) fn hides(text: &str, sensitive: &[Candidate], other: &Candidate) -> bool {
    let masked = sensitive.iter().any(|s| {
        let masks = match other.kind {
            EntityKind::Phone => matches!(s.kind, EntityKind::Card | EntityKind::Iban),
            EntityKind::Email => s.kind == EntityKind::UrlPassword,
            _ => false,
        };
        masks && s.overlaps(other)
    });
    masked || (other.kind == EntityKind::Phone && on_card_shape(text, &other.range))
}

/// Whether `range` overlaps a run of digits grouped as a card is printed.
fn on_card_shape(text: &str, range: &Range<usize>) -> bool {
    digit_runs().find_iter(text).filter(|m| m.start() < range.end && range.start < m.end()).any(|m| {
        let groups = digit_groups(text, m.range());
        let count: usize = groups.iter().map(|g| g.digits).sum();
        let (last, rest) = groups.split_last().expect("a run holds a group");
        (13..=19).contains(&count) && rest.len() >= 3 && rest.iter().all(|g| g.digits == 4) && last.digits <= 4
    })
}

fn compiled(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("the check's pattern compiles"))
}

/// What text recognition reads instead of a digit or an `x` in the monospaced fonts of editors
/// and terminals: the slashed zero comes back as `Ø` or `ø`, and an `x` between digits as `×`.
/// A key read that way is still the key, and a card or an IBAN still counts its zero.
const MISREAD_ZERO: &str = "Øø";
const MISREAD_X: char = '×';

fn digit(c: char) -> Option<u8> {
    match c {
        '0'..='9' => Some(c as u8 - b'0'),
        c if MISREAD_ZERO.contains(c) => Some(0),
        _ => None,
    }
}

/// Whether the piece at `start..end` stands apart from the text around it: no letter, digit or
/// underscore touches it.
fn apart(text: &str, start: usize, end: usize) -> bool {
    let glued = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    !glued(text[..start].chars().next_back()) && !glued(text[end..].chars().next())
}

// --------------------------------------------------------------------------------- cards

/// Runs of digit groups split by spaces or dashes. Text recognition reads the single space of a
/// block it lays out as code as two, so a run of spaces is one separator.
fn digit_runs() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    compiled(&RE, r"[0-9Øø]+(?:(?: +|-)[0-9Øø]+)*")
}

fn cards(text: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    for m in digit_runs().find_iter(text) {
        let groups = digit_groups(text, m.range());
        // An expiry, a security code or another number may follow a card in the same run
        // ("5555 5555 5555 4444 12/27", "4242424242424242 123"), so from each group on, every
        // span of whole groups a card could fill is tried, the longest first.
        let mut i = 0;
        while i < groups.len() {
            let mut count = 0;
            let mut ends = Vec::new();
            for (j, group) in groups.iter().enumerate().skip(i) {
                count += group.digits;
                if count > 19 {
                    break;
                }
                if count >= 13 {
                    ends.push(j);
                }
            }
            match ends.iter().rev().find_map(|&j| card(text, &groups[i..=j]).map(|c| (c, j))) {
                Some((candidate, j)) => {
                    out.push(candidate);
                    i = j + 1;
                }
                None => i += 1,
            }
        }
    }
    out
}

/// A group of digits in a run, by its byte range in the text.
struct Group {
    range: Range<usize>,
    digits: usize,
}

fn digit_groups(text: &str, run: Range<usize>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut open = false;
    for (i, c) in text[run.clone()].char_indices() {
        let at = run.start + i;
        match (digit(c).is_some(), open) {
            (true, true) => {
                let last = groups.last_mut().expect("an open group");
                last.range.end = at + c.len_utf8();
                last.digits += 1;
            }
            (true, false) => groups.push(Group { range: at..at + c.len_utf8(), digits: 1 }),
            (false, _) => {}
        }
        open = digit(c).is_some();
    }
    groups
}

/// The card these groups spell, if they spell one.
fn card(text: &str, groups: &[Group]) -> Option<Candidate> {
    let range = groups.first()?.range.start..groups.last()?.range.end;
    // Groups of one or two digits are a date's or a code's, never a card's.
    if groups.len() > 1 && groups.iter().any(|g| g.digits < 3) {
        return None;
    }
    // One kind of separator throughout: "4111 1111-1111 1111" is two numbers.
    let dashes: Vec<bool> = groups.windows(2).map(|w| text[w[0].range.end..w[1].range.start].contains('-')).collect();
    if dashes.windows(2).any(|w| w[0] != w[1]) || !apart(text, range.start, range.end) {
        return None;
    }
    let digits: Vec<u8> = text[range.clone()].chars().filter_map(digit).collect();
    (issued(&digits) && luhn(&digits)).then(|| Candidate {
        kind: EntityKind::Card,
        range,
        value: digits.iter().map(|d| char::from(b'0' + d)).collect(),
        date: None,
    })
}

/// Whether the number starts with a prefix a card network issues, at a length it issues.
fn issued(d: &[u8]) -> bool {
    let n = d.len();
    let prefix = |k: usize| d.iter().take(k).fold(0u32, |acc, &x| acc * 10 + u32::from(x));
    let (p2, p3, p4) = (prefix(2), prefix(3), prefix(4));
    let visa = d[0] == 4 && matches!(n, 13 | 16 | 19);
    let mastercard = ((51..=55).contains(&p2) || (2221..=2720).contains(&p4)) && n == 16;
    let amex = matches!(p2, 34 | 37) && n == 15;
    let discover = (p4 == 6011 || (644..=649).contains(&p3) || p2 == 65) && (16..=19).contains(&n);
    let jcb = (3528..=3589).contains(&p4) && (16..=19).contains(&n);
    let diners = ((300..=305).contains(&p3) || matches!(p2, 36 | 38 | 39)) && (14..=19).contains(&n);
    let unionpay = p2 == 62 && (16..=19).contains(&n);
    let mir = (2200..=2204).contains(&p4) && (16..=19).contains(&n);
    let troy = p4 == 9792 && n == 16;
    let maestro = (p2 == 50 || (56..=58).contains(&p2) || matches!(p2, 63 | 67)) && (13..=19).contains(&n);
    visa || mastercard || amex || discover || jcb || diners || unionpay || mir || troy || maestro
}

fn luhn(d: &[u8]) -> bool {
    let sum: u32 = d
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &x)| {
            let x = u32::from(x);
            if i % 2 == 1 {
                let doubled = x * 2;
                if doubled > 9 {
                    doubled - 9
                } else {
                    doubled
                }
            } else {
                x
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

// --------------------------------------------------------------------------------- IBANs

/// The length of an IBAN in each country of the SWIFT registry.
const IBAN_LENGTHS: &[(&str, usize)] = &[
    ("AD", 24),
    ("AE", 23),
    ("AL", 28),
    ("AT", 20),
    ("AZ", 28),
    ("BA", 20),
    ("BE", 16),
    ("BG", 22),
    ("BH", 22),
    ("BI", 27),
    ("BR", 29),
    ("BY", 28),
    ("CH", 21),
    ("CR", 22),
    ("CY", 28),
    ("CZ", 24),
    ("DE", 22),
    ("DJ", 27),
    ("DK", 18),
    ("DO", 28),
    ("EE", 20),
    ("EG", 29),
    ("ES", 24),
    ("FI", 18),
    ("FK", 18),
    ("FO", 18),
    ("FR", 27),
    ("GB", 22),
    ("GE", 22),
    ("GI", 23),
    ("GL", 18),
    ("GR", 27),
    ("GT", 28),
    ("HR", 21),
    ("HU", 28),
    ("IE", 22),
    ("IL", 23),
    ("IQ", 23),
    ("IS", 26),
    ("IT", 27),
    ("JO", 30),
    ("KW", 30),
    ("KZ", 20),
    ("LB", 28),
    ("LC", 32),
    ("LI", 21),
    ("LT", 20),
    ("LU", 20),
    ("LV", 21),
    ("LY", 25),
    ("MC", 27),
    ("MD", 24),
    ("ME", 22),
    ("MK", 19),
    ("MN", 20),
    ("MR", 27),
    ("MT", 31),
    ("MU", 30),
    ("NI", 28),
    ("NL", 18),
    ("NO", 15),
    ("OM", 23),
    ("PK", 24),
    ("PL", 28),
    ("PS", 29),
    ("PT", 25),
    ("QA", 29),
    ("RO", 24),
    ("RS", 22),
    ("RU", 33),
    ("SA", 24),
    ("SC", 31),
    ("SD", 18),
    ("SE", 24),
    ("SI", 19),
    ("SK", 24),
    ("SM", 27),
    ("SO", 23),
    ("ST", 25),
    ("SV", 28),
    ("TL", 23),
    ("TN", 24),
    ("TR", 26),
    ("UA", 29),
    ("VA", 22),
    ("VG", 24),
    ("XK", 20),
    ("YE", 30),
];

fn ibans(text: &str) -> Vec<Candidate> {
    static RE: OnceLock<Regex> = OnceLock::new();
    // A country, two check digits, then letters and digits in groups split by spaces (one, or the
    // two text recognition reads in a block it lays out as code).
    let re = compiled(&RE, r"[A-Z]{2}[0-9Øø]{2}(?: *[A-Z0-9Øø]){11,30}");
    re.find_iter(text)
        .filter_map(|m| {
            let country = &m.as_str()[..2];
            let length = IBAN_LENGTHS.iter().find(|(c, _)| *c == country)?.1;
            // The match may run on into the next group of capitals; the country's length ends it.
            let mut compact = String::with_capacity(length);
            let mut end = m.start();
            for (i, c) in m.as_str().char_indices() {
                if compact.len() == length {
                    break;
                }
                if c != ' ' {
                    compact.push(if digit(c) == Some(0) { '0' } else { c });
                    end = m.start() + i + c.len_utf8();
                }
            }
            (compact.len() == length && apart(text, m.start(), end) && mod97(&compact)).then(|| Candidate {
                kind: EntityKind::Iban,
                range: m.start()..end,
                value: compact,
                date: None,
            })
        })
        .collect()
}

/// The ISO 7064 check: the first four characters moved to the end, letters as 10 to 35, and the
/// whole number taken modulo 97 leaves 1.
fn mod97(iban: &str) -> bool {
    let (head, rest) = iban.split_at(4);
    let mut remainder = 0u32;
    for c in rest.chars().chain(head.chars()) {
        let value = c.to_digit(36).unwrap_or(0);
        remainder = if value < 10 { (remainder * 10 + value) % 97 } else { (remainder * 100 + value) % 97 };
    }
    remainder == 1
}

// ---------------------------------------------------------------------- keys and tokens

/// The prefixes services put on their keys and tokens, with the length that follows.
const PREFIXED: &[(&str, EntityKind)] = &[
    // Stripe's secret and restricted keys.
    (r"(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,}", EntityKind::ApiKey),
    // GitHub's personal, OAuth, user, server and refresh tokens, and its fine-grained tokens.
    (r"gh[pousr]_[A-Za-z0-9]{20,}", EntityKind::Token),
    (r"github_pat_[A-Za-z0-9_]{22,}", EntityKind::Token),
    // GitLab's personal access tokens.
    (r"glpat-[A-Za-z0-9_\-]{20,}", EntityKind::Token),
    // Slack's bot, user and app tokens.
    (r"xox[abprs]-[A-Za-z0-9\-]{10,}", EntityKind::Token),
    // AWS access key IDs, long-lived and temporary.
    (r"(?:AKIA|ASIA)[A-Z0-9]{16}", EntityKind::ApiKey),
    // Google Cloud and Firebase API keys.
    (r"AIza[A-Za-z0-9_\-]{35}", EntityKind::ApiKey),
    // Anthropic API keys.
    (r"sk-ant-[A-Za-z0-9_\-]{20,}", EntityKind::ApiKey),
    // A JSON Web Token: a header and a payload that start as `{"`, and a signature.
    (r"eyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}", EntityKind::Token),
];

fn prefixed(text: &str) -> Vec<Candidate> {
    static RES: OnceLock<Vec<(Regex, EntityKind)>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        PREFIXED
            .iter()
            .map(|(p, kind)| {
                let pattern = p.replace("0-9", &format!("0-9{MISREAD_ZERO}{MISREAD_X}"));
                (Regex::new(&pattern).expect("the prefix pattern compiles"), *kind)
            })
            .collect()
    });
    let mut out = Vec::new();
    for (re, kind) in res {
        for m in re.find_iter(text) {
            // A dash or a dot after the piece still belongs to a longer word ("ghp_…-x").
            let after = text[m.end()..].chars().next();
            if apart(text, m.start(), m.end()) && !matches!(after, Some('-')) {
                out.push(Candidate { kind: *kind, range: m.range(), value: m.as_str().into(), date: None });
            }
        }
    }
    out
}

/// The names that make an assignment's value a secret.
const SECRET_NAMES: &[&str] = &["KEY", "SECRET", "TOKEN", "PASS"];

fn assigned(text: &str) -> Vec<Candidate> {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `NAME=value`, `name: value`, `"name": "value"`, on one line.
    let re = compiled(&RE, r#"["']?([A-Za-z_][A-Za-z0-9_.\-]*)["']?[ \t]*[:=][ \t]*["']?([^\s"'`,;]+)"#);
    re.captures_iter(text)
        .filter_map(|caps| {
            let (name, value) = (caps.get(1)?, caps.get(2)?);
            let upper = name.as_str().to_ascii_uppercase();
            if !SECRET_NAMES.iter().any(|s| upper.contains(s)) || !random(value.as_str()) {
                return None;
            }
            let kind = if upper.contains("TOKEN") { EntityKind::Token } else { EntityKind::ApiKey };
            Some(Candidate { kind, range: value.range(), value: value.as_str().into(), date: None })
        })
        .collect()
}

/// Whether an assigned value looks like a generated secret rather than a word, a number, a path
/// or a link.
fn random(value: &str) -> bool {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 16 || value.contains("://") || value.starts_with(['/', '~', '.', '$', '<', '%', '{']) {
        return false;
    }
    let classes = [
        chars.iter().any(char::is_ascii_lowercase),
        chars.iter().any(char::is_ascii_uppercase),
        chars.iter().any(char::is_ascii_digit),
        chars.iter().any(|c| !c.is_ascii_alphanumeric()),
    ];
    classes.iter().filter(|&&c| c).count() >= 2 && entropy(&chars) >= 3.5
}

/// Shannon entropy in bits per character.
fn entropy(chars: &[char]) -> f64 {
    let mut counts: Vec<(char, usize)> = Vec::new();
    for &c in chars {
        match counts.iter_mut().find(|(k, _)| *k == c) {
            Some((_, n)) => *n += 1,
            None => counts.push((c, 1)),
        }
    }
    let total = chars.len() as f64;
    counts.iter().map(|&(_, n)| n as f64 / total).map(|p| -p * p.log2()).sum()
}

// ----------------------------------------------------------------------------- passwords

fn passwords(text: &str) -> Vec<Candidate> {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `scheme://user:password@host`; only the password is hidden, the rest stays readable. The
    // user ends at the first colon and may be empty (`redis://:password@cache`) or an address,
    // and a password may hold `@` and `:` of its own, so it runs to the last `@` before the host.
    let re = compiled(&RE, r"[A-Za-z][A-Za-z0-9+.\-]*://[^\s/:]*:([^\s/]+)@[^\s/@]");
    re.captures_iter(text)
        .filter_map(|caps| {
            let password = caps.get(1)?;
            Some(Candidate {
                kind: EntityKind::UrlPassword,
                range: password.range(),
                value: password.as_str().into(),
                date: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `find` reads in a line: the kind and the piece of each find.
    fn read(text: &str) -> Vec<(EntityKind, String)> {
        find(text).into_iter().map(|c| (c.kind, text[c.range].to_string())).collect()
    }

    #[test]
    fn finds_only_what_passes_its_check() {
        use EntityKind::*;
        let found: &[(&str, &[(EntityKind, &str)])] = &[
            // Cards pass Luhn and carry an issuer's prefix; the same digits with one wrong do not.
            ("Kart 4111 1111 1111 1111 · 08/27", &[(Card, "4111 1111 1111 1111")]),
            ("Amex 3782-822463-10005", &[(Card, "3782-822463-10005")]),
            ("Troy 9792030000000000", &[(Card, "9792030000000000")]),
            ("4111 1111 1111 1112", &[]),
            ("1234 5678 9012 3456", &[]),
            // An expiry, a security code or an order number beside a card is not part of it.
            ("Kart 5555 5555 5555 4444 12/27", &[(Card, "5555 5555 5555 4444")]),
            ("Visa 4242424242424242 123", &[(Card, "4242424242424242")]),
            ("Sipariş 12 4111 1111 1111 1111", &[(Card, "4111 1111 1111 1111")]),
            // A block laid out as code comes back with two spaces between groups.
            ("CARD=4111  1111  1111  1111", &[(Card, "4111  1111  1111  1111")]),
            ("4111  1111  1111  1112", &[]),
            ("IBAN=DE89  3704  0044  0532  0130  00", &[(Iban, "DE89  3704  0044  0532  0130  00")]),
            // IBANs need their country's length and mod 97; the group after one is not part of it.
            ("TR33 0006 1005 1978 6457 8413 26 MODA", &[(Iban, "TR33 0006 1005 1978 6457 8413 26")]),
            ("DE89370400440532013000", &[(Iban, "DE89370400440532013000")]),
            ("TR34 0006 1005 1978 6457 8413 26", &[]),
            // Prefixed keys and tokens, with or without an assignment around them.
            ("STRIPE_KEY=sk_live_51HcR8tLq2Vx0bN7kPzW3", &[(ApiKey, "sk_live_51HcR8tLq2Vx0bN7kPzW3")]),
            ("token ghp_8fK2mZq1Lw9xN4vB7cT0yP3sQe6R", &[(Token, "ghp_8fK2mZq1Lw9xN4vB7cT0yP3sQe6R")]),
            ("AWS_KEY_ID=AKIA4X7Q2M9KZT3B8WQE", &[(ApiKey, "AKIA4X7Q2M9KZT3B8WQE")]),
            (
                "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
                &[(Token, "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U")],
            ),
            // What text recognition reads in a monospaced font: a slashed zero, an `x` as `×`.
            ("Bearer ghp_8fK2mZq1Lw9×N4vB7cTØyP3sQe6R", &[(Token, "ghp_8fK2mZq1Lw9×N4vB7cTØyP3sQe6R")]),
            ("IBAN TR33 ØØØ6 1ØØ5 1978 6457 8413 26", &[(Iban, "TR33 ØØØ6 1ØØ5 1978 6457 8413 26")]),
            ("Troy 97920300000000ØØ", &[(Card, "97920300000000ØØ")]),
            // A key without a prefix is found by its name and its randomness.
            ("AWS_SECRET_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYzEX", &[(ApiKey, "wJalrXUtnFEMI/K7MDENG/bPxRfiCYzEX")]),
            ("\"session_token\": \"q8Zr2LwX9vNc4Tb7Ym3K\"", &[(Token, "q8Zr2LwX9vNc4Tb7Ym3K")]),
            // Only the password of a link, not its user or host.
            ("DB_URL=postgres://app:Kx9pQ2vLm4@db.internal/prod", &[(UrlPassword, "Kx9pQ2vLm4")]),
            // A password with an `@` of its own, a link without a user, and a user that is an address.
            ("postgres://app:p@ss9Kx2Lm@db.internal", &[(UrlPassword, "p@ss9Kx2Lm")]),
            ("redis://:s3cr3tPassW0rd@cache.local:6379", &[(UrlPassword, "s3cr3tPassW0rd")]),
            ("smtp://ayse@ornek.com.tr:Q7wP2xLm@mail.ornek.com.tr:587", &[(UrlPassword, "Q7wP2xLm")]),
            ("https://panel.ornek.com.tr:8443/giris", &[]),
            // Order numbers, small settings, words, hashes, UUIDs and links are no secrets.
            ("Sipariş TK-448812 yola çıktı", &[]),
            ("PASSWORD_MIN_LENGTH=12", &[]),
            ("SESSION_SECRET=correcthorsebatterystaple", &[]),
            ("API_KEY_PATH=/Users/ayse/.config/panel/key.pem", &[]),
            ("TOKEN_URL=https://auth.ornek.com.tr/oauth/token", &[]),
            ("commit 3f786850e387550fdab836ed7e6dc881de23001b", &[]),
            ("id 550e8400-e29b-41d4-a716-446655440000", &[]),
            ("APP_URL=https://panel.ornek.com.tr", &[]),
        ];
        for (text, expected) in found {
            let expected: Vec<(EntityKind, String)> = expected.iter().map(|(k, s)| (*k, s.to_string())).collect();
            assert_eq!(read(text), expected, "in {text:?}");
        }
    }

    /// The other detectors read the `secret@host` of a link as an email address. The password
    /// must hide that address, or a list with its addresses switched off exports the password.
    #[test]
    fn a_links_password_hides_the_address_read_in_it() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 23).and_then(|d| d.and_hms_opt(12, 0, 0)).expect("a date");
        for text in
            ["DB_URL=postgres://app:p@ss9Kx2Lm@db.internal/prod", "REDIS_URL=redis://:s3cr3tPassW0rd@cache.local:6379"]
        {
            let sensitive = find(text);
            #[allow(unused_mut)]
            let mut others = super::super::rules::find(text, now);
            #[cfg(target_os = "macos")]
            others.extend(super::super::apple::detect(text));
            let addresses: Vec<&str> = others
                .iter()
                .filter(|c| c.kind == EntityKind::Email && !hides(text, &sensitive, c))
                .map(|c| &text[c.range.clone()])
                .collect();
            assert!(addresses.is_empty(), "{addresses:?} kept beside the password in {text:?}");
        }
    }
}
