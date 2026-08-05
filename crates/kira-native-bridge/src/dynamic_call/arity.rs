//! The arity table the dynamic call dispatches through.
//!
//! One match arm per parameter count, each transmuting the target to a
//! function type of exactly that many integer-width parameters. Split out of
//! `dynamic_call` because it is mechanical: sixteen arms that say the same
//! thing at sixteen widths, and reading them adds nothing to understanding the
//! call surface they serve.
//!
//! The expansion lands in `dynamic_call`, which is where `c_void` is in scope.

/// Invokes `target` with `args`, as a function of `args.len()` parameters.
///
/// Expanded once per return type, so every `transmute` inside is between two
/// concrete pointer-width types.
macro_rules! invoke_with_args {
    ($target:expr, $args:expr, $ret:ty, $zero:expr) => {{
        let target: *mut c_void = $target;
        let args: &[usize] = $args;
        // SAFETY: the caller of the enclosing `extern "C"` function guarantees
        // `target` is a function whose parameters are all integer- or
        // pointer-class and whose return type is `$ret`. `prepare` has already
        // rejected a null target, a float argument, and an arity this match
        // does not cover, so the arm selected below has exactly the parameter
        // count the callee expects and every argument travels in the register
        // or stack slot a C caller would have used.
        unsafe {
            match args.len() {
                0 => {
                    let f: extern "system" fn() -> $ret = ::std::mem::transmute(target);
                    f()
                }
                1 => {
                    let f: extern "system" fn(usize) -> $ret = ::std::mem::transmute(target);
                    f(args[0])
                }
                2 => {
                    let f: extern "system" fn(usize, usize) -> $ret = ::std::mem::transmute(target);
                    f(args[0], args[1])
                }
                3 => {
                    let f: extern "system" fn(usize, usize, usize) -> $ret =
                        ::std::mem::transmute(target);
                    f(args[0], args[1], args[2])
                }
                4 => {
                    let f: extern "system" fn(usize, usize, usize, usize) -> $ret =
                        ::std::mem::transmute(target);
                    f(args[0], args[1], args[2], args[3])
                }
                5 => {
                    let f: extern "system" fn(usize, usize, usize, usize, usize) -> $ret =
                        ::std::mem::transmute(target);
                    f(args[0], args[1], args[2], args[3], args[4])
                }
                6 => {
                    let f: extern "system" fn(usize, usize, usize, usize, usize, usize) -> $ret =
                        ::std::mem::transmute(target);
                    f(args[0], args[1], args[2], args[3], args[4], args[5])
                }
                7 => {
                    let f: extern "system" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> $ret = ::std::mem::transmute(target);
                    f(
                        args[0], args[1], args[2], args[3], args[4], args[5], args[6],
                    )
                }
                8 => {
                    let f: extern "system" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> $ret = ::std::mem::transmute(target);
                    f(
                        args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
                    )
                }
                9 => {
                    let f: extern "system" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> $ret = ::std::mem::transmute(target);
                    f(
                        args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
                        args[8],
                    )
                }
                10 => {
                    let f: extern "system" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> $ret = ::std::mem::transmute(target);
                    f(
                        args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
                        args[8], args[9],
                    )
                }
                11 => {
                    let f: extern "system" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> $ret = ::std::mem::transmute(target);
                    f(
                        args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
                        args[8], args[9], args[10],
                    )
                }
                12 => {
                    let f: extern "system" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> $ret = ::std::mem::transmute(target);
                    f(
                        args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
                        args[8], args[9], args[10], args[11],
                    )
                }
                13 => {
                    let f: extern "system" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> $ret = ::std::mem::transmute(target);
                    f(
                        args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
                        args[8], args[9], args[10], args[11], args[12],
                    )
                }
                14 => {
                    let f: extern "system" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> $ret = ::std::mem::transmute(target);
                    f(
                        args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
                        args[8], args[9], args[10], args[11], args[12], args[13],
                    )
                }
                15 => {
                    let f: extern "system" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> $ret = ::std::mem::transmute(target);
                    f(
                        args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
                        args[8], args[9], args[10], args[11], args[12], args[13], args[14],
                    )
                }
                16 => {
                    let f: extern "system" fn(
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                        usize,
                    ) -> $ret = ::std::mem::transmute(target);
                    f(
                        args[0], args[1], args[2], args[3], args[4], args[5], args[6], args[7],
                        args[8], args[9], args[10], args[11], args[12], args[13], args[14],
                        args[15],
                    )
                }
                _ => $zero,
            }
        }
    }};
}

pub(crate) use invoke_with_args;
