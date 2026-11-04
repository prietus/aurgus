//! Port of libalpm's version comparison (`alpm_pkg_vercmp` / `rpmvercmp`).

use std::cmp::Ordering::{self, Equal, Greater, Less};

pub fn vercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Equal;
    }
    let (e1, v1, r1) = parse_evr(a);
    let (e2, v2, r2) = parse_evr(b);
    let mut ret = rpmvercmp(e1, e2);
    if ret == Equal {
        ret = rpmvercmp(v1, v2);
        if ret == Equal
            && let (Some(r1), Some(r2)) = (r1, r2)
        {
            ret = rpmvercmp(r1, r2);
        }
    }
    ret
}

/// Splits `[epoch:]version[-release]`.
fn parse_evr(s: &str) -> (&str, &str, Option<&str>) {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    let (epoch, rest) = if s[digits..].starts_with(':') {
        let e = &s[..digits];
        (if e.is_empty() { "0" } else { e }, &s[digits + 1..])
    } else {
        ("0", s)
    };
    match rest.rfind('-') {
        Some(i) => (epoch, &rest[..i], Some(&rest[i + 1..])),
        None => (epoch, rest, None),
    }
}

fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Equal;
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut one, mut two) = (0usize, 0usize);
    let (mut p1, mut p2) = (0usize, 0usize);

    while one < a.len() && two < b.len() {
        while one < a.len() && !a[one].is_ascii_alphanumeric() {
            one += 1;
        }
        while two < b.len() && !b[two].is_ascii_alphanumeric() {
            two += 1;
        }
        if one >= a.len() || two >= b.len() {
            break;
        }
        // Different separator lengths decide the comparison.
        if one - p1 != two - p2 {
            return if one - p1 < two - p2 { Less } else { Greater };
        }
        p1 = one;
        p2 = two;
        let isnum = a[p1].is_ascii_digit();
        let class: fn(&u8) -> bool = if isnum {
            u8::is_ascii_digit
        } else {
            u8::is_ascii_alphabetic
        };
        while p1 < a.len() && class(&a[p1]) {
            p1 += 1;
        }
        while p2 < b.len() && class(&b[p2]) {
            p2 += 1;
        }
        let (s1, s2) = (&a[one..p1], &b[two..p2]);
        if s2.is_empty() {
            // Numeric segments are newer than alpha segments.
            return if isnum { Greater } else { Less };
        }
        let ord = if isnum {
            let t1 = &s1[s1.iter().take_while(|&&c| c == b'0').count()..];
            let t2 = &s2[s2.iter().take_while(|&&c| c == b'0').count()..];
            t1.len().cmp(&t2.len()).then_with(|| t1.cmp(t2))
        } else {
            s1.cmp(s2)
        };
        if ord != Equal {
            return ord;
        }
        one = p1;
        two = p2;
    }

    let (end1, end2) = (one >= a.len(), two >= b.len());
    if end1 && end2 {
        return Equal;
    }
    // "1.0" > "1.0alpha", "1.0.1" > "1.0"
    if (end1 && !b[two].is_ascii_alphabetic()) || (!end1 && a[one].is_ascii_alphabetic()) {
        Less
    } else {
        Greater
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering() {
        let cases = [
            ("1.0", "1.0", Equal),
            ("1.0", "1.1", Less),
            ("1.0-1", "1.0-2", Less),
            ("1.0", "1.0-5", Equal),
            ("1:0.1", "2.0", Greater),
            ("1.0a", "1.0", Less),
            ("1.0", "1.0.1", Less),
            ("1.0.a", "1.0.1", Less),
            ("1.10", "1.9", Greater),
            ("001", "1", Equal),
            ("r120.abc-1", "r99.zzz-1", Greater),
            ("2.0rc1", "2.0", Less),
            ("1.0..1", "1.0.1", Greater),
        ];
        for (a, b, want) in cases {
            assert_eq!(vercmp(a, b), want, "{a} vs {b}");
            assert_eq!(vercmp(b, a), want.reverse(), "{b} vs {a}");
        }
    }
}
