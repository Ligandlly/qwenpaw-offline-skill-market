//! Version ordering. `latest` decisions, the publish guard and the dry run all
//! read the same comparison, so it lives on its own.

use std::cmp::Ordering;

/// Version precedence, in the part of semver that decides which version is
/// `latest`: build metadata is ignored, a prerelease sorts *below* its own
/// release (`1.0.0-beta < 1.0.0`), and a missing segment counts as zero
/// (`1.0 == 1.0.0`).
///
/// The market does not require semver — `limits::valid_version` accepts any
/// version a host might have produced — so a non-numeric segment falls back to
/// a lexical compare instead of being rejected.
pub(crate) fn compare_versions(a: &str, b: &str) -> Ordering {
    let (a_core, a_pre) = split_version(a);
    let (b_core, b_pre) = split_version(b);
    cmp_core(a_core, b_core).then_with(|| cmp_prerelease(a_pre, b_pre))
}

/// `major.minor.patch[-pre][+build]` → (`major.minor.patch`, `pre`). Both build
/// metadata and everything after the first `-` are dropped from the core; a
/// hyphen inside an identifier (`1.0.0-alpha-1`) stays part of the prerelease.
fn split_version(v: &str) -> (&str, &str) {
    let without_build = v.split('+').next().unwrap_or(v);
    match without_build.split_once('-') {
        Some((core, pre)) => (core, pre),
        None => (without_build, ""),
    }
}

fn cmp_core(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.split('.'), b.split('.'));
    loop {
        // A missing segment is a zero, so trailing `.0`s never decide.
        match (ai.next(), bi.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (x, y) => {
                let ord = cmp_segment(x.unwrap_or("0"), y.unwrap_or("0"));
                if ord.is_ne() {
                    return ord;
                }
            }
        }
    }
}

fn cmp_segment(x: &str, y: &str) -> std::cmp::Ordering {
    match (x.parse::<u64>(), y.parse::<u64>()) {
        (Ok(nx), Ok(ny)) => nx.cmp(&ny),
        _ => x.cmp(y),
    }
}

fn cmp_prerelease(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Equal,
        // A release outranks every prerelease of the same core version.
        (true, false) => return Greater,
        (false, true) => return Less,
        (false, false) => {}
    }
    let (mut ai, mut bi) = (a.split('.'), b.split('.'));
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Equal,
            // `alpha` < `alpha.1`: the longer prerelease wins a shared prefix.
            (Some(_), None) => return Greater,
            (None, Some(_)) => return Less,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    // Numeric identifiers rank below alphanumeric ones.
                    (Ok(_), Err(_)) => Less,
                    (Err(_), Ok(_)) => Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if ord.is_ne() {
                    return ord;
                }
            }
        }
    }
}

/// Bump the patch component of ``x.y.z`` for the "publish a new version" hint.
/// Anything unexpected (a tag like ``v1``, a date, four components) yields
/// ``None`` and the form keeps whatever the user typed.
pub(crate) fn suggest_next_version(latest: &str) -> Option<String> {
    let parts: Vec<&str> = latest.trim().split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut numbers = [0u64; 3];
    for (slot, part) in numbers.iter_mut().zip(parts.iter()) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some(format!(
        "{}.{}.{}",
        numbers[0],
        numbers[1],
        numbers[2].checked_add(1)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_sanely() {
        use std::cmp::Ordering::*;
        let cases = [
            // core ordering is numeric, not lexical
            ("1.0.10", "1.0.9", Greater),
            ("1.0.0", "1.0.0", Equal),
            ("2.0.0", "10.0.0", Less),
            ("1.1.0", "1.0.99", Greater),
            ("1.0.1", "1.0", Greater),
            // a missing segment is a zero, never a lexical empty string
            ("1.0", "1.0.0", Equal),
            ("1", "1.0.0", Equal),
            ("1.0", "1.0.1", Less),
            // a prerelease sorts below its own release
            ("1.0.0", "1.0.0-beta", Greater),
            ("1.0.0-beta", "1.0.0", Less),
            ("2.0.0-rc.1", "2.0.0", Less),
            ("1.0.1-beta", "1.0.1", Less),
            ("1.0.1-beta", "1.0.0", Greater),
            // prerelease identifiers, in semver precedence order
            ("1.0.0-alpha", "1.0.0-alpha.1", Less),
            ("1.0.0-alpha.1", "1.0.0-alpha.beta", Less),
            ("1.0.0-alpha.beta", "1.0.0-beta", Less),
            ("1.0.0-beta", "1.0.0-beta.2", Less),
            ("1.0.0-beta.2", "1.0.0-beta.11", Less),
            ("1.0.0-beta.11", "1.0.0-rc.1", Less),
            ("1.0.0-rc.1", "1.0.0-rc.2", Less),
            ("1.0.0-0", "1.0.0", Less),
            // build metadata is not part of precedence at all
            ("1.0.0+build.1", "1.0.0", Equal),
            ("1.0.0+build.2", "1.0.0+build.1", Equal),
            ("1.0.0-rc.1+build", "1.0.0-rc.1", Equal),
        ];
        for (left, right, expected) in cases {
            assert_eq!(compare_versions(left, right), expected, "{left} vs {right}");
            assert_eq!(
                compare_versions(right, left),
                expected.reverse(),
                "{right} vs {left} (antisymmetry)"
            );
        }
    }

    /// The comparison is used as `is_gt()` on the stored `latest_version`, so
    /// it only needs to be a consistent total order over a realistic release
    /// train — sorted once by hand, then re-checked pairwise.
    #[test]
    fn version_order_is_a_consistent_total_order() {
        use std::cmp::Ordering::*;
        let train = [
            "0.1.0",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-beta",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.0.2-rc.1",
            "1.0.2",
            "1.2.0",
            "1.10.0",
            "2.0.0",
        ];
        for (i, lower) in train.iter().enumerate() {
            for higher in &train[i + 1..] {
                assert_eq!(compare_versions(lower, higher), Less, "{lower} < {higher}");
                assert_eq!(compare_versions(higher, lower), Greater, "{higher} > {lower}");
            }
            assert_eq!(compare_versions(lower, lower), Equal, "{lower} == {lower}");
        }
    }

    #[test]
    fn suggests_the_next_patch_version() {
        assert_eq!(suggest_next_version("1.0.0").as_deref(), Some("1.0.1"));
        assert_eq!(suggest_next_version("2.2.2").as_deref(), Some("2.2.3"));
        assert_eq!(suggest_next_version("1.0.9").as_deref(), Some("1.0.10"));
        // Anything non-semver leaves the field alone.
        for odd in ["v1", "1.0", "1.0.0.post1", "latest", ""] {
            assert_eq!(suggest_next_version(odd), None, "{odd}");
        }
    }
}
