//! Cases from the Forth 2012 core tests (John Hayes' `tester.fr`, as
//! `T{ 1 2 + -> 3 }T`) and Exercism's Forth exercise, in fuxth's words: a
//! program and what it leaves, or the error it ends with. Where fuxth
//! chose otherwise, the case says so.
mod common;
use common::{Ended, V, fuxth};

fn ints(values: &[i64]) -> Ended {
    Ended::Left(Vec::new(), values.iter().map(|n| V::Int(*n)).collect())
}

fn bools(values: &[bool]) -> Ended {
    Ended::Left(Vec::new(), values.iter().map(|b| V::Bool(*b)).collect())
}

fn ends(code: &str) -> impl Fn(&Ended) -> bool + '_ {
    move |ended| matches!(ended, Ended::Failed(c, _) | Ended::Refused(c, _) if c == code)
}

#[test]
fn core_stack_words() {
    for (program, left) in [
        ("1 2 3 4 5", &[1, 2, 3, 4, 5][..]),
        ("-1 -2 -3 -4 -5", &[-1, -2, -3, -4, -5]),
        ("1 2 drop", &[1]),
        ("1 dup", &[1, 1]),
        ("1 2 over", &[1, 2, 1]),
        ("1 2 3 rot", &[2, 3, 1]),
        ("1 2 swap", &[2, 1]),
        ("1 2 nip", &[2]),
        ("1 2 3 -rot", &[3, 1, 2]),
        ("1 2 2dup", &[1, 2, 1, 2]),
        ("1 2 3 2drop", &[1]),
        ("1 2 swap over dup drop", &[2, 1, 2]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
}

#[test]
fn core_arithmetic() {
    for (program, left) in [
        ("0 5 add", &[5][..]),
        ("1 2 add 4 sub 3 mul 2 div", &[-1]),
        ("-1 1 add", &[0]),
        ("0 5 sub", &[-5]),
        ("-1 -1 sub", &[0]),
        ("2 3 mul", &[6]),
        ("-3 -3 mul", &[9]),
        ("10 3 sub 2 mul 5 div 7 3 mod", &[2, 1]),
        ("0 negate 1 negate -1 negate", &[0, -1, 1]),
        ("0 abs 1 abs -1 abs", &[0, 1, 1]),
        ("0 1 min 0 -1 min", &[0, -1]),
        ("0 1 max 0 -1 max", &[1, 0]),
        // Symmetric division (Forth's SM/REM): the quotient truncates toward
        // zero, and the remainder takes the dividend's sign. Forth leaves
        // `/` to each system; gforth's floors (-7 2 / is -4).
        ("7 2 div 7 2 mod", &[3, 1]),
        ("-7 2 div -7 2 mod", &[-3, -1]),
        ("7 -2 div 7 -2 mod", &[-3, 1]),
        ("-7 -2 div -7 -2 mod", &[3, -1]),
        ("-9223372036854775808 -1 mod 7 -1 mod", &[0, 0]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
    // Where Forth wraps around, fuxth says so.
    for program in [
        "9223372036854775807 1 add",
        "-9223372036854775808 -1 div",
        "-9223372036854775808 negate",
    ] {
        assert!(ends("overflow")(&fuxth(program, u64::MAX)), "{program}");
    }
    assert!(ends("div_by_zero")(&fuxth("1 0 div", u64::MAX)));
    assert!(ends("div_by_zero")(&fuxth("1 0 mod", u64::MAX)));
}

#[test]
fn core_comparisons() {
    // Booleans are their own kind, not -1 and 0.
    for (program, left) in [
        (
            "0 0 eq 1 1 eq -1 -1 eq 1 0 eq",
            &[true, true, true, false][..],
        ),
        (
            "0 1 lt 1 2 lt -1 0 lt 0 0 lt 1 0 lt",
            &[true, true, true, false, false],
        ),
        ("0 1 gt 1 0 gt 0 0 gt", &[false, true, false]),
        ("true not false not", &[false, true]),
        ("1 2 ne 2 2 le 3 2 ge", &[true, true, true]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), bools(left), "{program}");
    }
    // `0 0 eq 1 add` is -1 + 1 in Forth; a boolean isn't an integer here.
    assert!(ends("type")(&fuxth("0 0 eq 1 add", u64::MAX)));
    assert_eq!(fuxth("true 0 eq", u64::MAX), bools(&[false]));
}

#[test]
fn core_control_and_definitions() {
    for (program, left) in [
        ("define gi1 if 123 then end false gi1 true gi1", &[123][..]),
        (
            "define gi2 if 123 else 234 then end false gi2 true gi2",
            &[234, 123],
        ),
        // T{ : GI3 BEGIN DUP 5 < WHILE DUP 1+ REPEAT ; }T, as until.
        (
            "define gi3 begin dup 1 add dup 5 lt not until end 0 gi3",
            &[0, 1, 2, 3, 4, 5],
        ),
        // RECURSE is a word's own name here.
        (
            "define gi4 dup 1 gt if dup 1 sub gi4 mul then end 5 gi4",
            &[120],
        ),
        ("123 let x123 x123", &[123]),
        ("6 let six six six mul", &[36]),
        ("define nop end 1 nop", &[1]),
        ("define dup-twice dup dup end 1 dup-twice", &[1, 1, 1]),
        ("define countup 1 2 3 end countup", &[1, 2, 3]),
        ("define foo 5 end define bar foo end bar", &[5]),
        ("define swap dup end 1 swap", &[1, 1]),
        ("define twice dup add end 5 twice twice", &[20]),
        ("0 begin 1 add dup 5 eq until", &[5]),
        (
            "define countdown begin dup 1 sub dup 0 eq until end 3 countdown",
            &[3, 2, 1, 0],
        ),
        ("1 2 lt if 10 else 20 then", &[10]),
        ("2 1 lt if 10 else 20 then", &[20]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
}
