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
    fn string_output_is_shortest_exact() {
        assert_eq!(dec("1.00").to_decimal_string(), "1");
        assert_eq!(dec("2.50").to_decimal_string(), "2.5");
        assert_eq!(dec("-1.000").to_decimal_string(), "-1");
        assert_eq!(dec("0.30").to_decimal_string(), "0.3");
        assert_eq!(dec("100").to_decimal_string(), "100");
    }

    #[test]
    fn mixed_scale_sum_keeps_a_representable_result() {
        // Aligning "10" to eighteen places is 10^19, past the mantissa, but the
        // value 10 fits — shedding the trailing zeros lands it rather than trapping.
        let ten = dec("10").add(dec("0.000000000000000000")).unwrap();
        assert!(ten.equals(dec("10")).unwrap());
        let also = dec("10").subtract(dec("0.000000000000000000")).unwrap();
        assert!(also.equals(dec("10")).unwrap());
    }

    #[test]
    fn the_representable_minimum_round_trips_and_negation_traps() {
        let min = dec("-9223372036854775808");
        assert_eq!(min, Decimal::from_parts(i64::MIN, 0));
        assert_eq!(min.negate(), Err(DecimalError::Overflow));
        assert_eq!(dec("5").negate().unwrap(), dec("-5"));
        // Its positive magnitude is genuinely out of range.
        assert_eq!(Decimal::parse("9223372036854775808"), Err(DecimalError::Overflow));
    }

    #[test]
    fn division_rounds_once_at_the_fitting_scale() {
        // Rounding at MAX_SCALE and then again to fit drifts the last digit; a
        // single rounding of the exact rational at scale seventeen keeps it.
        let a = Decimal::from_parts(7_500_000_000_000_000_004, 0);
        let b = Decimal::from_parts(750_000_000_000_000_000, 0);
        assert_eq!(a.divide(b).unwrap().to_decimal_string(), "10.00000000000000001");
    }

    #[test]
    fn from_float_captures_the_float_not_its_shortest_text() {
        // 0.1_f64 is 0.1000000000000000055…, distinct from the exact tenth.
        let from_float = Decimal::from_f64(0.1).unwrap();
        assert!(!from_float.equals(dec("0.1")).unwrap());
        assert_eq!(from_float.to_decimal_string(), "0.100000000000000006");
        // A whole float still reads exactly.
        assert!(Decimal::from_f64(2.0).unwrap().equals(dec("2")).unwrap());
    }

    #[test]
    fn division_results_are_unchanged_by_the_hint() {
        // The observable `divide` answers, through whichever path, are the ones
        // the exact algorithm gave before the hint existed.
        assert!(dec("1").divide(dec("8")).unwrap().equals(dec("0.125")).unwrap());
        assert_eq!(&dec("22").divide(dec("7")).unwrap().to_decimal_string()[..12], "3.1428571428");
        assert_eq!(dec("-1").divide(dec("3")).unwrap(), dec("1").divide(dec("3")).unwrap().negate().unwrap());
        // A quotient with a large integer part reduces its scale to fit rather
        // than trapping: 100 / 4 is 25.
        assert!(dec("100").divide(dec("4")).unwrap().equals(dec("25")).unwrap());
        assert!(dec("1000000000").divide(dec("2")).unwrap().equals(dec("500000000")).unwrap());
    }
