//! Neutral facts about how much community review an AUR package has had.
//!
//! These are deliberately NOT a score and never a verdict: a package being
//! new, unpopular or orphaned is the normal state of most of the AUR and says
//! nothing about its intent. The signals only help you decide how carefully
//! to read the files yourself, because fewer other eyes have done so.
//! Nothing here ever blocks an install; only audit findings can do that.

use crate::rpc::AurPkg;
use crate::util::{dim, fmt_date, now};

/// Short tags such as "new: 12 days", "no votes", "orphan".
pub fn tags(p: &AurPkg) -> Vec<String> {
    let mut v = Vec::new();
    if p.first_submitted > 0 {
        let days = now().saturating_sub(p.first_submitted) / 86400;
        if days < 90 {
            v.push(format!("new: {days} days"));
        }
    }
    match p.num_votes {
        0 => v.push("no votes".into()),
        1..=4 => v.push(format!("{} votes", p.num_votes)),
        _ => {}
    }
    if p.maintainer.is_none() {
        v.push("orphan".into());
    }
    if let Some(ts) = p.out_of_date {
        v.push(format!("out-of-date since {}", fmt_date(ts)));
    }
    v
}

/// True when few people are likely to have read this package yet
/// (so a first review deserves reading every file, not skimming).
pub fn few_eyes(p: &AurPkg) -> bool {
    let days = if p.first_submitted > 0 {
        now().saturating_sub(p.first_submitted) / 86400
    } else {
        u64::MAX
    };
    days < 90 || p.num_votes < 5 || p.maintainer.is_none()
}

/// Inline rendering for package lists: dim, never colored as a warning.
pub fn inline(p: &AurPkg) -> String {
    let t = tags(p);
    if t.is_empty() {
        String::new()
    } else {
        dim(format!("  [{}]", t.join(", ")))
    }
}
