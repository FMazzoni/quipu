//! Single canonical UTC-timestamp source.
//!
//! All RFC3339 strings the rest of the crate emits must come from
//! `now_rfc3339()`. Tested for the `Z` UTC suffix.

use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc().format(&Rfc3339).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_ends_with_z() {
        let s = now_rfc3339();
        assert!(s.ends_with('Z'), "expected UTC `Z` suffix, got: {s}");
        // Round-trip parse to confirm the string is valid RFC3339.
        OffsetDateTime::parse(&s, &Rfc3339).expect("must round-trip");
    }
}
