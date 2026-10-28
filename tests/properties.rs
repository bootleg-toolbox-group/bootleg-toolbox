//! Property tests (proptest): rules that must hold for any input, checked on thousands of
//! generated cases.

use std::cmp::Ordering;

use bootleg_toolbox::files::natural_cmp;
use bootleg_toolbox::sbe::{self, SECTOR};
use bootleg_toolbox::tagging::{find_date, parse_info};
use proptest::prelude::*;

/// Track lengths in sample frames: from a few sectors to about 20 minutes.
fn disc() -> impl Strategy<Value = Vec<u64>> {
    prop::collection::vec(1u64..53_000_000, 1..30)
}

proptest! {
    /// A fixed disc keeps all its audio, in order, and only gains the silence that fills
    /// the last sector: the new tracks follow each other without gap or overlap.
    #[test]
    fn sbe_plan_keeps_the_stream(lengths in disc()) {
        let plan = sbe::plan(&lengths);
        prop_assert_eq!(plan.len(), lengths.len());
        let total: u64 = lengths.iter().sum();
        let mut start = 0;
        let mut frames = 0;
        for (i, (cut, len)) in plan.iter().zip(&lengths).enumerate() {
            let (s, e, pad) = match cut {
                Some(c) => (c.start, c.end, c.pad),
                None => (start, start + len, 0),
            };
            prop_assert_eq!(s, start, "track {} does not start where the previous one ends", i);
            prop_assert!(e >= s);
            start = e;
            frames += e - s + pad;
        }
        prop_assert_eq!(start, total);
        prop_assert_eq!(frames, total.div_ceil(SECTOR) * SECTOR);
    }

    /// Every new track but the last ends on a sector boundary: no SBE is left.
    #[test]
    fn sbe_plan_leaves_no_sbe(lengths in disc()) {
        let plan = sbe::plan(&lengths);
        let n = lengths.len();
        let new_lengths: Vec<u64> = plan.iter().zip(&lengths).map(|(cut, len)| cut.map_or(*len, |c| c.frames())).collect();
        prop_assert!(!sbe::needs_fix(&new_lengths));
        prop_assert_eq!(new_lengths[n - 1] % SECTOR, 0);
    }

    /// A disc without SBE before its last track is left as it is.
    #[test]
    fn sbe_plan_leaves_aligned_tracks(sectors in prop::collection::vec(1u64..90_000, 1..30), last in 1u64..53_000_000) {
        let mut lengths: Vec<u64> = sectors.iter().map(|s| s * SECTOR).collect();
        lengths.push(last);
        prop_assert!(!sbe::needs_fix(&lengths));
        let plan = sbe::plan(&lengths);
        prop_assert!(plan[..plan.len() - 1].iter().all(Option::is_none));
    }

    /// The natural order is an order: sorting with it is stable and consistent.
    #[test]
    fn natural_order_is_consistent(a in "[a-zA-Z0-9 ._-]{0,12}", b in "[a-zA-Z0-9 ._-]{0,12}", c in "[a-zA-Z0-9 ._-]{0,12}") {
        prop_assert_eq!(natural_cmp(&a, &a), Ordering::Equal);
        prop_assert_eq!(natural_cmp(&a, &b), natural_cmp(&b, &a).reverse());
        if natural_cmp(&a, &b) != Ordering::Greater && natural_cmp(&b, &c) != Ordering::Greater {
            prop_assert_ne!(natural_cmp(&a, &c), Ordering::Greater);
        }
    }

    /// "Track 2" comes before "Track 10", whatever the numbers.
    #[test]
    fn natural_order_counts(m in 0u64..1_000_000, n in 0u64..1_000_000) {
        prop_assert_eq!(natural_cmp(&format!("Track {m}.flac"), &format!("Track {n}.flac")), m.cmp(&n));
    }

    /// Info files come from anywhere: reading one never fails, whatever it holds, and
    /// a date found is always a real YYYY-MM-DD date.
    #[test]
    fn info_files_never_break(text in "\\PC{0,400}", files in 0usize..40) {
        let parsed = parse_info(&text, files, None);
        prop_assert!(parsed.date.is_empty() || valid_date(&parsed.date), "{}", parsed.date);
        if let Some(date) = find_date(&text) {
            prop_assert!(valid_date(&date), "{}", date);
        }
    }

    /// Dates as tapers write them.
    #[test]
    fn dates_are_found(y in 1950u32..2030, m in 1u32..=12, d in 1u32..=28) {
        let expected = format!("{y:04}-{m:02}-{d:02}");
        prop_assert_eq!(find_date(&format!("Recorded {y:04}-{m:02}-{d:02}")), Some(expected.clone()));
        prop_assert_eq!(find_date(&format!("{d:02}.{m:02}.{y:04} Live")), Some(expected));
    }
}

fn valid_date(date: &str) -> bool {
    let parts: Vec<&str> = date.split('-').collect();
    let [y, m, d] = parts[..] else { return false };
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<u32>(), m.parse::<u32>(), d.parse::<u32>()) else { return false };
    y.to_string().len() == 4 && (1..=12).contains(&m) && (1..=31).contains(&d)
}
