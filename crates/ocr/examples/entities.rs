//! Measures how many dates NSDataDetector reads in each language of the app, next to the Rust
//! rules and to both together, as `find_entities` runs them. Each sample sentence holds one date
//! with its expected start, end and all-day flag; a reading counts when all three match. Outside
//! macOS the NSDataDetector column stays empty.
//!
//! ```sh
//! cargo run -p glimpsfy-ocr --example entities
//! cargo run -p glimpsfy-ocr --example entities -- --verbose
//! ```

use chrono::{Local, NaiveDate, NaiveDateTime};
use glimpsfy_ocr::entities::measure_dates;
use glimpsfy_ocr::EntityDate;

/// A sample: the sentence, the start, the end (`None` for an all-day date of one day) and
/// whether it is all day. A timed sample without an end in the text expects the assumed hour.
type Sample = (&'static str, &'static str, Option<&'static str>, bool);

const SAMPLES: &[(&str, &[Sample])] = &[
    (
        "en",
        &[
            (
                "The launch is on November 27, 2026 at 2:30 pm in the main hall.",
                "2026-11-27 14:30",
                Some("2026-11-27 15:30"),
                false,
            ),
            ("Delivery window: Monday 30 November, 09:00–12:00", "2026-11-30 09:00", Some("2026-11-30 12:00"), false),
            ("Please reply by 3 December 2026.", "2026-12-03 00:00", None, true),
            ("Dinner on Thursday, Dec 3 at 7 pm", "2026-12-03 19:00", Some("2026-12-03 20:00"), false),
            ("Meeting 2026-12-14 10:00", "2026-12-14 10:00", Some("2026-12-14 11:00"), false),
        ],
    ),
    (
        "tr",
        &[
            (
                "Ürün sunumunu 27 Kasım Cuma 14:30’da Kadıköy ofisinde yapıyoruz.",
                "2026-11-27 14:30",
                Some("2026-11-27 15:30"),
                false,
            ),
            ("Teslimat: Pazartesi 30 Kasım, 09:00–12:00", "2026-11-30 09:00", Some("2026-11-30 12:00"), false),
            ("Tarih: 3 Aralık 2026 Perşembe, 19:00 · 4 kişi", "2026-12-03 19:00", Some("2026-12-03 20:00"), false),
            ("Son başvuru 15.01.2027 tarihidir.", "2027-01-15 00:00", None, true),
            ("Toplantı 14 Aralık saat 10.00'da başlar.", "2026-12-14 10:00", Some("2026-12-14 11:00"), false),
        ],
    ),
    (
        "de",
        &[
            (
                "Die Präsentation ist am 27. November 2026 um 14:30 Uhr.",
                "2026-11-27 14:30",
                Some("2026-11-27 15:30"),
                false,
            ),
            (
                "Lieferfenster: Mo., 30. November 2026 · 09:00–12:00",
                "2026-11-30 09:00",
                Some("2026-11-30 12:00"),
                false,
            ),
            ("Bitte antworten Sie bis zum 3. Dezember.", "2026-12-03 00:00", None, true),
            ("Termin: 15.01.2027, 10:00", "2027-01-15 10:00", Some("2027-01-15 11:00"), false),
            ("Treffen am Donnerstag, 3. Dezember um 19 Uhr", "2026-12-03 19:00", Some("2026-12-03 20:00"), false),
        ],
    ),
    (
        "es",
        &[
            (
                "La presentación es el 27 de noviembre de 2026 a las 14:30.",
                "2026-11-27 14:30",
                Some("2026-11-27 15:30"),
                false,
            ),
            ("Entrega: lunes 30 de noviembre, 09:00–12:00", "2026-11-30 09:00", Some("2026-11-30 12:00"), false),
            ("Responda antes del 3 de diciembre.", "2026-12-03 00:00", None, true),
            ("Cita: 15/01/2027 10:00", "2027-01-15 10:00", Some("2027-01-15 11:00"), false),
            ("Cena el jueves 3 de diciembre a las 19:00", "2026-12-03 19:00", Some("2026-12-03 20:00"), false),
        ],
    ),
    (
        "ru",
        &[
            ("Презентация состоится 27 ноября 2026 года в 14:30.", "2026-11-27 14:30", Some("2026-11-27 15:30"), false),
            ("Доставка: понедельник, 30 ноября, 09:00–12:00", "2026-11-30 09:00", Some("2026-11-30 12:00"), false),
            ("Ответьте до 3 декабря.", "2026-12-03 00:00", None, true),
            ("Встреча 15.01.2027 в 10:00", "2027-01-15 10:00", Some("2027-01-15 11:00"), false),
            ("Ужин в четверг, 3 декабря в 19:00", "2026-12-03 19:00", Some("2026-12-03 20:00"), false),
        ],
    ),
    (
        "ja",
        &[
            (
                "発表会は2026年11月27日（金）14時30分から始まります。",
                "2026-11-27 14:30",
                Some("2026-11-27 15:30"),
                false,
            ),
            ("配達：11月30日(月) 9:00〜12:00", "2026-11-30 09:00", Some("2026-11-30 12:00"), false),
            ("12月3日までにご返信ください。", "2026-12-03 00:00", None, true),
            ("会議は2027年1月15日 10:00です。", "2027-01-15 10:00", Some("2027-01-15 11:00"), false),
            ("夕食は12月3日 午後7時です。", "2026-12-03 19:00", Some("2026-12-03 20:00"), false),
        ],
    ),
    (
        "zh-Hans",
        &[
            ("产品发布会将于2026年11月27日（星期五）14:30举行。", "2026-11-27 14:30", Some("2026-11-27 15:30"), false),
            ("送货时间：11月30日 周一 9:00-12:00", "2026-11-30 09:00", Some("2026-11-30 12:00"), false),
            ("请在12月3日前回复。", "2026-12-03 00:00", None, true),
            ("会议时间为2027年1月15日上午10点。", "2027-01-15 10:00", Some("2027-01-15 11:00"), false),
            ("晚餐在12月3日晚上7点半。", "2026-12-03 19:30", Some("2026-12-03 20:30"), false),
        ],
    ),
];

fn at(text: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").expect("a sample's date parses")
}

fn matches(found: &EntityDate, sample: &Sample) -> bool {
    let (_, start, end, all_day) = *sample;
    found.start == at(start) && found.end == end.map(at) && found.all_day == all_day
}

fn main() {
    let verbose = std::env::args().any(|a| a == "--verbose");
    // The samples' years assume late November 2026; the rules read them against that day, and
    // NSDataDetector against the system's clock.
    let now = NaiveDate::from_ymd_opt(2026, 11, 23).and_then(|d| d.and_hms_opt(12, 0, 0)).expect("a valid day");
    let today = Local::now().date_naive();
    if today.signed_duration_since(now.date()).num_days().abs() > 60 {
        eprintln!("note: today is {today}; NSDataDetector may give the samples without a year another year");
    }
    println!("{:<8} {:>15} {:>8} {:>8}", "language", "NSDataDetector", "rules", "both");
    let mut totals = [0usize; 3];
    let mut count = 0;
    for (language, samples) in SAMPLES {
        let mut hits = [0usize; 3];
        for sample in *samples {
            let readings = measure_dates(sample.0, now);
            for (i, found) in readings.iter().enumerate() {
                let hit = found.iter().any(|(_, date)| matches(date, sample));
                hits[i] += usize::from(hit);
                if verbose && !hit {
                    let name = ["NSDataDetector", "rules", "both"][i];
                    eprintln!("[{language}] {name} missed «{}»: {found:?}", sample.0);
                }
            }
        }
        for i in 0..3 {
            totals[i] += hits[i];
        }
        count += samples.len();
        let n = samples.len();
        println!("{language:<8} {:>13}/{n} {:>6}/{n} {:>6}/{n}", hits[0], hits[1], hits[2]);
    }
    println!("{:<8} {:>12}/{count} {:>5}/{count} {:>5}/{count}", "all", totals[0], totals[1], totals[2]);
}
