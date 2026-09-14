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

/// A 128-bit result narrowed to the `i64` mantissa, first shedding trailing
/// fractional zeros so an exact value that only overflows because of its scale
/// still fits. `10` aligned to scale eighteen is `10^19`, past the mantissa, yet
/// the value is `10`: dropping the zeros lands it at scale zero rather than
/// trapping. Removing a trailing zero is exact, so this never changes the value.
fn narrow_reduced(mut mantissa: i128, mut scale: u32) -> Result<Decimal, DecimalError> {
    while scale > 0 && mantissa % 10 == 0 {
        mantissa /= 10;
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
        // The magnitude accumulates in 128 bits and takes its sign before
        // narrowing, so the representable minimum — whose magnitude is one past
        // `i64::MAX` — reads as itself rather than overflowing on its last digit.
        let mut mantissa: i128 = 0;
        for byte in integer.bytes().chain(fraction.bytes()) {
            if !byte.is_ascii_digit() {
                return Err(DecimalError::Parse);
            }
            mantissa = mantissa
                .checked_mul(10)
                .and_then(|value| value.checked_add(i128::from(byte - b'0')))
                .ok_or(DecimalError::Overflow)?;
        }
        let scale = u32::try_from(fraction.len()).map_err(|_| DecimalError::Overflow)?;
        if scale > MAX_SCALE {
            return Err(DecimalError::Parse);
        }
        if negative {
            mantissa = -mantissa;
        }
        let mantissa = i64::try_from(mantissa).map_err(|_| DecimalError::Overflow)?;
        Ok(Decimal { mantissa, scale })
    }

    /// The mantissa and scale with trailing fractional zeros shed, the shortest
    /// representation of the same value. `1.00` (`{100, 2}`) becomes `{1, 0}`.
    fn without_trailing_zeros(self) -> (i64, u32) {
        let mut mantissa = self.mantissa;
        let mut scale = self.scale;
        while scale > 0 && mantissa % 10 == 0 {
            mantissa /= 10;
            scale -= 1;
        }
        (mantissa, scale)
    }

    /// The shortest exact decimal text for this value: a leading `-` when
    /// negative, the integer digits, and a `.` with the fractional digits when
    /// there are any. Trailing fractional zeros are dropped, so `1.0` and `1.00`
    /// both render as `1` — the shortest text that reads back to the same value.
    #[must_use]
    pub fn to_decimal_string(self) -> String {
        let (mantissa, scale) = self.without_trailing_zeros();
        if scale == 0 {
            return mantissa.to_string();
        }
        let negative = mantissa < 0;
        let digits = mantissa.unsigned_abs().to_string();
        let scale = scale as usize;
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
        narrow_reduced(left.checked_add(right).ok_or(DecimalError::Overflow)?, scale)
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
        narrow_reduced(left.checked_sub(right).ok_or(DecimalError::Overflow)?, scale)
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

    /// Negation, a trap when the value has no negative in range.
    ///
    /// The one value that traps is the representable minimum: its magnitude is
    /// `i64::MAX + 1`, so the positive it would negate to does not fit the
    /// mantissa. Every other value negates exactly.
    pub fn negate(self) -> Result<Self, DecimalError> {
        Ok(Decimal {
            mantissa: self.mantissa.checked_neg().ok_or(DecimalError::Overflow)?,
            scale: self.scale,
        })
    }

    /// Quotient rounded half-to-even, at the largest scale up to `MAX_SCALE` that
    /// its integer part leaves room for, in 128-bit intermediates.
    ///
    /// The scale is chosen before the single rounding, not by rounding at
    /// `MAX_SCALE` and rounding the result again to fit — a second rounding of an
    /// already-rounded mantissa drifts the last digit. Each candidate scale
    /// rounds the *exact* rational `self / other` once; the largest whose result
    /// fits the mantissa is the answer. A quotient whose integer part alone
    /// exceeds the mantissa, past scale zero, traps.
    pub fn divide(self, other: Self) -> Result<Self, DecimalError> {
        if other.mantissa == 0 {
            return Err(DecimalError::DivideByZero);
        }
        // value * 10^scale = self.mantissa * 10^(scale + other.scale - self.scale)
        //                    / other.mantissa.
        let shift = other.scale as i64 - self.scale as i64;
        let self_mantissa = i128::from(self.mantissa);
        let other_mantissa = i128::from(other.mantissa);
        let mut scale = MAX_SCALE;
        loop {
            let candidate = Self::quotient_at_scale(
                self_mantissa,
                other_mantissa,
                shift + scale as i64,
                scale,
            );
            if let Some(decimal) = candidate {
                return Ok(decimal);
            }
            if scale == 0 {
                return Err(DecimalError::Overflow);
            }
            scale -= 1;
        }
    }

    /// `self / other` rounded half-to-even to `scale` fractional digits, once,
    /// as a `Decimal` — or `None` when that scaling or the narrowed result does
    /// not fit, which asks the caller to try a smaller scale. `power` is
    /// `scale + other.scale - self.scale`, the exponent that lifts the exact
    /// rational to the working scale.
    fn quotient_at_scale(
        self_mantissa: i128,
        other_mantissa: i128,
        power: i64,
        scale: u32,
    ) -> Option<Self> {
        let (numerator, denominator) = if power >= 0 {
            let factor = pow10(u32::try_from(power).ok()?)?;
            (self_mantissa.checked_mul(factor)?, other_mantissa)
        } else {
            let factor = pow10(u32::try_from(-power).ok()?)?;
            (self_mantissa, other_mantissa.checked_mul(factor)?)
        };
        let mantissa = match divide_round_half_even_hinted(numerator, denominator) {
            Some(mantissa) => mantissa,
            None => divide_round_half_even(numerator, denominator).ok()?,
        };
        i64::try_from(mantissa)
            .ok()
            .map(|mantissa| Decimal { mantissa, scale })
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

    /// The `Number` nearest an `f64`: the float's actual binary value rounded to
    /// as many fractional digits as fit, not its shortest round-tripping text.
    ///
    /// `0.1_f64` is not a tenth — it is `0.1000000000000000055…` — so `Number`
    /// from it is `0.100000000000000006`, the documented "decimal nearest the
    /// float, which was already rounded", distinct from the exact `Number("0.1")`.
    /// Rendering to a fixed number of places asks the formatter for the true
    /// value at that precision; the widest precision whose digits fit the mantissa
    /// is the answer, and a value too large for even the integer part traps.
    pub fn from_f64(value: f64) -> Result<Self, DecimalError> {
        if !value.is_finite() {
            return Err(DecimalError::Parse);
        }
        for places in (0..=MAX_SCALE as usize).rev() {
            if let Ok(decimal) = Self::parse(&format!("{value:.places$}")) {
                return Ok(decimal);
            }
        }
        Err(DecimalError::Overflow)
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
mod tests;
