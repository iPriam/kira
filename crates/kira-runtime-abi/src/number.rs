//! The exact base-10 decimal behind `Number`.
//!
//! One `i64` mantissa and a decimal scale: the value is `mantissa * 10^-scale`.
//! `0.1` is `{ mantissa: 1, scale: 1 }`, `0.10` is `{ mantissa: 10, scale: 2 }`,
//! and the two compare equal because equality is numeric, not representational.
//!
//! The mantissa is 64 bits on purpose. Holding, comparing and summing decimals
//! at a shared scale are the common operations, and those stay single hardware
//! instructions and half the memory of an `i128`. Only multiplication and
//! division, which have to widen by construction, reach for a 128-bit
//! intermediate, and they narrow the result back to 64 bits or trap. The cost is
//! range: about eighteen significant digits, which is exact for money and most
//! else, rather than the thirty-eight an `i128` would carry.
//!
//! The VM and the native runtime both perform `Number` arithmetic by calling
//! this type, so the two cannot disagree: parity is a property of there being
//! one implementation rather than of a test comparing two. Every operation that
//! can leave range or divide by zero answers a typed error rather than
//! panicking, so a backend turns it into a trap with a message.

use core::cmp::Ordering;

/// The largest number of fractional digits a `Number` keeps.
///
/// Multiplication can grow the scale without bound and division has no exact
/// answer at any finite scale, so both round to this. Eighteen is what an `i64`
/// mantissa can hold as a pure fraction; a quotient whose integer part needs
/// more than the remaining digits overflows and traps.
pub const MAX_SCALE: u32 = 18;

/// Why a `Number` operation had no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecimalError {
    /// The result did not fit the `i64` mantissa.
    Overflow,
    /// A division whose divisor was zero.
    DivideByZero,
    /// Text that did not read as a decimal.
    Parse,
}

impl DecimalError {
    /// The trap message a backend prints for this failure.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            DecimalError::Overflow => "Number overflowed its 64-bit mantissa",
            DecimalError::DivideByZero => "Number divided by zero",
            DecimalError::Parse => "text does not read as a Number",
        }
    }
}

/// An exact base-10 decimal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decimal {
    mantissa: i64,
    scale: u32,
}

/// Ten to the `power` as an `i128`, or `None` when it overflows one.
fn pow10(power: u32) -> Option<i128> {
    let mut value: i128 = 1;
    for _ in 0..power {
        value = value.checked_mul(10)?;
    }
    Some(value)
}

/// A 128-bit result narrowed to the `i64` mantissa, or `Overflow`.
fn narrow(mantissa: i128, scale: u32) -> Result<Decimal, DecimalError> {
    let mantissa = i64::try_from(mantissa).map_err(|_| DecimalError::Overflow)?;
    Ok(Decimal { mantissa, scale })
}

/// A 128-bit quotient narrowed to the `i64` mantissa, dropping fractional digits
/// to make it fit rather than trapping.
///
/// A division always lands at `MAX_SCALE`, and a quotient with a large integer
/// part needs more than the sixty-four bits leave for that many fractional
/// digits — `100 / 4` is `25`, which does not fit at eighteen places. Rounding
/// the scale down until it fits is what keeps a plain division of two small
/// numbers from overflowing; only a genuinely huge integer part, past what the
/// mantissa holds at scale zero, still traps.
fn narrow_quotient(mut mantissa: i128, mut scale: u32) -> Result<Decimal, DecimalError> {
    while i64::try_from(mantissa).is_err() && scale > 0 {
        mantissa = round_i128_to_scale(mantissa, scale, scale - 1)?;
        scale -= 1;
    }
    narrow(mantissa, scale)
}

/// Rounds a truncated division `(quotient, remainder)` half-to-even.
///
/// `(quotient, remainder)` must be the truncation toward zero:
/// `numerator == quotient * denominator + remainder`, the remainder taking the
/// numerator's sign and staying smaller than the denominator. A tie
/// (`remainder * 2 == denominator`) goes to the even neighbour so a long series
/// of divisions does not drift.
fn round_quotient_half_even(
    quotient: i128,
    remainder: i128,
    numerator: i128,
    denominator: i128,
) -> Result<i128, DecimalError> {
    if remainder == 0 {
        return Ok(quotient);
    }
    let twice = remainder
        .unsigned_abs()
        .checked_mul(2)
        .ok_or(DecimalError::Overflow)?;
    let round_away = match twice.cmp(&denominator.unsigned_abs()) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => quotient % 2 != 0,
    };
    if round_away {
        let sign: i128 = if (numerator < 0) ^ (denominator < 0) {
            -1
        } else {
            1
        };
        quotient.checked_add(sign).ok_or(DecimalError::Overflow)
    } else {
        Ok(quotient)
    }
}

/// `numerator / denominator`, rounded half-to-even, by exact integer division.
///
/// The reference every faster path is checked against, and the fallback the
/// hinted path takes when a float's estimate is too far off to correct cheaply.
fn divide_round_half_even(numerator: i128, denominator: i128) -> Result<i128, DecimalError> {
    round_quotient_half_even(
        numerator / denominator,
        numerator % denominator,
        numerator,
        denominator,
    )
}

/// The same quotient, reached through an `f64` estimate rather than a 128-bit
/// division, or `None` when the estimate is too far to correct within budget.
///
/// A 128-bit divide is a software routine; a float divide is one instruction and
/// a multiply-back to check it is a few more. The estimate is corrected to the
/// exact truncated division by a bounded number of unit steps — enough for the
/// estimate to be right for a quotient a float can hold to full width, and a
/// fall back to the exact divide for one it cannot. Every path that does not
/// reach a valid truncated division answers `None`, so a wrong estimate is never
/// a wrong answer, only the exact path taken instead.
fn divide_round_half_even_hinted(numerator: i128, denominator: i128) -> Option<i128> {
    let estimate = numerator as f64 / denominator as f64;
    if !estimate.is_finite() {
        return None;
    }
    let rounded = estimate.round();
    // Beyond a float's exact-integer range the estimate's low digits are noise,
    // which the unit-step correction below cannot close in a bounded number of
    // steps — so this is where the exact path takes over.
    if !rounded.is_finite() || rounded.abs() >= 9.007e15 {
        return None;
    }
    let mut quotient = rounded as i128;
    let mut remainder = numerator.checked_sub(quotient.checked_mul(denominator)?)?;
    let denominator_magnitude = denominator.unsigned_abs();
    let mut budget = 4;
    loop {
        let valid = remainder.unsigned_abs() < denominator_magnitude
            && (remainder == 0 || (remainder < 0) == (numerator < 0));
        if valid {
            break;
        }
        if budget == 0 {
            return None;
        }
        budget -= 1;
        // Keep `numerator == quotient * denominator + remainder` as the unit
        // step moves the pair toward the truncated division.
        if (remainder > 0) == (denominator > 0) {
            quotient = quotient.checked_add(1)?;
            remainder -= denominator;
        } else {
            quotient = quotient.checked_sub(1)?;
            remainder += denominator;
        }
    }
    round_quotient_half_even(quotient, remainder, numerator, denominator).ok()
}

impl Decimal {
    /// The decimal `0`.
    #[must_use]
    pub const fn zero() -> Self {
        Decimal {
            mantissa: 0,
            scale: 0,
        }
    }

    /// An integer as an exact decimal.
    #[must_use]
    pub const fn from_i64(value: i64) -> Self {
        Decimal {
            mantissa: value,
            scale: 0,
        }
    }

    /// The raw mantissa, for a caller that stores the value.
    #[must_use]
    pub const fn mantissa(self) -> i64 {
        self.mantissa
    }

    /// The raw scale, for a caller that stores the value.
    #[must_use]
    pub const fn scale(self) -> u32 {
        self.scale
    }

    /// Rebuilds a decimal from a stored mantissa and scale.
    #[must_use]
    pub const fn from_parts(mantissa: i64, scale: u32) -> Self {
        Decimal { mantissa, scale }
    }

    /// The mantissa as a 128-bit value, restated at `target` scale. Only ever
    /// used to *raise* a scale, which is exact; the factor and product are
    /// 128-bit so an alignment never overflows for an in-range operand.
    fn wide_at(self, target: u32) -> Option<i128> {
        debug_assert!(target >= self.scale);
        let factor = pow10(target - self.scale)?;
        i128::from(self.mantissa).checked_mul(factor)
    }

    /// The two mantissas once both are at the same scale, as 128-bit values so a
    /// comparison or difference cannot overflow, plus that common scale.
    fn align(self, other: Self) -> Result<(i128, i128, u32), DecimalError> {
        let scale = self.scale.max(other.scale);
        let left = self.wide_at(scale).ok_or(DecimalError::Overflow)?;
        let right = other.wide_at(scale).ok_or(DecimalError::Overflow)?;
        Ok((left, right, scale))
    }

    /// Parses a decimal string: an optional sign, digits, an optional `.` and
    /// more digits. No exponent, no thousands separators — a `Number` literal is
    /// exact digits, and anything else is a `Parse` error rather than a guess.
    pub fn parse(text: &str) -> Result<Self, DecimalError> {
        let text = text.trim();
        let (negative, rest) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        if rest.is_empty() {
            return Err(DecimalError::Parse);
        }
        let (integer, fraction) = match rest.split_once('.') {
            Some((integer, fraction)) => (integer, fraction),
            None => (rest, ""),
        };
        if integer.is_empty() && fraction.is_empty() {
            return Err(DecimalError::Parse);
        }
        let mut mantissa: i64 = 0;
        for byte in integer.bytes().chain(fraction.bytes()) {
            if !byte.is_ascii_digit() {
                return Err(DecimalError::Parse);
            }
            mantissa = mantissa
                .checked_mul(10)
                .and_then(|value| value.checked_add(i64::from(byte - b'0')))
                .ok_or(DecimalError::Overflow)?;
        }
        let scale = u32::try_from(fraction.len()).map_err(|_| DecimalError::Overflow)?;
        if scale > MAX_SCALE {
            return Err(DecimalError::Parse);
        }
        if negative {
            mantissa = -mantissa;
        }
        Ok(Decimal { mantissa, scale })
    }

    /// The shortest exact decimal text for this value: a leading `-` when
    /// negative, the integer digits, and a `.` with the fractional digits when
    /// the scale is not zero. `1.0` and `1.00` both print at their own scale.
    #[must_use]
    pub fn to_decimal_string(self) -> String {
        if self.scale == 0 {
            return self.mantissa.to_string();
        }
        let negative = self.mantissa < 0;
        let digits = self.mantissa.unsigned_abs().to_string();
        let scale = self.scale as usize;
        let mut out = String::new();
        if negative {
            out.push('-');
        }
        if digits.len() > scale {
            let point = digits.len() - scale;
            out.push_str(&digits[..point]);
            out.push('.');
            out.push_str(&digits[point..]);
        } else {
            out.push_str("0.");
            for _ in 0..(scale - digits.len()) {
                out.push('0');
            }
            out.push_str(&digits);
        }
        out
    }

    /// Sum, exact. Same-scale operands take the 64-bit fast path; mixed scales
    /// align through 128 bits and narrow back.
    pub fn add(self, other: Self) -> Result<Self, DecimalError> {
        if self.scale == other.scale {
            let mantissa = self
                .mantissa
                .checked_add(other.mantissa)
                .ok_or(DecimalError::Overflow)?;
            return Ok(Decimal {
                mantissa,
                scale: self.scale,
            });
        }
        let (left, right, scale) = self.align(other)?;
        narrow(left.checked_add(right).ok_or(DecimalError::Overflow)?, scale)
    }

    /// Difference, exact.
    pub fn subtract(self, other: Self) -> Result<Self, DecimalError> {
        if self.scale == other.scale {
            let mantissa = self
                .mantissa
                .checked_sub(other.mantissa)
                .ok_or(DecimalError::Overflow)?;
            return Ok(Decimal {
                mantissa,
                scale: self.scale,
            });
        }
        let (left, right, scale) = self.align(other)?;
        narrow(left.checked_sub(right).ok_or(DecimalError::Overflow)?, scale)
    }

    /// Product, exact up to `MAX_SCALE`, half-to-even beyond it.
    ///
    /// The common case — two mantissas whose product fits an `i64` and whose
    /// combined scale is already within `MAX_SCALE` — is one 64-bit multiply and
    /// nothing wider. Only a product that overflows the `i64` or a combined scale
    /// past `MAX_SCALE` reaches for the 128-bit path, where the overflow may
    /// still round back into range.
    pub fn multiply(self, other: Self) -> Result<Self, DecimalError> {
        let scale = self.scale + other.scale;
        if scale <= MAX_SCALE
            && let Some(mantissa) = self.mantissa.checked_mul(other.mantissa)
        {
            return Ok(Decimal { mantissa, scale });
        }
        let wide = i128::from(self.mantissa) * i128::from(other.mantissa);
        let target = scale.min(MAX_SCALE);
        narrow(round_i128_to_scale(wide, scale, target)?, target)
    }

    /// Negation.
    #[must_use]
    pub fn negate(self) -> Self {
        Decimal {
            mantissa: self.mantissa.wrapping_neg(),
            scale: self.scale,
        }
    }

    /// Quotient rounded half-to-even at `MAX_SCALE`, in 128-bit intermediates.
    pub fn divide(self, other: Self) -> Result<Self, DecimalError> {
        if other.mantissa == 0 {
            return Err(DecimalError::DivideByZero);
        }
        // value * 10^MAX_SCALE = (self.mantissa / other.mantissa) * 10^p.
        let p = MAX_SCALE as i64 + other.scale as i64 - self.scale as i64;
        let self_mantissa = i128::from(self.mantissa);
        let other_mantissa = i128::from(other.mantissa);
        let (numerator, denominator) = if p >= 0 {
            let factor = pow10(u32::try_from(p).map_err(|_| DecimalError::Overflow)?)
                .ok_or(DecimalError::Overflow)?;
            (
                self_mantissa
                    .checked_mul(factor)
                    .ok_or(DecimalError::Overflow)?,
                other_mantissa,
            )
        } else {
            let factor = pow10(u32::try_from(-p).map_err(|_| DecimalError::Overflow)?)
                .ok_or(DecimalError::Overflow)?;
            (
                self_mantissa,
                other_mantissa
                    .checked_mul(factor)
                    .ok_or(DecimalError::Overflow)?,
            )
        };
        let mantissa = match divide_round_half_even_hinted(numerator, denominator) {
            Some(mantissa) => mantissa,
            None => divide_round_half_even(numerator, denominator)?,
        };
        narrow_quotient(mantissa, MAX_SCALE)
    }

    /// This value rounded to `target` fractional digits, half-to-even.
    pub fn round_to_scale(self, target: u32) -> Result<Self, DecimalError> {
        narrow(
            round_i128_to_scale(i128::from(self.mantissa), self.scale, target)?,
            target,
        )
    }

    /// The numeric ordering, so `1.0` and `1.00` compare `Equal`.
    pub fn compare(self, other: Self) -> Result<Ordering, DecimalError> {
        let (left, right, _) = self.align(other)?;
        Ok(left.cmp(&right))
    }

    /// Whether the two are numerically equal.
    pub fn equals(self, other: Self) -> Result<bool, DecimalError> {
        Ok(self.compare(other)? == Ordering::Equal)
    }

    /// The integer part, truncated toward zero.
    pub fn to_i64(self) -> Result<i64, DecimalError> {
        if self.scale == 0 {
            return Ok(self.mantissa);
        }
        let divisor = pow10(self.scale).ok_or(DecimalError::Overflow)?;
        i64::try_from(i128::from(self.mantissa) / divisor).map_err(|_| DecimalError::Overflow)
    }

    /// The nearest `f64`, for a conversion that is allowed to be lossy.
    #[must_use]
    pub fn to_f64(self) -> f64 {
        self.mantissa as f64 / 10f64.powi(self.scale as i32)
    }

    /// The `Number` nearest an `f64`, via its shortest decimal rendering so the
    /// scale is the one a person reading the float would expect.
    pub fn from_f64(value: f64) -> Result<Self, DecimalError> {
        if !value.is_finite() {
            return Err(DecimalError::Parse);
        }
        Self::parse(&format!("{value}"))
    }
}

/// A 128-bit mantissa at `scale` rounded to `target` fractional digits,
/// half-to-even. Raising the scale is exact; lowering drops low digits and
/// rounds by the dropped remainder, a tie going to the even neighbour.
fn round_i128_to_scale(mantissa: i128, scale: u32, target: u32) -> Result<i128, DecimalError> {
    if target >= scale {
        let factor = pow10(target - scale).ok_or(DecimalError::Overflow)?;
        return mantissa.checked_mul(factor).ok_or(DecimalError::Overflow);
    }
    let divisor = pow10(scale - target).ok_or(DecimalError::Overflow)?;
    let quotient = mantissa / divisor;
    let remainder = (mantissa % divisor).abs();
    let half = divisor / 2;
    let round_up = match remainder.cmp(&half) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => quotient % 2 != 0,
    };
    if round_up {
        let sign: i128 = if mantissa < 0 { -1 } else { 1 };
        quotient.checked_add(sign).ok_or(DecimalError::Overflow)
    } else {
        Ok(quotient)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        Decimal::parse(text).expect("a decimal")
    }

    #[test]
    fn the_tenths_add_exactly() {
        // The whole reason the type exists: no binary-float 0.30000000000000004.
        assert_eq!(dec("0.1").add(dec("0.2")).unwrap(), dec("0.3"));
        assert_eq!(dec("0.1").add(dec("0.2")).unwrap().to_decimal_string(), "0.3");
    }

    #[test]
    fn equality_is_numeric_not_representational() {
        assert!(dec("1.0").equals(dec("1.00")).unwrap());
        assert!(dec("1.0").equals(dec("1")).unwrap());
        assert!(!dec("1.0").equals(dec("1.01")).unwrap());
        assert_eq!(dec("1.0"), Decimal::from_parts(10, 1));
        assert_ne!(dec("1.0"), dec("1.00"), "the stored forms still differ");
    }

    #[test]
    fn multiply_and_subtract_are_exact() {
        assert_eq!(dec("1.5").multiply(dec("1.5")).unwrap(), dec("2.25"));
        assert_eq!(dec("0.3").subtract(dec("0.1")).unwrap(), dec("0.2"));
        assert_eq!(dec("2").multiply(dec("-3.5")).unwrap(), dec("-7.0"));
    }

    #[test]
    fn division_rounds_half_to_even_at_max_scale() {
        let third = dec("1").divide(dec("3")).unwrap();
        assert_eq!(third.scale(), MAX_SCALE);
        assert!(third.to_decimal_string().starts_with("0.3333333333"));
        assert_eq!(dec("1").divide(dec("4")).unwrap(), dec("0.25").round_to_scale(MAX_SCALE).unwrap());
        assert_eq!(dec("2.5").round_to_scale(0).unwrap(), dec("2"));
        assert_eq!(dec("3.5").round_to_scale(0).unwrap(), dec("4"));
        assert_eq!(dec("-2.5").round_to_scale(0).unwrap(), dec("-2"));
    }

    #[test]
    fn out_of_range_is_an_error_not_a_panic() {
        assert_eq!(dec("1").divide(dec("0")), Err(DecimalError::DivideByZero));
        let big = Decimal::from_parts(i64::MAX, 0);
        assert_eq!(big.add(big), Err(DecimalError::Overflow));
        // A product past the 64-bit mantissa traps rather than wrapping.
        let wide = Decimal::from_parts(3_037_000_500, 0);
        assert_eq!(wide.multiply(wide), Err(DecimalError::Overflow));
    }

    #[test]
    fn parse_rejects_what_is_not_a_decimal() {
        assert_eq!(Decimal::parse("abc"), Err(DecimalError::Parse));
        assert_eq!(Decimal::parse(""), Err(DecimalError::Parse));
        assert_eq!(Decimal::parse("1.2.3"), Err(DecimalError::Parse));
        assert_eq!(dec("-0.05").to_decimal_string(), "-0.05");
        assert_eq!(dec("42").to_decimal_string(), "42");
    }

    #[test]
    fn to_int_truncates_toward_zero() {
        assert_eq!(dec("3.9").to_i64().unwrap(), 3);
        assert_eq!(dec("-3.9").to_i64().unwrap(), -3);
    }

    #[test]
    fn the_hinted_divide_agrees_with_the_exact_divide() {
        // The float-hinted quotient must equal the exact 128-bit one wherever it
        // answers at all — its `None` is a fall back to the exact path, never a
        // different result. A wide sample of both signs so the fast path and its
        // correction and its fallback all fire.
        let denominators = [
            1_i128, 2, 3, 6, 7, 10, 16, 99, 100, 128, 9973, 1_000_003,
            i128::from(i64::MAX),
        ];
        let numerators = [
            0_i128, 1, 2, 5, 9, 10, 49, 50, 51, 149, 150, 151, 999, 1000, 1001,
            123_456_789, 9_007_199_254_740_993, i128::from(i64::MAX),
        ];
        let mut fast_paths = 0_u32;
        for &magnitude in &denominators {
            for &denominator in &[magnitude, -magnitude] {
                for &value in &numerators {
                    for &numerator in &[value, -value] {
                        let exact = divide_round_half_even(numerator, denominator).unwrap();
                        if let Some(hinted) =
                            divide_round_half_even_hinted(numerator, denominator)
                        {
                            assert_eq!(
                                hinted, exact,
                                "hinted != exact for {numerator} / {denominator}"
                            );
                            fast_paths += 1;
                        }
                    }
                }
            }
        }
        // The fast path must actually be taken, or the test proves nothing.
        assert!(fast_paths > 100, "the hinted path never fired ({fast_paths})");
    }

    #[test]
    fn division_results_are_unchanged_by_the_hint() {
        // The observable `divide` answers, through whichever path, are the ones
        // the exact algorithm gave before the hint existed.
        assert!(dec("1").divide(dec("8")).unwrap().equals(dec("0.125")).unwrap());
        assert_eq!(&dec("22").divide(dec("7")).unwrap().to_decimal_string()[..12], "3.1428571428");
        assert_eq!(dec("-1").divide(dec("3")).unwrap(), dec("1").divide(dec("3")).unwrap().negate());
        // A quotient with a large integer part reduces its scale to fit rather
        // than trapping: 100 / 4 is 25.
        assert!(dec("100").divide(dec("4")).unwrap().equals(dec("25")).unwrap());
        assert!(dec("1000000000").divide(dec("2")).unwrap().equals(dec("500000000")).unwrap());
    }
}
