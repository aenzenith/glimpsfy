//! Filler words and hesitation sounds in seven languages (en, tr, de, es, ru, ja, zh): which
//! words "Cut all" removes, and the prompt that makes the model write them out.
//!
//! Whisper was trained on cleaned-up subtitles and drops "ııı" and "um" unless its prompt is
//! full of them; [`prompt`] gives it such a sentence in the spoken language. A word is then
//! compared in a normal form ([`normalize`]): lower case with Turkish's dotted and dotless i,
//! without punctuation, and with repeated letters collapsed, because the model writes the same
//! sound as "Iıı", "ııı", "ıı" or "Eee...".
//!
//! Hesitation sounds are fillers wherever they are. Words that are fillers only sometimes are
//! marked by their neighbours: "like", "also", "ну" and the like only between pauses or commas,
//! and Turkish "şey" unless a word before it makes it the noun "thing" ("bir şey", "her şey",
//! "söylediğim şey").

use crate::engine::unspaced;
use crate::{FillerKind, Word};

/// The longest filler in words (or, in languages without spaces, in the model's word pieces).
const MAX_PHRASE: usize = 3;
/// A pause this long around a word parts it from its neighbours like a comma.
const PAUSE_MS: u64 = 250;
/// Turkish "şey" after a modifier is the noun when no more than this lies between them.
const JOINED_MS: u64 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    /// Always a filler.
    Always,
    /// A filler between pauses, commas or other fillers only.
    Bounded,
    /// Turkish "şey"; see the module's notes.
    TurkishSey,
}

struct Entry {
    /// The normal form; the words of a phrase are joined by one space, or by nothing in languages
    /// without spaces.
    form: &'static str,
    kind: FillerKind,
    rule: Rule,
}

const fn sound(form: &'static str) -> Entry {
    Entry { form, kind: FillerKind::Sound, rule: Rule::Always }
}

const fn word(form: &'static str, rule: Rule) -> Entry {
    Entry { form, kind: FillerKind::Word, rule }
}

// The forms are normal forms: lower case, no punctuation, no letter twice in a row ("eee" is "e").
const TURKISH: &[Entry] = &[
    sound("ı"),
    sound("e"),
    sound("ö"),
    sound("ıh"),
    sound("hı"),
    sound("eh"),
    sound("öh"),
    sound("ım"),
    sound("hm"),
    sound("m"),
    word("şey", Rule::TurkishSey),
    word("yani", Rule::Always),
    word("hani", Rule::Always),
];

const ENGLISH: &[Entry] = &[
    sound("um"),
    sound("uh"),
    sound("uhm"),
    sound("er"),
    sound("erm"),
    sound("ehm"),
    sound("hm"),
    sound("m"),
    word("like", Rule::Bounded),
    word("you know", Rule::Bounded),
    word("i mean", Rule::Bounded),
];

const GERMAN: &[Entry] = &[
    sound("äh"),
    sound("ähm"),
    sound("öh"),
    sound("öhm"),
    sound("ehm"),
    sound("em"),
    sound("hm"),
    sound("m"),
    word("also", Rule::Bounded),
    word("sozusagen", Rule::Bounded),
    word("quasi", Rule::Bounded),
];

const SPANISH: &[Entry] = &[
    sound("eh"),
    sound("em"),
    sound("ehm"),
    sound("hm"),
    sound("m"),
    word("o sea", Rule::Always),
    word("este", Rule::Bounded),
    word("pues", Rule::Bounded),
    word("bueno", Rule::Bounded),
    word("sabes", Rule::Bounded),
];

const RUSSIAN: &[Entry] = &[
    sound("э"),
    sound("эм"),
    sound("м"),
    sound("хм"),
    word("это самое", Rule::Always),
    word("как бы", Rule::Bounded),
    word("ну", Rule::Bounded),
    word("типа", Rule::Bounded),
    word("вот", Rule::Bounded),
    word("короче", Rule::Bounded),
    word("значит", Rule::Bounded),
];

const JAPANESE: &[Entry] = &[
    sound("えー"),
    sound("えーと"),
    sound("えっと"),
    sound("えと"),
    sound("あー"),
    sound("うーん"),
    sound("んー"),
    word("あのー", Rule::Always),
    word("そのー", Rule::Always),
    word("あの", Rule::Bounded),
    word("その", Rule::Bounded),
    word("まあ", Rule::Bounded),
    word("なんか", Rule::Bounded),
];

const CHINESE: &[Entry] = &[
    sound("嗯"),
    sound("呃"),
    sound("额"),
    sound("唔"),
    Entry { form: "啊", kind: FillerKind::Sound, rule: Rule::Bounded },
    word("那个", Rule::Bounded),
    word("这个", Rule::Bounded),
    word("就是", Rule::Bounded),
    word("就是说", Rule::Bounded),
];

/// Words that make the next "şey" the noun "thing": determiners, quantifiers and adjectives.
const TURKISH_MODIFIERS: &[&str] = &[
    "bir", "her", "hiçbir", "herhangi", "birçok", "birkaç", "bazı", "bu", "şu", "o", "öyle", "böyle", "şöyle", "ne",
    "hangi", "aynı", "başka", "tek", "çok", "az", "iyi", "kötü", "güzel", "yeni", "eski", "büyük", "küçük", "garip",
    "tuhaf", "ilginç", "basit", "zor", "kolay", "son", "ilk", "diğer", "öbür", "öteki", "tüm", "bütün", "ufak", "koca",
    "acayip", "saçma", "harika", "müthiş", "doğru", "yanlış", "gerçek", "asıl", "esas", "ayrı", "ortak", "kesin",
    "net", "temel", "genel", "özel", "ana", "hoş", "fena", "kalan",
];

/// Word endings that make a modifier of a word: participles ("söylediğim şey", "olan şey",
/// "yapacak şey") and adjectives ("önemli şey", "anlamsız şey", "buradaki şey"). Normal forms,
/// so a doubled letter is written once.
const TURKISH_MODIFIER_ENDINGS: &[&str] = &[
    "dığım",
    "diğim",
    "duğum",
    "düğüm",
    "tığım",
    "tiğim",
    "tuğum",
    "tüğüm",
    "dığın",
    "diğin",
    "duğun",
    "düğün",
    "tığın",
    "tiğin",
    "tuğun",
    "tüğün",
    "dığı",
    "diği",
    "duğu",
    "düğü",
    "tığı",
    "tiği",
    "tuğu",
    "tüğü",
    "dığımız",
    "diğimiz",
    "duğumuz",
    "düğümüz",
    "dıkları",
    "dikleri",
    "dukları",
    "dükleri",
    "tıkları",
    "tikleri",
    "tukları",
    "tükleri",
    "acak",
    "ecek",
    "acağım",
    "eceğim",
    "acağı",
    "eceği",
    "an",
    "en",
    "lı",
    "li",
    "lu",
    "lü",
    "sız",
    "siz",
    "suz",
    "süz",
    "ki",
    "kü",
    "sal",
    "sel",
];

/// Words after which "şey" is the hesitation: conjunctions and discourse words.
const TURKISH_CONNECTORS: &[&str] = &[
    "ve", "ama", "fakat", "ancak", "yani", "hani", "işte", "sonra", "şimdi", "de", "da", "ki", "ya", "yok", "evet",
    "hayır", "peki", "tamam", "bak", "bakın", "mesela", "aslında",
];

/// The prompt that makes the model write the fillers of `language` out, or `None` for a
/// language without a filler list.
pub fn prompt(language: &str) -> Option<&'static str> {
    Some(match language {
        "tr" => "Iıı, şey, bugün eee yani size hani bunu göstereceğim.",
        "en" => "Um, so, uh, today I'm, like, going to show you, hmm, this.",
        "de" => "Ähm, also, äh, heute zeige ich euch, hm, sozusagen das hier.",
        "es" => "Eh, bueno, este, hoy les voy a mostrar, mmm, o sea, esto.",
        "ru" => "Э-э, ну, как бы, сегодня я, эм, покажу вам вот это.",
        "ja" => "えーと、あのー、今日は、まあ、これをお見せします。",
        "zh" => "嗯，那个，今天我呃，就是给大家看一下这个。",
        _ => return None,
    })
}

fn lexicon(language: &str) -> &'static [Entry] {
    match language {
        "tr" => TURKISH,
        "en" => ENGLISH,
        "de" => GERMAN,
        "es" => SPANISH,
        "ru" => RUSSIAN,
        "ja" => JAPANESE,
        "zh" => CHINESE,
        _ => &[],
    }
}

/// The form a word is compared in: lower case (Turkish and Azerbaijani with I → ı and İ → i),
/// letters and digits only, and no letter twice in a row.
pub fn normalize(text: &str, language: &str) -> String {
    let dotless = matches!(language, "tr" | "az");
    let mut out = String::with_capacity(text.len());
    let mut last = None;
    let mut add = |c: char| {
        if c.is_alphanumeric() && last != Some(c) {
            out.push(c);
            last = Some(c);
        }
    };
    for c in text.chars() {
        match c {
            'I' if dotless => add('ı'),
            'İ' if dotless => add('i'),
            _ => c.to_lowercase().for_each(&mut add),
        }
    }
    out
}

/// Marks `words[j]`, and the rest of a phrase that starts there, when it is a filler. The words
/// before `j` are marked already; at least [`MAX_PHRASE`] words follow it unless `finished` says
/// that `words` ends the recording.
pub(crate) fn mark(language: &str, words: &mut [Word], j: usize, finished: bool) {
    if words[j].filler.is_some() {
        return;
    }
    let lexicon = lexicon(language);
    if lexicon.is_empty() {
        return;
    }
    let separator = if unspaced(language) { "" } else { " " };
    for n in (1..=MAX_PHRASE).rev() {
        if j + n > words.len() {
            continue;
        }
        let form: Vec<String> = words[j..j + n].iter().map(|w| normalize(&w.text, language)).collect();
        if form.iter().any(String::is_empty) {
            continue;
        }
        let form = form.join(separator);
        let Some(entry) = lexicon.iter().find(|e| e.form == form) else { continue };
        let context = Context { language, lexicon, words, finished };
        let applies = match entry.rule {
            Rule::Always => true,
            Rule::Bounded => context.before(j) && context.after(j + n - 1),
            Rule::TurkishSey => context.sey(j),
        };
        if applies {
            for w in &mut words[j..j + n] {
                w.filler = Some(entry.kind);
            }
            return;
        }
    }
}

struct Context<'a> {
    language: &'a str,
    lexicon: &'static [Entry],
    words: &'a [Word],
    finished: bool,
}

impl Context<'_> {
    /// Whether a pause, a comma or a filler comes right before word `k`.
    fn before(&self, k: usize) -> bool {
        let Some(prev) = k.checked_sub(1).map(|p| &self.words[p]) else { return true };
        breaks(&prev.text) || prev.filler.is_some() || self.words[k].start_ms.saturating_sub(prev.end_ms) >= PAUSE_MS
    }

    /// Whether a pause, a comma or a hesitation sound comes right after word `k`.
    fn after(&self, k: usize) -> bool {
        if breaks(&self.words[k].text) {
            return true;
        }
        match self.words.get(k + 1) {
            None => self.finished,
            Some(next) => next.start_ms.saturating_sub(self.words[k].end_ms) >= PAUSE_MS || self.is_sound(next),
        }
    }

    fn is_sound(&self, word: &Word) -> bool {
        let form = normalize(&word.text, self.language);
        self.lexicon.iter().any(|e| e.kind == FillerKind::Sound && e.rule == Rule::Always && e.form == form)
    }

    /// Whether Turkish "şey" at `j` is the hesitation rather than the noun.
    fn sey(&self, j: usize) -> bool {
        // The word it would belong to, past any hesitation in between ("bir ııı şey").
        let Some(p) = (0..j).rev().find(|k| self.words[*k].filler.is_none()) else { return true };
        let prev = &self.words[p];
        let joined = !breaks(&prev.text) && self.words[j].start_ms.saturating_sub(prev.end_ms) < JOINED_MS;
        if !joined {
            return true;
        }
        let form = normalize(&prev.text, self.language);
        if modifies(&form) {
            return false;
        }
        p + 1 < j || TURKISH_CONNECTORS.contains(&form.as_str()) || self.after(j)
    }
}

/// Whether a Turkish word, in normal form, makes the next "şey" a noun.
fn modifies(form: &str) -> bool {
    TURKISH_MODIFIERS.contains(&form)
        || (form.chars().count() >= 4 && TURKISH_MODIFIER_ENDINGS.iter().any(|e| form.ends_with(e)))
}

/// Whether a word's text ends with punctuation that parts it from the next word.
fn breaks(text: &str) -> bool {
    text.trim_end_matches(['"', '\'', '»', '”', '’', ')', '」', '』', '）'])
        .chars()
        .last()
        .is_some_and(|c| ",.;:!?…-–—，。、！？；：".contains(c))
}
