//! Single canonical UTC-timestamp source.
//!
#![doc = include_str!("../docs/modules/time.md")]

use time::{format_description::well_known::Rfc3339, OffsetDateTime, PrimitiveDateTime};

pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc().format(&Rfc3339).unwrap()
}

/// This machine's clock, as an instant rather than a string.
///
/// The only caller that needs one is the clock-skew diagnosis, which subtracts
/// two instants; formatting to RFC 3339 and parsing back would be the same
/// value with a round trip in the middle.
pub fn now_utc() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

/// Parses an HTTP `Date` header (RFC 9110 IMF-fixdate), or `None`.
///
/// Spelled out as a literal format rather than handed to the RFC 2822 parser:
/// IMF-fixdate is a fixed-width subset with a hard-coded `GMT`, and the two
/// obsolete forms RFC 2822 additionally allows are forms no HTTP server emits.
/// A `None` here is not a failure — a server that sends no usable `Date` simply
/// leaves the clock unchecked, which must never be reported as a skewed clock.
pub fn parse_http_date(s: &str) -> Option<OffsetDateTime> {
    let fmt = time::macros::format_description!(
        "[weekday repr:short], [day] [month repr:short] [year] [hour]:[minute]:[second] GMT"
    );
    PrimitiveDateTime::parse(s.trim(), fmt)
        .ok()
        .map(PrimitiveDateTime::assume_utc)
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

    #[test]
    fn an_http_date_header_parses_to_the_instant_it_names() {
        let d = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").expect("IMF-fixdate parses");
        assert_eq!(d.unix_timestamp(), 784_111_777);
        // Leading and trailing space is what a header value arrives with.
        assert!(parse_http_date(" Sun, 06 Nov 1994 08:49:37 GMT ").is_some());
    }

    #[test]
    fn an_unparseable_date_is_none_rather_than_a_wrong_instant() {
        assert!(parse_http_date("").is_none());
        assert!(parse_http_date("yesterday").is_none());
        // RFC 3339 is not IMF-fixdate; guessing at it would invent a skew.
        assert!(parse_http_date("1994-11-06T08:49:37Z").is_none());
    }
}
