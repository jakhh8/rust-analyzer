use base_db::fixture::WithFixture;
use chalk_ir::Substitution;
use hir_def::db::DefDatabase;

use crate::{
    consteval::try_const_usize, db::HirDatabase, mir::pad16, test_db::TestDB, Const, ConstScalar,
    Interner,
};

use super::{
    super::mir::{MirEvalError, MirLowerError},
    ConstEvalError,
};

mod intrinsics;

fn simplify(e: ConstEvalError) -> ConstEvalError {
    match e {
        ConstEvalError::MirEvalError(MirEvalError::InFunction(_, e)) => {
            simplify(ConstEvalError::MirEvalError(*e))
        }
        _ => e,
    }
}

#[track_caller]
fn check_fail(ra_fixture: &str, error: ConstEvalError) {
    assert_eq!(eval_goal(ra_fixture).map_err(simplify), Err(error));
}

#[track_caller]
fn check_number(ra_fixture: &str, answer: i128) {
    let r = eval_goal(ra_fixture).unwrap();
    match &r.data(Interner).value {
        chalk_ir::ConstValue::Concrete(c) => match &c.interned {
            ConstScalar::Bytes(b, _) => {
                assert_eq!(
                    b,
                    &answer.to_le_bytes()[0..b.len()],
                    "Bytes differ. In decimal form: actual = {}, expected = {answer}",
                    i128::from_le_bytes(pad16(b, true))
                );
            }
            x => panic!("Expected number but found {:?}", x),
        },
        _ => panic!("result of const eval wasn't a concrete const"),
    }
}

fn eval_goal(ra_fixture: &str) -> Result<Const, ConstEvalError> {
    let (db, file_id) = TestDB::with_single_file(ra_fixture);
    let module_id = db.module_for_file(file_id);
    let def_map = module_id.def_map(&db);
    let scope = &def_map[module_id.local_id].scope;
    let const_id = scope
        .declarations()
        .find_map(|x| match x {
            hir_def::ModuleDefId::ConstId(x) => {
                if db.const_data(x).name.as_ref()?.to_string() == "GOAL" {
                    Some(x)
                } else {
                    None
                }
            }
            _ => None,
        })
        .unwrap();
    db.const_eval(const_id, Substitution::empty(Interner))
}

#[test]
fn add() {
    check_number(r#"const GOAL: usize = 2 + 2;"#, 4);
    check_number(r#"const GOAL: i32 = -2 + --5;"#, 3);
    check_number(r#"const GOAL: i32 = 7 - 5;"#, 2);
    check_number(r#"const GOAL: i32 = 7 + (1 - 5);"#, 3);
}

#[test]
fn bit_op() {
    check_number(r#"const GOAL: u8 = !0 & !(!0 >> 1)"#, 128);
    check_number(r#"const GOAL: i8 = !0 & !(!0 >> 1)"#, 0);
    check_number(r#"const GOAL: i8 = 1 << 7"#, (1i8 << 7) as i128);
    // FIXME: report panic here
    check_number(r#"const GOAL: i8 = 1 << 8"#, 0);
}

#[test]
fn casts() {
    check_number(r#"const GOAL: usize = 12 as *const i32 as usize"#, 12);
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    const GOAL: i32 = {
        let a = [10, 20, 3, 15];
        let x: &[i32] = &a;
        let y: *const [i32] = x;
        let z = y as *const i32;
        unsafe { *z }
    };
        "#,
        10,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    const GOAL: i16 = {
        let a = &mut 5;
        let z = a as *mut _;
        unsafe { *z }
    };
        "#,
        5,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    const GOAL: usize = {
        let a = [10, 20, 3, 15];
        let x: &[i32] = &a;
        let y: *const [i32] = x;
        let z = y as *const [u8]; // slice fat pointer cast don't touch metadata
        let w = unsafe { &*z };
        w.len()
    };
        "#,
        4,
    );
}

#[test]
fn locals() {
    check_number(
        r#"
    const GOAL: usize = {
        let a = 3 + 2;
        let b = a * a;
        b
    };
    "#,
        25,
    );
}

#[test]
fn references() {
    check_number(
        r#"
    const GOAL: usize = {
        let x = 3;
        let y = &mut x;
        *y = 5;
        x
    };
    "#,
        5,
    );
    check_number(
        r#"
    struct Foo(i32);
    impl Foo {
        fn method(&mut self, x: i32) {
            self.0 = 2 * self.0 + x;
        }
    }
    const GOAL: i32 = {
        let mut x = Foo(3);
        x.method(5);
        x.0
    };
    "#,
        11,
    );
}

#[test]
fn reference_autoderef() {
    check_number(
        r#"
    const GOAL: usize = {
        let x = 3;
        let y = &mut x;
        let y: &mut usize = &mut y;
        *y = 5;
        x
    };
    "#,
        5,
    );
    check_number(
        r#"
    const GOAL: usize = {
        let x = 3;
        let y = &&&&&&&x;
        let z: &usize = &y;
        *z
    };
    "#,
        3,
    );
    check_number(
        r#"
    struct Foo<T> { x: T }
    impl<T> Foo<T> {
        fn foo(&mut self) -> T { self.x }
    }
    fn f(i: &mut &mut Foo<Foo<i32>>) -> i32 {
        ((**i).x).foo()
    }
    fn g(i: Foo<Foo<i32>>) -> i32 {
        i.x.foo()
    }
    const GOAL: i32 = f(&mut &mut Foo { x: Foo { x: 3 } }) + g(Foo { x: Foo { x: 5 } });
    "#,
        8,
    );
}

#[test]
fn overloaded_deref() {
    check_number(
        r#"
    //- minicore: deref_mut
    struct Foo;

    impl core::ops::Deref for Foo {
        type Target = i32;
        fn deref(&self) -> &i32 {
            &5
        }
    }

    const GOAL: i32 = {
        let x = Foo;
        let y = &*x;
        *y + *x
    };
    "#,
        10,
    );
}

#[test]
fn overloaded_deref_autoref() {
    check_number(
        r#"
    //- minicore: deref_mut
    struct Foo;
    struct Bar;

    impl core::ops::Deref for Foo {
        type Target = Bar;
        fn deref(&self) -> &Bar {
            &Bar
        }
    }

    impl Bar {
        fn method(&self) -> i32 {
            5
        }
    }

    const GOAL: i32 = Foo.method();
    "#,
        5,
    );
}

#[test]
fn overloaded_index() {
    check_number(
        r#"
    //- minicore: index
    struct Foo;

    impl core::ops::Index<usize> for Foo {
        type Output = i32;
        fn index(&self, index: usize) -> &i32 {
            if index == 7 {
                &700
            } else {
                &1000
            }
        }
    }

    impl core::ops::IndexMut<usize> for Foo {
        fn index_mut(&mut self, index: usize) -> &mut i32 {
            if index == 7 {
                &mut 7
            } else {
                &mut 10
            }
        }
    }

    const GOAL: i32 = {
        (Foo[2]) + (Foo[7]) + (*&Foo[2]) + (*&Foo[7]) + (*&mut Foo[2]) + (*&mut Foo[7])
    };
    "#,
        3417,
    );
}

#[test]
fn function_call() {
    check_number(
        r#"
    const fn f(x: usize) -> usize {
        2 * x + 5
    }
    const GOAL: usize = f(3);
    "#,
        11,
    );
    check_number(
        r#"
    const fn add(x: usize, y: usize) -> usize {
        x + y
    }
    const GOAL: usize = add(add(1, 2), add(3, add(4, 5)));
    "#,
        15,
    );
}

#[test]
fn trait_basic() {
    check_number(
        r#"
    trait Foo {
        fn f(&self) -> u8;
    }

    impl Foo for u8 {
        fn f(&self) -> u8 {
            *self + 33
        }
    }

    const GOAL: u8 = {
        let x = 3;
        Foo::f(&x)
    };
    "#,
        36,
    );
}

#[test]
fn trait_method() {
    check_number(
        r#"
    trait Foo {
        fn f(&self) -> u8;
    }

    impl Foo for u8 {
        fn f(&self) -> u8 {
            *self + 33
        }
    }

    const GOAL: u8 = {
        let x = 3;
        x.f()
    };
    "#,
        36,
    );
}

#[test]
fn generic_fn() {
    check_number(
        r#"
    trait Foo {
        fn f(&self) -> u8;
    }

    impl Foo for () {
        fn f(&self) -> u8 {
            0
        }
    }

    struct Succ<S>(S);

    impl<T: Foo> Foo for Succ<T> {
        fn f(&self) -> u8 {
            self.0.f() + 1
        }
    }

    const GOAL: u8 = Succ(Succ(())).f();
    "#,
        2,
    );
    check_number(
        r#"
    trait Foo {
        fn f(&self) -> u8;
    }

    impl Foo for u8 {
        fn f(&self) -> u8 {
            *self + 33
        }
    }

    fn foof<T: Foo>(x: T, y: T) -> u8 {
        x.f() + y.f()
    }

    const GOAL: u8 = foof(2, 5);
    "#,
        73,
    );
    check_number(
        r#"
    fn bar<A, B>(a: A, b: B) -> B {
        b
    }
        const GOAL: u8 = bar("hello", 12);
        "#,
        12,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    fn bar<A, B>(a: A, b: B) -> B {
        b
    }
    fn foo<T>(x: [T; 2]) -> T {
        bar(x[0], x[1])
    }

    const GOAL: u8 = foo([2, 5]);
    "#,
        5,
    );
}

#[test]
fn impl_trait() {
    check_number(
        r#"
    trait Foo {
        fn f(&self) -> u8;
    }

    impl Foo for u8 {
        fn f(&self) -> u8 {
            *self + 33
        }
    }

    fn foof(x: impl Foo, y: impl Foo) -> impl Foo {
        x.f() + y.f()
    }

    const GOAL: u8 = foof(2, 5).f();
    "#,
        106,
    );
    check_number(
        r#"
        struct Foo<T>(T, T, (T, T));
        trait S {
            fn sum(&self) -> i64;
        }
        impl S for i64 {
            fn sum(&self) -> i64 {
                *self
            }
        }
        impl<T: S> S for Foo<T> {
            fn sum(&self) -> i64 {
                self.0.sum() + self.1.sum() + self.2 .0.sum() + self.2 .1.sum()
            }
        }

        fn foo() -> Foo<impl S> {
            Foo(
                Foo(1i64, 2, (3, 4)),
                Foo(5, 6, (7, 8)),
                (
                    Foo(9, 10, (11, 12)),
                    Foo(13, 14, (15, 16)),
                ),
            )
        }
        const GOAL: i64 = foo().sum();
    "#,
        136,
    );
}

#[test]
fn ifs() {
    check_number(
        r#"
    const fn f(b: bool) -> u8 {
        if b { 1 } else { 10 }
    }

    const GOAL: u8 = f(true) + f(true) + f(false);
        "#,
        12,
    );
    check_number(
        r#"
    const fn max(a: i32, b: i32) -> i32 {
        if a < b { b } else { a }
    }

    const GOAL: i32 = max(max(1, max(10, 3)), 0-122);
        "#,
        10,
    );

    check_number(
        r#"
    const fn max(a: &i32, b: &i32) -> &i32 {
        if *a < *b { b } else { a }
    }

    const GOAL: i32 = *max(max(&1, max(&10, &3)), &5);
        "#,
        10,
    );
}

#[test]
fn loops() {
    check_number(
        r#"
    const GOAL: u8 = {
        let mut x = 0;
        loop {
            x = x + 1;
            while true {
                break;
            }
            x = x + 1;
            if x == 2 {
                continue;
            }
            break;
        }
        x
    };
        "#,
        4,
    );
    check_number(
        r#"
    const GOAL: u8 = {
        let mut x = 0;
        loop {
            x = x + 1;
            if x == 5 {
                break x + 2;
            }
        }
    };
        "#,
        7,
    );
    check_number(
        r#"
    const GOAL: u8 = {
        'a: loop {
            let x = 'b: loop {
                let x = 'c: loop {
                    let x = 'd: loop {
                        let x = 'e: loop {
                            break 'd 1;
                        };
                        break 2 + x;
                    };
                    break 3 + x;
                };
                break 'a 4 + x;
            };
            break 5 + x;
        }
    };
        "#,
        8,
    );
}

#[test]
fn for_loops() {
    check_number(
        r#"
    //- minicore: iterator

    struct Range {
        start: u8,
        end: u8,
    }

    impl Iterator for Range {
        type Item = u8;
        fn next(&mut self) -> Option<u8> {
            if self.start >= self.end {
                None
            } else {
                let r = self.start;
                self.start = self.start + 1;
                Some(r)
            }
        }
    }

    const GOAL: u8 = {
        let mut sum = 0;
        let ar = Range { start: 1, end: 11 };
        for i in ar {
            sum = sum + i;
        }
        sum
    };
        "#,
        55,
    );
}

#[test]
fn ranges() {
    check_number(
        r#"
    //- minicore: range
    const GOAL: i32 = (1..2).start + (20..10).end + (100..=200).start + (2000..=1000).end
        + (10000..).start + (..100000).end + (..=1000000).end;
        "#,
        1111111,
    );
}

#[test]
fn recursion() {
    check_number(
        r#"
    const fn fact(k: i32) -> i32 {
        if k > 0 { fact(k - 1) * k } else { 1 }
    }

    const GOAL: i32 = fact(5);
        "#,
        120,
    );
}

#[test]
fn structs() {
    check_number(
        r#"
        struct Point {
            x: i32,
            y: i32,
        }

        const GOAL: i32 = {
            let p = Point { x: 5, y: 2 };
            let y = 1;
            let x = 3;
            let q = Point { y, x };
            p.x + p.y + p.x + q.y + q.y + q.x
        };
        "#,
        17,
    );
    check_number(
        r#"
        struct Point {
            x: i32,
            y: i32,
        }

        const GOAL: i32 = {
            let p = Point { x: 5, y: 2 };
            let p2 = Point { x: 3, ..p };
            p.x * 1000 + p.y * 100 + p2.x * 10 + p2.y
        };
        "#,
        5232,
    );
    check_number(
        r#"
        struct Point {
            x: i32,
            y: i32,
        }

        const GOAL: i32 = {
            let p = Point { x: 5, y: 2 };
            let Point { x, y } = p;
            let Point { x: x2, .. } = p;
            let Point { y: y2, .. } = p;
            x * 1000 + y * 100 + x2 * 10 + y2
        };
        "#,
        5252,
    );
}

#[test]
fn unions() {
    check_number(
        r#"
        union U {
            f1: i64,
            f2: (i32, i32),
        }

        const GOAL: i32 = {
            let p = U { f1: 0x0123ABCD0123DCBA };
            let p = unsafe { p.f2 };
            p.0 + p.1 + p.1
        };
        "#,
        0x0123ABCD * 2 + 0x0123DCBA,
    );
}

#[test]
fn tuples() {
    check_number(
        r#"
    const GOAL: u8 = {
        let a = (10, 20, 3, 15);
        a.1
    };
        "#,
        20,
    );
    check_number(
        r#"
    const GOAL: u8 = {
        let mut a = (10, 20, 3, 15);
        a.1 = 2;
        a.0 + a.1 + a.2 + a.3
    };
        "#,
        30,
    );
    check_number(
        r#"
    struct TupleLike(i32, i64, u8, u16);
    const GOAL: i64 = {
        let a = TupleLike(10, 20, 3, 15);
        let TupleLike(b, .., c) = a;
        a.1 * 100 + b as i64 + c as i64
    };
        "#,
        2025,
    );
    check_number(
        r#"
    const GOAL: u8 = {
        match (&(2 + 2), &4) {
            (left_val, right_val) => {
                if !(*left_val == *right_val) {
                    2
                } else {
                    5
                }
            }
        }
    };
        "#,
        5,
    );
}

#[test]
fn path_pattern_matching() {
    check_number(
        r#"
    enum Season {
        Spring,
        Summer,
        Fall,
        Winter,
    }

    use Season::*;

    const fn f(x: Season) -> i32 {
        match x {
            Spring => 1,
            Summer => 2,
            Fall => 3,
            Winter => 4,
        }
    }
    const GOAL: i32 = f(Spring) + 10 * f(Summer) + 100 * f(Fall) + 1000 * f(Winter);
        "#,
        4321,
    );
}

#[test]
fn pattern_matching_literal() {
    check_number(
        r#"
    const fn f(x: i32) -> i32 {
        match x {
            -1 => 1,
            1 => 10,
            _ => 100,
        }
    }
    const GOAL: i32 = f(-1) + f(1) + f(0) + f(-5);
        "#,
        211,
    );
    check_number(
        r#"
    const fn f(x: &str) -> u8 {
        match x {
            "foo" => 1,
            "bar" => 10,
            _ => 100,
        }
    }
    const GOAL: u8 = f("foo") + f("bar");
        "#,
        11,
    );
}

#[test]
fn pattern_matching_ergonomics() {
    check_number(
        r#"
    const fn f(x: &(u8, u8)) -> u8 {
        match x {
            (a, b) => *a + *b
        }
    }
    const GOAL: u8 = f(&(2, 3));
        "#,
        5,
    );
    check_number(
        r#"
    const GOAL: u8 = {
        let a = &(2, 3);
        let &(x, y) = a;
        x + y
    };
        "#,
        5,
    );
}

#[test]
fn let_else() {
    check_number(
        r#"
    const fn f(x: &(u8, u8)) -> u8 {
        let (a, b) = x;
        *a + *b
    }
    const GOAL: u8 = f(&(2, 3));
        "#,
        5,
    );
    check_number(
        r#"
    enum SingleVariant {
        Var(u8, u8),
    }
    const fn f(x: &&&&&SingleVariant) -> u8 {
        let SingleVariant::Var(a, b) = x;
        *a + *b
    }
    const GOAL: u8 = f(&&&&&SingleVariant::Var(2, 3));
        "#,
        5,
    );
    check_number(
        r#"
    //- minicore: option
    const fn f(x: Option<i32>) -> i32 {
        let Some(x) = x else { return 10 };
        2 * x
    }
    const GOAL: i32 = f(Some(1000)) + f(None);
        "#,
        2010,
    );
}

#[test]
fn function_param_patterns() {
    check_number(
        r#"
    const fn f((a, b): &(u8, u8)) -> u8 {
        *a + *b
    }
    const GOAL: u8 = f(&(2, 3));
        "#,
        5,
    );
    check_number(
        r#"
    const fn f(c @ (a, b): &(u8, u8)) -> u8 {
        *a + *b + c.0 + (*c).1
    }
    const GOAL: u8 = f(&(2, 3));
        "#,
        10,
    );
    check_number(
        r#"
    const fn f(ref a: u8) -> u8 {
        *a
    }
    const GOAL: u8 = f(2);
        "#,
        2,
    );
    check_number(
        r#"
    struct Foo(u8);
    impl Foo {
        const fn f(&self, (a, b): &(u8, u8)) -> u8 {
            self.0 + *a + *b
        }
    }
    const GOAL: u8 = Foo(4).f(&(2, 3));
        "#,
        9,
    );
}

#[test]
fn match_guards() {
    check_number(
        r#"
    //- minicore: option, eq
    impl<T: PartialEq> PartialEq for Option<T> {
        fn eq(&self, other: &Rhs) -> bool {
            match (self, other) {
                (Some(x), Some(y)) => x == y,
                (None, None) => true,
                _ => false,
            }
        }
    }
    fn f(x: Option<i32>) -> i32 {
        match x {
            y if y == Some(42) => 42000,
            Some(y) => y,
            None => 10
        }
    }
    const GOAL: i32 = f(Some(42)) + f(Some(2)) + f(None);
        "#,
        42012,
    );
}

#[test]
fn options() {
    check_number(
        r#"
    //- minicore: option
    const GOAL: u8 = {
        let x = Some(2);
        match x {
            Some(y) => 2 * y,
            _ => 10,
        }
    };
        "#,
        4,
    );
    check_number(
        r#"
    //- minicore: option
    fn f(x: Option<Option<i32>>) -> i32 {
        if let Some(y) = x && let Some(z) = y {
            z
        } else if let Some(y) = x {
            1
        } else {
            0
        }
    }
    const GOAL: i32 = f(Some(Some(10))) + f(Some(None)) + f(None);
        "#,
        11,
    );
    check_number(
        r#"
    //- minicore: option
    const GOAL: u8 = {
        let x = None;
        match x {
            Some(y) => 2 * y,
            _ => 10,
        }
    };
        "#,
        10,
    );
    check_number(
        r#"
    //- minicore: option
    const GOAL: Option<&u8> = None;
        "#,
        0,
    );
}

#[test]
fn from_trait() {
    check_number(
        r#"
    //- minicore: from
    struct E1(i32);
    struct E2(i32);

    impl From<E1> for E2 {
        fn from(E1(x): E1) -> Self {
            E2(1000 * x)
        }
    }
    const GOAL: i32 = {
        let x: E2 = E1(2).into();
        x.0
    };
    "#,
        2000,
    );
}

#[test]
fn try_operator() {
    check_number(
        r#"
    //- minicore: option, try
    const fn f(x: Option<i32>, y: Option<i32>) -> Option<i32> {
        Some(x? * y?)
    }
    const fn g(x: Option<i32>, y: Option<i32>) -> i32 {
        match f(x, y) {
            Some(k) => k,
            None => 5,
        }
    }
    const GOAL: i32 = g(Some(10), Some(20)) + g(Some(30), None) + g(None, Some(40)) + g(None, None);
        "#,
        215,
    );
    check_number(
        r#"
    //- minicore: result, try, from
    struct E1(i32);
    struct E2(i32);

    impl From<E1> for E2 {
        fn from(E1(x): E1) -> Self {
            E2(1000 * x)
        }
    }

    const fn f(x: Result<i32, E1>) -> Result<i32, E2> {
        Ok(x? * 10)
    }
    const fn g(x: Result<i32, E1>) -> i32 {
        match f(x) {
            Ok(k) => 7 * k,
            Err(E2(k)) => 5 * k,
        }
    }
    const GOAL: i32 = g(Ok(2)) + g(Err(E1(3)));
        "#,
        15140,
    );
}

#[test]
fn try_block() {
    check_number(
        r#"
    //- minicore: option, try
    const fn g(x: Option<i32>, y: Option<i32>) -> i32 {
        let r = try { x? * y? };
        match r {
            Some(k) => k,
            None => 5,
        }
    }
    const GOAL: i32 = g(Some(10), Some(20)) + g(Some(30), None) + g(None, Some(40)) + g(None, None);
        "#,
        215,
    );
}

#[test]
fn closures() {
    check_number(
        r#"
    //- minicore: fn, copy
    const GOAL: i32 = {
        let y = 5;
        let c = |x| x + y;
        c(2)
    };
        "#,
        7,
    );
    check_number(
        r#"
    //- minicore: fn, copy
    const GOAL: i32 = {
        let y = 5;
        let c = |(a, b): &(i32, i32)| *a + *b + y;
        c(&(2, 3))
    };
        "#,
        10,
    );
    check_number(
        r#"
    //- minicore: fn, copy
    const GOAL: i32 = {
        let mut y = 5;
        let c = |x| {
            y = y + x;
        };
        c(2);
        c(3);
        y
    };
        "#,
        10,
    );
    check_number(
        r#"
    //- minicore: fn, copy
    struct X(i32);
    impl X {
        fn mult(&mut self, n: i32) {
            self.0 = self.0 * n
        }
    }
    const GOAL: i32 = {
        let x = X(1);
        let c = || {
            x.mult(2);
            || {
                x.mult(3);
                || {
                    || {
                        x.mult(4);
                        || {
                            x.mult(x.0);
                            || {
                                x.0
                            }
                        }
                    }
                }
            }
        };
        let r = c()()()()()();
        r + x.0
    };
        "#,
        24 * 24 * 2,
    );
}

#[test]
fn or_pattern() {
    check_number(
        r#"
    const GOAL: u8 = {
        let (a | a) = 2;
        a
    };
        "#,
        2,
    );
    check_number(
        r#"
    //- minicore: option
    const fn f(x: Option<i32>) -> i32 {
        let (Some(a) | Some(a)) = x else { return 2; };
        a
    }
    const GOAL: i32 = f(Some(10)) + f(None);
        "#,
        12,
    );
    check_number(
        r#"
    //- minicore: option
    const fn f(x: Option<i32>, y: Option<i32>) -> i32 {
        match (x, y) {
            (Some(x), Some(y)) => x * y,
            (Some(a), _) | (_, Some(a)) => a,
            _ => 10,
        }
    }
    const GOAL: i32 = f(Some(10), Some(20)) + f(Some(30), None) + f(None, Some(40)) + f(None, None);
        "#,
        280,
    );
}

#[test]
fn function_pointer() {
    check_number(
        r#"
    fn add2(x: u8) -> u8 {
        x + 2
    }
    const GOAL: u8 = {
        let plus2 = add2;
        plus2(3)
    };
        "#,
        5,
    );
    check_number(
        r#"
    fn add2(x: u8) -> u8 {
        x + 2
    }
    const GOAL: u8 = {
        let plus2: fn(u8) -> u8 = add2;
        plus2(3)
    };
        "#,
        5,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    fn add2(x: u8) -> u8 {
        x + 2
    }
    fn mult3(x: u8) -> u8 {
        x * 3
    }
    const GOAL: u8 = {
        let x = [add2, mult3];
        x[0](1) + x[1](5)
    };
        "#,
        18,
    );
}

#[test]
fn enum_variant_as_function() {
    check_number(
        r#"
    //- minicore: option
    const GOAL: u8 = {
        let f = Some;
        f(3).unwrap_or(2)
    };
        "#,
        3,
    );
    check_number(
        r#"
    //- minicore: option
    const GOAL: u8 = {
        let f: fn(u8) -> Option<u8> = Some;
        f(3).unwrap_or(2)
    };
        "#,
        3,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    enum Foo {
        Add2(u8),
        Mult3(u8),
    }
    use Foo::*;
    const fn f(x: Foo) -> u8 {
        match x {
            Add2(x) => x + 2,
            Mult3(x) => x * 3,
        }
    }
    const GOAL: u8 = {
        let x = [Add2, Mult3];
        f(x[0](1)) + f(x[1](5))
    };
        "#,
        18,
    );
}

#[test]
fn function_traits() {
    check_number(
        r#"
    //- minicore: fn
    fn add2(x: u8) -> u8 {
        x + 2
    }
    fn call(f: impl Fn(u8) -> u8, x: u8) -> u8 {
        f(x)
    }
    fn call_mut(mut f: impl FnMut(u8) -> u8, x: u8) -> u8 {
        f(x)
    }
    fn call_once(f: impl FnOnce(u8) -> u8, x: u8) -> u8 {
        f(x)
    }
    const GOAL: u8 = call(add2, 3) + call_mut(add2, 3) + call_once(add2, 3);
        "#,
        15,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, fn
    fn add2(x: u8) -> u8 {
        x + 2
    }
    fn call(f: &dyn Fn(u8) -> u8, x: u8) -> u8 {
        f(x)
    }
    fn call_mut(f: &mut dyn FnMut(u8) -> u8, x: u8) -> u8 {
        f(x)
    }
    const GOAL: u8 = call(&add2, 3) + call_mut(&mut add2, 3);
        "#,
        10,
    );
    check_number(
        r#"
    //- minicore: fn
    fn add2(x: u8) -> u8 {
        x + 2
    }
    fn call(f: impl Fn(u8) -> u8, x: u8) -> u8 {
        f(x)
    }
    fn call_mut(mut f: impl FnMut(u8) -> u8, x: u8) -> u8 {
        f(x)
    }
    fn call_once(f: impl FnOnce(u8) -> u8, x: u8) -> u8 {
        f(x)
    }
    const GOAL: u8 = {
        let add2: fn(u8) -> u8 = add2;
        call(add2, 3) + call_mut(add2, 3) + call_once(add2, 3)
    };
        "#,
        15,
    );
    check_number(
        r#"
    //- minicore: fn
    fn add2(x: u8) -> u8 {
        x + 2
    }
    fn call(f: &&&&&impl Fn(u8) -> u8, x: u8) -> u8 {
        f(x)
    }
    const GOAL: u8 = call(&&&&&add2, 3);
        "#,
        5,
    );
}

#[test]
fn dyn_trait() {
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    trait Foo {
        fn foo(&self) -> u8 { 10 }
    }
    struct S1;
    struct S2;
    struct S3;
    impl Foo for S1 {
        fn foo(&self) -> u8 { 1 }
    }
    impl Foo for S2 {
        fn foo(&self) -> u8 { 2 }
    }
    impl Foo for S3 {}
    const GOAL: u8 = {
        let x: &[&dyn Foo] = &[&S1, &S2, &S3];
        x[0].foo() + x[1].foo() + x[2].foo()
    };
        "#,
        13,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    trait Foo {
        fn foo(&self) -> i32 { 10 }
    }
    trait Bar {
        fn bar(&self) -> i32 { 20 }
    }

    struct S;
    impl Foo for S {
        fn foo(&self) -> i32 { 200 }
    }
    impl Bar for dyn Foo {
        fn bar(&self) -> i32 { 700 }
    }
    const GOAL: i32 = {
        let x: &dyn Foo = &S;
        x.bar() + x.foo()
    };
        "#,
        900,
    );
}

#[test]
fn array_and_index() {
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    const GOAL: u8 = {
        let a = [10, 20, 3, 15];
        let x: &[u8] = &a;
        x[1]
    };
        "#,
        20,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    const GOAL: usize = [1, 2, 3][2];"#,
        3,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    const GOAL: usize = { let a = [1, 2, 3]; let x: &[i32] = &a; x.len() };"#,
        3,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    const GOAL: usize = {
        let a = [1, 2, 3];
        let x: &[i32] = &a;
        let y = &*x;
        y.len()
    };"#,
        3,
    );
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    const GOAL: usize = [1, 2, 3, 4, 5].len();"#,
        5,
    );
}

#[test]
fn byte_string() {
    check_number(
        r#"
    //- minicore: coerce_unsized, index, slice
    const GOAL: u8 = {
        let a = b"hello";
        let x: &[u8] = a;
        x[0]
    };
        "#,
        104,
    );
}

#[test]
fn consts() {
    check_number(
        r#"
    const F1: i32 = 1;
    const F3: i32 = 3 * F2;
    const F2: i32 = 2 * F1;
    const GOAL: i32 = F3;
    "#,
        6,
    );
}

#[test]
fn enums() {
    check_number(
        r#"
    enum E {
        F1 = 1,
        F2 = 2 * E::F1 as isize, // Rustc expects an isize here
        F3 = 3 * E::F2 as isize,
    }
    const GOAL: u8 = E::F3 as u8;
    "#,
        6,
    );
    check_number(
        r#"
    enum E { F1 = 1, F2, }
    const GOAL: u8 = E::F2 as u8;
    "#,
        2,
    );
    check_number(
        r#"
    enum E { F1, }
    const GOAL: u8 = E::F1 as u8;
    "#,
        0,
    );
    let r = eval_goal(
        r#"
        enum E { A = 1, B }
        const GOAL: E = E::A;
        "#,
    )
    .unwrap();
    assert_eq!(try_const_usize(&r), Some(1));
}

#[test]
fn const_loop() {
    check_fail(
        r#"
    const F1: i32 = 1 * F3;
    const F3: i32 = 3 * F2;
    const F2: i32 = 2 * F1;
    const GOAL: i32 = F3;
    "#,
        ConstEvalError::MirLowerError(MirLowerError::Loop),
    );
}

#[test]
fn const_transfer_memory() {
    check_number(
        r#"
    const A1: &i32 = &2;
    const A2: &i32 = &5;
    const GOAL: i32 = *A1 + *A2;
    "#,
        7,
    );
}

#[test]
fn const_impl_assoc() {
    check_number(
        r#"
    struct U5;
    impl U5 {
        const VAL: usize = 5;
    }
    const GOAL: usize = U5::VAL;
    "#,
        5,
    );
}

#[test]
fn const_generic_subst_fn() {
    check_number(
        r#"
    const fn f<const A: usize>(x: usize) -> usize {
        A * x + 5
    }
    const GOAL: usize = f::<2>(3);
    "#,
        11,
    );
}

#[test]
fn const_generic_subst_assoc_const_impl() {
    check_number(
        r#"
    struct Adder<const N: usize, const M: usize>;
    impl<const N: usize, const M: usize> Adder<N, M> {
        const VAL: usize = N + M;
    }
    const GOAL: usize = Adder::<2, 3>::VAL;
    "#,
        5,
    );
}

#[test]
fn const_trait_assoc() {
    // FIXME: this should evaluate to 0
    check_fail(
        r#"
    struct U0;
    trait ToConst {
        const VAL: usize;
    }
    impl ToConst for U0 {
        const VAL: usize = 0;
    }
    const GOAL: usize = U0::VAL;
    "#,
        ConstEvalError::MirLowerError(MirLowerError::IncompleteExpr),
    );
}

#[test]
fn exec_limits() {
    check_fail(
        r#"
    const GOAL: usize = loop {};
    "#,
        ConstEvalError::MirEvalError(MirEvalError::ExecutionLimitExceeded),
    );
    check_fail(
        r#"
    const fn f(x: i32) -> i32 {
        f(x + 1)
    }
    const GOAL: i32 = f(0);
    "#,
        ConstEvalError::MirEvalError(MirEvalError::StackOverflow),
    );
    // Reasonable code should still work
    check_number(
        r#"
    const fn nth_odd(n: i32) -> i32 {
        2 * n - 1
    }
    const fn f(n: i32) -> i32 {
        let sum = 0;
        let i = 0;
        while i < n {
            i = i + 1;
            sum = sum + nth_odd(i);
        }
        sum
    }
    const GOAL: i32 = f(10000);
    "#,
        10000 * 10000,
    );
}

#[test]
fn type_error() {
    let e = eval_goal(
        r#"
    const GOAL: u8 = {
        let x: u16 = 2;
        let y: (u8, u8) = x;
        y.0
    };
    "#,
    );
    assert!(matches!(e, Err(ConstEvalError::MirLowerError(MirLowerError::TypeMismatch(_)))));
}
