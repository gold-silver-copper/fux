//! Cases from the Forth 2012 core tests (John Hayes' `tester.fr`, as
//! `T{ 1 2 + -> 3 }T`, Annex F) and Exercism's Forth exercise, in fuxth's
//! words, translated through the plan's table: a program and what it
//! leaves, or the error it ends with. Where fuxth chose otherwise (§2 of
//! the plan: typed cells), the case says so.
mod common;
use common::{Ended, Toy, V, fuxth};
use fuxth::{Machine, Outcome, STDLIB, Value, Vocabulary, token};

fn ints(values: &[i64]) -> Ended {
    Ended::Left(Vec::new(), values.iter().map(|n| V::Int(*n)).collect())
}

fn bools(values: &[bool]) -> Ended {
    Ended::Left(Vec::new(), values.iter().map(|b| V::Bool(*b)).collect())
}

fn mixed(values: &[V]) -> Ended {
    Ended::Left(Vec::new(), values.to_vec())
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
        // MAX-INT 1+ -> MIN-INT: past 64 bits, it wraps around, as Forth's
        // two's-complement cells do.
        ("9223372036854775807 1 add", &[i64::MIN]),
        ("-9223372036854775808 1 sub", &[i64::MAX]),
        ("-9223372036854775808 -1 div", &[i64::MIN]),
        ("-9223372036854775808 negate", &[i64::MIN]),
        ("4294967296 4294967296 mul", &[0]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
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
    // It fails `type`, which `throw` numbers -12 (argument type mismatch),
    // not the plan's -24 (invalid numeric argument): that is `range`'s.
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
        (
            "define gi4 dup 1 gt if dup 1 sub recurse mul then end 5 gi4",
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

/// The flags `if` and `until` take: Forth's are integers, and fuxth takes
/// an integer too, true unless it is 0; its comparisons give booleans.
#[test]
fn core_flags() {
    for (program, left) in [
        ("-1 if 1 else 2 then 0 if 3 else 4 then", &[1, 4][..]),
        ("5 begin 1 sub dup 0 eq until", &[0]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
    assert_eq!(fuxth("0 not 7 not", u64::MAX), bools(&[true, false]));
}

/// ' EXECUTE FIND IMMEDIATE LITERAL POSTPONE STATE [ ] :NONAME RECURSE
/// EXIT VALUE TO CHAR [CHAR], as `tick execute find immediate literal
/// postpone compiling stop-compiling start-compiling code … end recurse
/// leave-definition let to first`. Deviations: `find` gives true for an
/// immediate word too (Forth gives 1 or -1); `compiling` gives a boolean;
/// GI6 leaves its numbers counting down, as it has no return stack yet.
#[test]
fn core_dictionary_and_compiling() {
    let gt = "define gt1 123 end define gt2 tick gt1 end immediate
        define gt4 postpone gt1 end immediate define gt5 gt4 end
        define gt6 345 end immediate define gt7 postpone gt6 end
        define gt8 compiling end immediate ";
    for (program, left) in [
        // T{ ' GT1 EXECUTE -> 123 }T  T{ GT2 EXECUTE -> 123 }T
        ("tick gt1 execute", &[V::Int(123)][..]),
        ("gt2 execute", &[V::Int(123)]),
        // T{ GT1STRING FIND -> ' GT1 -1 }T  T{ GT2STRING FIND -> ' GT2 1 }T
        (
            "str gt1 find swap tick gt1 eq",
            &[V::Bool(true), V::Bool(true)],
        ),
        (
            "str gt2 find swap tick gt2 eq",
            &[V::Bool(true), V::Bool(true)],
        ),
        // T{ : GT3 GT2 LITERAL ; -> }T  T{ GT3 -> ' GT1 }T
        (
            "define gt3 gt2 literal end gt3 tick gt1 eq",
            &[V::Bool(true)],
        ),
        // T{ GT5 -> 123 }T  T{ GT7 -> 345 }T
        ("gt5", &[V::Int(123)]),
        ("gt7", &[V::Int(345)]),
        // T{ GT8 -> 0 }T  T{ : GT9 GT8 LITERAL ; -> }T  T{ GT9 0= -> <FALSE> }T
        (
            "gt8 define gt9 gt8 literal end gt9",
            &[V::Bool(false), V::Bool(true)],
        ),
        // T{ : GC5 [ 1 2 + ] LITERAL ; GC5 -> 3 }T
        (
            "define gc5 stop-compiling 1 2 add start-compiling literal end gc5",
            &[V::Int(3)],
        ),
        // T{ :NONAME 1234 ; EXECUTE -> 1234 }T
        ("code 1234 end execute", &[V::Int(1234)]),
        // T{ 0 GI6 -> 0 }T  T{ 4 GI6 -> 0 1 2 3 4 }T, counting down here.
        (
            "define gi6 dup if dup 1 sub recurse then end 0 gi6",
            &[V::Int(0)],
        ),
        (
            "define gi6 dup if dup 1 sub recurse then end 4 gi6",
            &[V::Int(4), V::Int(3), V::Int(2), V::Int(1), V::Int(0)],
        ),
        // T{ : GE1 1 EXIT 2 ; GE1 -> 1 }T
        ("define ge1 1 leave-definition 2 end ge1", &[V::Int(1)]),
        // T{ 111 VALUE V1 -> }T  T{ V1 -> 111 }T  T{ 222 TO V1 -> }T  T{ V1 -> 222 }T
        ("111 let v1 v1 222 to v1 v1", &[V::Int(111), V::Int(222)]),
        // T{ : VD2 TO V2 ; -> }T  T{ 333 VD2 -> }T  T{ V2 -> 333 }T
        ("222 let v2 define vd2 to v2 end 333 vd2 v2", &[V::Int(333)]),
        // T{ CHAR X -> 58 }T  T{ : GC1 [CHAR] X ; GC1 -> 58 }T
        (
            "str X first define gc1 str X first end gc1",
            &[V::Int(88), V::Int(88)],
        ),
    ] {
        let program = format!("{gt}{program}");
        assert_eq!(fuxth(&program, u64::MAX), mixed(left), "{program}");
    }
}

/// EVALUATE, where the host enables it: T{ GE1 EVALUATE -> 123 }T, and a
/// definition made by the text evaluated (T{ GE3 ... }T).
#[test]
fn core_evaluate() -> Result<(), fuxth::Error> {
    let mut words = Vocabulary::default();
    words.evaluate();
    let mut machine = Machine::new(words);
    machine.library("stdlib.fux", STDLIB, &mut Toy::default())?;
    let program = "define ge1 str 123 end immediate ge1 evaluate
        str 'define ge3 456 end' evaluate ge3
        define ge4 str 'ge1 evaluate' evaluate end ge4";
    machine.read(token::source("program", program)?, None);
    let ran = machine.run(&mut Toy::default(), u64::MAX)?;
    assert_eq!(ran, Outcome::Finished);
    let left = [123, 456, 123].map(Value::Int);
    assert_eq!(machine.stack(), left.as_slice());
    Ok(())
}

/// EMIT and TYPE, as `emit` and `write`: T{ 65 EMIT -> }T prints A.
#[test]
fn core_output() {
    let ended = fuxth("65 emit str BC write 10 emit str D print", u64::MAX);
    let printed = match ended {
        Ended::Left(printed, _) => printed.concat(),
        other @ (Ended::Failed(..) | Ended::Refused(..) | Ended::Endless) => format!("{other:?}"),
    };
    assert_eq!(printed, "ABC\nD\n");
}

/// Annex F's tests of the Exception word set: CATCH THROW ABORT ABORT", as
/// `try … catch … end`, `throw`, `abort` and `fail`. Deviations (§2 of the
/// plan): the handler gets one failure, its code and its number (here
/// `thrown 99` for 99), where CATCH gives the number, and 0 when nothing
/// is thrown; a `try` body starts with an empty stack, so the values it
/// works on come in by name, and a failure leaves the stack as it was at
/// `try`, its values too, where CATCH restores only its depth.
#[test]
fn exception_catch_and_throw() {
    let failed = |code: &str, number| V::Failure(code.to_owned(), Some(number));
    let defs = "define t1 9 end define t2 8 0 throw end define t3 7 8 9 99 throw end
        define t4 1 sub dup 0 gt if recurse else 999 throw -222 then end ";
    for (program, left) in [
        // T{ c1 -> 1 2 3 9 0 }T: no THROW, no 0 either.
        (
            "1 2 3 str all try t1 catch end",
            &[V::Int(1), V::Int(2), V::Int(3), V::Int(9)][..],
        ),
        // T{ c2 -> 1 2 8 0 }T: 0 THROW does nothing.
        (
            "1 2 str all try t2 catch end",
            &[V::Int(1), V::Int(2), V::Int(8)],
        ),
        // T{ c3 -> 1 2 99 }T: the stack as it was at CATCH.
        (
            "1 2 str all try t3 catch end",
            &[V::Int(1), V::Int(2), failed("thrown", 99)],
        ),
        // T{ c4 -> 3 4 5 0 999 -111 }T: the calls unwound; 10 comes in by
        // name, so nothing is left in its place.
        (
            "3 4 5 10 let n str all try n t4 catch end -111",
            &[
                V::Int(3),
                V::Int(4),
                V::Int(5),
                failed("thrown", 999),
                V::Int(-111),
            ],
        ),
        // T{ c5 -> 5 }T: the depth at CATCH, however much the body dropped.
        (
            "1 2 3 4 str all try 9999 throw catch drop end",
            &[1, 2, 3, 4].map(V::Int),
        ),
        // T{ 1 2 ' t6 c6 -> 1 2 11 }T: ABORT is -1 THROW.
        (
            "1 2 str all try abort catch end",
            &[V::Int(1), V::Int(2), failed("abort", -1)],
        ),
        // T{ 3 0 ' t10 c6 -> 3 77 }T  T{ 4 5 ' t10 c6 -> 4 77 12 }T: ABORT"
        // is -2 THROW, its message made as it runs.
        (
            "3 0 let flag str all try 77 flag if str 'not displayed' fail then catch end",
            &[V::Int(3), V::Int(77)],
        ),
        (
            "4 5 let flag str all try 77 flag if str 'not displayed' fail then catch end",
            &[V::Int(4), failed("fail", -2)],
        ),
        // QUIT is -56 THROW (9.3.5's table).
        ("str all try quit catch end", &[failed("quit", -56)]),
    ] {
        let program = format!("{defs}{program}");
        assert_eq!(fuxth(&program, u64::MAX), mixed(left), "{program}");
    }
}

/// T{ 6 7 ' t9 c6 3 -> 6 7 13 3 }T: an undefined word (-13) found by text
/// three EVALUATEs deep is caught, and the sources it was read from end.
#[test]
fn exception_unlinks_the_sources_read() -> Result<(), fuxth::Error> {
    let mut words = Vocabulary::default();
    words.evaluate();
    let mut machine = Machine::new(words);
    machine.library("stdlib.fux", STDLIB, &mut Toy::default())?;
    let program = "define t7 str '333 undefined-word 334' evaluate 335 end
        define t8 str '222 t7 223' evaluate 224 end
        define t9 str '111 112 t8 113' evaluate 114 end
        6 7 str all try t9 catch end 3";
    machine.read(token::source("program", program)?, None);
    let ran = machine.run(&mut Toy::default(), u64::MAX)?;
    assert_eq!(ran, Outcome::Finished);
    let left: Vec<V> = machine.stack().iter().map(common::value).collect();
    let undefined = V::Failure("unknown_word".to_owned(), Some(-13));
    assert_eq!(left, [V::Int(6), V::Int(7), undefined, V::Int(3)]);
    Ok(())
}

/// TUCK ?DUP 2SWAP 2OVER DEPTH >R R> R@, as `tuck dup-nonzero 2swap 2over
/// depth to-return from-return copy-return`. Deviation: `dup-nonzero`
/// takes a boolean too, false counting as 0.
#[test]
fn core_stack_and_return_stack() {
    for (program, left) in [
        // T{ 1 2 TUCK -> 2 1 2 }T
        ("1 2 tuck", &[2, 1, 2][..]),
        // T{ -1 ?DUP -> -1 -1 }T  T{ 0 ?DUP -> 0 }T  T{ 1 ?DUP -> 1 1 }T
        (
            "-1 dup-nonzero 0 dup-nonzero 1 dup-nonzero",
            &[-1, -1, 0, 1, 1],
        ),
        // T{ 1 2 3 4 2SWAP -> 3 4 1 2 }T  T{ 1 2 3 4 2OVER -> 1 2 3 4 1 2 }T
        ("1 2 3 4 2swap", &[3, 4, 1, 2]),
        ("1 2 3 4 2over", &[1, 2, 3, 4, 1, 2]),
        // T{ 0 1 DEPTH -> 0 1 2 }T  T{ 0 DEPTH -> 0 1 }T  T{ DEPTH -> 0 }T
        ("0 1 depth", &[0, 1, 2]),
        ("depth", &[0]),
        // T{ 123 GR1 -> 123 }T  T{ 123 GR2 -> 123 }T
        ("define gr1 to-return from-return end 123 gr1", &[123]),
        (
            "define gr2 to-return copy-return from-return drop end 123 gr2",
            &[123],
        ),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
    assert_eq!(fuxth("false dup-nonzero", u64::MAX), bools(&[false]));
    // Inside list … end, the stack starts empty, for depth too.
    let in_list = fuxth("1 2 list 3 depth end", u64::MAX);
    let list = V::List(vec![V::Int(3), V::Int(1)]);
    assert_eq!(in_list, mixed(&[V::Int(1), V::Int(2), list]));
    assert!(ends("stack_underflow")(&fuxth("from-return", u64::MAX)));
}

/// /MOD */ */MOD WITHIN AND OR XOR INVERT LSHIFT RSHIFT, as `div-mod
/// mul-div mul-div-mod within and or xor invert shift-left shift-right`.
/// Deviations: division truncates toward zero, as `div` does (SM/REM);
/// `and`, `or`, `xor` and `invert` are logical on booleans.
#[test]
fn core_arithmetic_and_bits() {
    for (program, left) in [
        // T{ 7 2 /MOD -> 1 3 }T, symmetric: the remainder takes the sign of
        // the dividend.
        ("7 2 div-mod -7 2 div-mod", &[1, 3, -1, -3][..]),
        // T{ MAX-INT 2 MAX-INT */ -> 2 }T: the product past 64 bits is kept.
        ("9223372036854775807 2 9223372036854775807 mul-div", &[2]),
        (
            "9223372036854775807 3 6 mul-div-mod",
            &[3, 4611686018427387903],
        ),
        ("-7 1 2 mul-div-mod", &[-1, -3]),
        // T{ 0 0 AND -> 0 }T  T{ 0 1 AND -> 0 }T  T{ 1 1 AND -> 1 }T  …
        ("0 1 and 1 1 and 0 1 or 0 0 or", &[0, 1, 1, 0]),
        ("12 10 and 12 10 or 12 10 xor", &[8, 14, 6]),
        // T{ 0S INVERT -> 1S }T  T{ 1S 1S XOR -> 0S }T
        ("0 invert -1 -1 xor", &[-1, 0]),
        // T{ 1 0 LSHIFT -> 1 }T  T{ 1 1 LSHIFT -> 2 }T  T{ 1 2 LSHIFT -> 4 }T
        // T{ 1S 1 LSHIFT 1 XOR -> 1S }T  T{ MSB 1 LSHIFT -> 0 }T
        ("1 0 shift-left 1 1 shift-left 1 2 shift-left", &[1, 2, 4]),
        ("-1 1 shift-left 1 xor", &[-1]),
        ("-9223372036854775808 1 shift-left", &[0]),
        // T{ 1 0 RSHIFT -> 1 }T  T{ 1 1 RSHIFT -> 0 }T  T{ 4 2 RSHIFT -> 1 }T
        // T{ MSB 1 RSHIFT MSB AND -> 0 }T: zeros come in at the top.
        (
            "1 0 shift-right 1 1 shift-right 4 2 shift-right",
            &[1, 0, 1],
        ),
        ("-9223372036854775808 1 shift-right", &[1 << 62]),
        ("1 64 shift-left 1 64 shift-right", &[0, 0]),
    ] {
        assert_eq!(fuxth(program, u64::MAX), ints(left), "{program}");
    }
    for (program, left) in [
        (
            "true false and true false or true false xor",
            &[false, true, true][..],
        ),
        ("true invert false invert", &[false, true]),
        // T{ 0 0 0 WITHIN -> <FALSE> }T  T{ 1 0 2 WITHIN -> <TRUE> }T …
        (
            "0 0 0 within 1 0 2 within 2 0 2 within -1 0 2 within",
            &[false, true, false, false],
        ),
        // With hi below lo, it goes round the end of the integers.
        ("1 10 2 within 5 10 2 within", &[true, false]),
        (
            "9223372036854775807 9223372036854775806 -9223372036854775808 within",
            &[true],
        ),
    ] {
        assert_eq!(fuxth(program, u64::MAX), bools(left), "{program}");
    }
    assert!(ends("type")(&fuxth("1 true and", u64::MAX)));
    assert!(ends("div_by_zero")(&fuxth("1 2 0 mul-div", u64::MAX)));
    assert!(ends("range")(&fuxth("1 -1 shift-left", u64::MAX)));
}

/// BEGIN WHILE REPEAT, AGAIN, DO ?DO LOOP +LOOP I J LEAVE UNLOOP, as
/// `begin … while … end`, `begin … again`, `do … loop`, `loop-by`, `i`,
/// `j`, `leave` and `leave-definition`. Deviations: `do` skips its body
/// when the limit equals the start, as `?do` does; `end` closes one
/// `while`, so GI5's two need an `if`; `unloop` has no job, since
/// `leave-definition` leaves the definition's loops.
#[test]
fn core_loops() {
    let gd = "define gd1 do i loop end define gd2 do i -1 loop-by end
        define gd3 do 1 0 do j loop loop end define gd4 do 1 0 do j loop -1 loop-by end
        define gd5 123 swap 0 do i 4 gt if drop 234 leave then loop end
        define gd6 0 swap 0 do i 1 add 0 do i j add 3 eq if i j leave-definition then 1 add loop loop end
        define gi3 begin dup 5 lt while dup 1 add end end
        define ag0 701 begin dup 7 mod 0 eq if leave-definition then 1 add again end ";
    for (program, left) in [
        // T{ 0 GI3 -> 0 1 2 3 4 5 }T  T{ 4 GI3 -> 4 5 }T  T{ 6 GI3 -> 6 }T
        ("0 gi3", &[0, 1, 2, 3, 4, 5][..]),
        ("4 gi3", &[4, 5]),
        ("6 gi3", &[6]),
        // T{ AG0 -> 707 }T
        ("ag0", &[707]),
        // T{ 4 1 GD1 -> 1 2 3 }T  T{ 2 -1 GD1 -> -1 0 1 }T
        ("4 1 gd1", &[1, 2, 3]),
        ("2 -1 gd1", &[-1, 0, 1]),
        // T{ MID-UINT+1 MID-UINT GD1 -> MID-UINT }T
        ("-9223372036854775808 9223372036854775807 gd1", &[i64::MAX]),
        // ?DO: none, the limit equal to the start.
        ("3 3 gd1", &[]),
        // T{ 1 4 GD2 -> 4 3 2 1 }T  T{ -1 2 GD2 -> 2 1 0 -1 }T
        ("1 4 gd2", &[4, 3, 2, 1]),
        ("-1 2 gd2", &[2, 1, 0, -1]),
        // T{ MID-UINT MID-UINT+1 GD2 -> MID-UINT+1 MID-UINT }T
        (
            "9223372036854775807 -9223372036854775808 gd2",
            &[i64::MIN, i64::MAX],
        ),
        // T{ 4 1 GD3 -> 1 2 3 }T  T{ 1 4 GD4 -> 4 3 2 1 }T
        ("4 1 gd3", &[1, 2, 3]),
        ("1 4 gd4", &[4, 3, 2, 1]),
        // T{ 1 GD5 -> 123 }T  T{ 5 GD5 -> 123 }T  T{ 6 GD5 -> 234 }T
        ("1 gd5 5 gd5 6 gd5", &[123, 123, 234]),
        // T{ 1 GD6 -> 1 }T  T{ 2 GD6 -> 3 }T  T{ 3 GD6 -> 4 1 2 }T
        ("1 gd6", &[1]),
        ("2 gd6", &[3]),
        ("3 gd6", &[4, 1, 2]),
        // +LOOP by more than one ends as it crosses the limit, either way.
        ("10 0 do i 3 loop-by", &[0, 3, 6, 9]),
        ("0 10 do i -3 loop-by", &[10, 7, 4, 1]),
    ] {
        let program = format!("{gd}{program}");
        assert_eq!(fuxth(&program, u64::MAX), ints(left), "{program}");
    }
    // I outside a loop, or in a word a loop calls, is no loop's index.
    assert!(ends("not_top_level")(&fuxth("i", u64::MAX)));
    let called = "define f i 1 add end 3 0 do f loop";
    assert!(ends("not_top_level")(&fuxth(called, u64::MAX)));
    assert!(ends("not_top_level")(&fuxth("3 0 do j loop", u64::MAX)));
}

/// CASE OF ENDOF ENDCASE (Core extensions), as `case … of … end … end`.
#[test]
fn core_case() {
    let cs =
        "define cs1 case 1 of 111 end 2 of 222 end 3 of 333 end to-return 999 from-return end end
        define cs2 to-return case
          -1 of case copy-return 1 of 100 end 2 of 200 end to-return -300 from-return end end
          -2 of case copy-return 1 of -99 end to-return -199 from-return end end
          to-return 299 from-return
        end from-return drop end ";
    for (program, left) in [
        // T{ 1 CS1 -> 111 }T … T{ 4 CS1 -> 999 }T
        ("1 cs1 2 cs1 3 cs1 4 cs1", &[111, 222, 333, 999][..]),
        // T{ -1 1 CS2 -> 100 }T … T{ 0 2 CS2 -> 299 }T
        ("-1 1 cs2 -1 2 cs2 -1 3 cs2", &[100, 200, -300]),
        ("-2 1 cs2 -2 2 cs2 0 2 cs2", &[-99, -199, 299]),
        // A case with no of, and one at the top level: the last end drops
        // the top value, x unless the part before it moved it.
        ("5 case 7 swap end", &[7]),
        ("2 case 1 of 10 end 2 of 20 end 30 end", &[20]),
    ] {
        let program = format!("{cs}{program}");
        assert_eq!(fuxth(&program, u64::MAX), ints(left), "{program}");
    }
}

/// AHEAD (Programming-Tools), and CS-ROLL, which is `swap` on the marks
/// of the constructs open: T{ : PT1 AHEAD 1111 2222 THEN 3333 ; -> }T
/// T{ PT1 -> 3333 }T.
#[test]
fn tools_ahead() {
    let pt = "define pt1 ahead 1111 2222 then 3333 end
        define my-else postpone ahead swap postpone then end immediate
        define pt2 if 1 my-else 2 then end ";
    for (program, left) in [("pt1", &[3333][..]), ("true pt2 false pt2", &[1, 2])] {
        let program = format!("{pt}{program}");
        assert_eq!(fuxth(&program, u64::MAX), ints(left), "{program}");
    }
}

/// <# # #S #> HOLD HOLDS SIGN, as `digits-begin digit digits digits-end
/// hold holds sign`. Deviations: the number is one integer, not a double;
/// `digit` takes a negative one's digits from its magnitude, so `sign`
/// needs no `abs` first; `digits-end` gives one string.
#[test]
fn core_number_text() {
    let text = |s: &str| V::Str(s.to_owned());
    for (program, left) in [
        // T{ GP1 -> <TRUE> }T: <# 41 HOLD 42 HOLD 0 0 #> S" BA" S=
        ("digits-begin 65 hold 66 hold 0 digits-end", text("BA")),
        // T{ GP2 -> <TRUE> }T: <# -1 SIGN 0 SIGN -1 SIGN 0 0 #> S" --" S=
        (
            "digits-begin -1 sign 0 sign -1 sign 0 digits-end",
            text("--"),
        ),
        // T{ GP3 -> <TRUE> }T: <# 1 0 # # #> S" 01" S=
        ("digits-begin 1 digit digit digits-end", text("01")),
        // T{ GP4 -> <TRUE> }T: <# 1 0 #S #> S" 1" S=
        ("digits-begin 1 digits digits-end", text("1")),
        // HOLDS: <# S" Test" HOLDS #>
        ("digits-begin str Test holds 0 digits-end", text("Test")),
        (
            "-123 dup digits-begin digits swap sign digits-end",
            text("-123"),
        ),
        (
            "-9223372036854775808 dup digits-begin digits swap sign digits-end",
            text("-9223372036854775808"),
        ),
    ] {
        assert_eq!(fuxth(program, u64::MAX), mixed(&[left]), "{program}");
    }
}
