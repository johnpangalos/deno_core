// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Converters for `any`, `boolean`, the integer types and the floating point
//! types.

use std::borrow::Cow;
use std::ops::Deref;

use super::out_of_range;
use super::to_number;
use super::WebIdlConverter;
use super::WebIdlError;
use crate::v8;

/// Options for the integer converters.
///
/// `enforce_range` takes precedence over `clamp`. With neither set, out of
/// range values wrap modulo 2^bits.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  /// Throw on NaN, infinities and values outside the range of the type.
  pub enforce_range: bool,
  /// Clamp to the range of the type, rounding half to even. NaN becomes 0.
  pub clamp: bool,
}

/// A WebIDL `unrestricted double`: NaN and infinities are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UnrestrictedDouble(pub f64);

impl Deref for UnrestrictedDouble {
  type Target = f64;

  fn deref(&self) -> &f64 {
    &self.0
  }
}

/// A WebIDL `unrestricted float`: NaN and infinities are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UnrestrictedFloat(pub f32);

impl Deref for UnrestrictedFloat {
  type Target = f32;

  fn deref(&self) -> &f32 {
    &self.0
  }
}

/// Returns the value unchanged. Never fails.
impl<'a> WebIdlConverter<'a> for v8::Local<'a, v8::Value> {
  type Options = ();

  fn convert<C>(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value)
  }
}

/// `ToBoolean`. Never fails.
impl<'a> WebIdlConverter<'a> for bool {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value.boolean_value(scope))
  }
}

/// Converts a number to an integer. The bounds only apply to `enforce_range`
/// and `clamp`. The default mode wraps modulo 2^bits.
fn convert_int(
  number: f64,
  bits: u32,
  signed: bool,
  (min, max): (f64, f64),
  options: &IntOptions,
) -> Result<i128, &'static str> {
  if options.enforce_range {
    if !number.is_finite() {
      return Err("is not a finite number.");
    }
    let truncated = number.trunc();
    if truncated < min || truncated > max {
      return Err("is outside the range of the type.");
    }
    return Ok(truncated as i128);
  }
  if options.clamp {
    if number.is_nan() {
      return Ok(0);
    }
    return Ok(number.clamp(min, max).round_ties_even() as i128);
  }
  if !number.is_finite() {
    return Ok(0);
  }
  // fmod is exact, so the modulo is exact too, even for 64-bit values.
  let remainder = number.trunc() % 2f64.powi(bits as i32);
  let modulus = 1i128 << bits;
  let wrapped = (remainder as i128).rem_euclid(modulus);
  if signed && wrapped >= modulus / 2 {
    Ok(wrapped - modulus)
  } else {
    Ok(wrapped)
  }
}

macro_rules! int_converter {
  ($ty:ty, $bits:expr, $signed:expr, $min:expr, $max:expr) => {
    /// ConvertToInt with [`IntOptions`]. The 64-bit bounds are limited to
    /// +/-(2^53 - 1) so that they are exact in a double.
    impl<'a> WebIdlConverter<'a> for $ty {
      type Options = IntOptions;

      fn convert<C>(
        scope: &mut v8::HandleScope<'a>,
        value: v8::Local<'a, v8::Value>,
        prefix: Cow<'static, str>,
        context: C,
        options: &IntOptions,
      ) -> Result<Self, WebIdlError>
      where
        C: Fn() -> Cow<'static, str>,
      {
        let number = to_number(scope, value, &prefix, &context)?;
        match convert_int(number, $bits, $signed, ($min, $max), options) {
          Ok(integer) => Ok(integer as $ty),
          Err(reason) => Err(out_of_range(&prefix, &context, reason)),
        }
      }
    }
  };
}

int_converter!(i8, 8, true, -128.0, 127.0);
int_converter!(u8, 8, false, 0.0, 255.0);
int_converter!(i16, 16, true, -32768.0, 32767.0);
int_converter!(u16, 16, false, 0.0, 65535.0);
int_converter!(i32, 32, true, -2147483648.0, 2147483647.0);
int_converter!(u32, 32, false, 0.0, 4294967295.0);
int_converter!(i64, 64, true, -9007199254740991.0, 9007199254740991.0);
int_converter!(u64, 64, false, 0.0, 9007199254740991.0);

/// A finite double. Rejects NaN and infinities.
impl<'a> WebIdlConverter<'a> for f64 {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let number = to_number(scope, value, &prefix, &context)?;
    if !number.is_finite() {
      return Err(out_of_range(&prefix, &context, "is not a finite number."));
    }
    Ok(number)
  }
}

/// A finite float. The double is rounded to nearest, and a result that
/// overflows to infinity is rejected.
impl<'a> WebIdlConverter<'a> for f32 {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let number = to_number(scope, value, &prefix, &context)? as f32;
    if !number.is_finite() {
      return Err(out_of_range(&prefix, &context, "is not a finite number."));
    }
    Ok(number)
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedDouble {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(UnrestrictedDouble(to_number(
      scope, value, &prefix, &context,
    )?))
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedFloat {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    // Overflow to infinity is allowed for unrestricted floats.
    Ok(UnrestrictedFloat(
      to_number(scope, value, &prefix, &context)? as f32,
    ))
  }
}

#[cfg(test)]
mod tests {
  use std::borrow::Cow;

  use super::*;
  use crate::webidl::with_js_value;

  fn prefix() -> Cow<'static, str> {
    Cow::Borrowed("P")
  }

  fn context() -> Cow<'static, str> {
    Cow::Borrowed("C")
  }

  fn int<T>(source: &str, options: IntOptions) -> Result<T, String>
  where
    T: for<'a> WebIdlConverter<'a, Options = IntOptions>,
  {
    with_js_value(source, |scope, value| {
      T::convert(scope, value, prefix(), context, &options)
        .map_err(|e| e.to_string())
    })
  }

  fn boolean(source: &str) -> Result<bool, String> {
    with_js_value(source, |scope, value| {
      bool::convert(scope, value, prefix(), context, &())
        .map_err(|e| e.to_string())
    })
  }

  fn float<T>(source: &str) -> Result<T, String>
  where
    T: for<'a> WebIdlConverter<'a, Options = ()>,
  {
    with_js_value(source, |scope, value| {
      T::convert(scope, value, prefix(), context, &())
        .map_err(|e| e.to_string())
    })
  }

  fn enforce() -> IntOptions {
    IntOptions {
      enforce_range: true,
      clamp: false,
    }
  }

  fn clamp() -> IntOptions {
    IntOptions {
      enforce_range: false,
      clamp: true,
    }
  }

  #[test]
  fn any_never_fails() {
    for source in ["undefined", "null", "Symbol()", "1n"] {
      let ok = with_js_value(source, |scope, value| {
        <v8::Local<v8::Value>>::convert(scope, value, prefix(), context, &())
          .is_ok()
      });
      assert!(ok, "{source}");
    }
  }

  #[test]
  fn bool_falsy_values() {
    for source in ["0", "-0", "NaN", "''", "null", "undefined", "0n"] {
      assert_eq!(boolean(source), Ok(false), "{source}");
    }
  }

  #[test]
  fn bool_truthy_values() {
    for source in ["'false'", "({})", "[]", "Symbol()", "-1"] {
      assert_eq!(boolean(source), Ok(true), "{source}");
    }
  }

  #[test]
  fn int_converts_strings_and_primitives() {
    let default = IntOptions::default();
    assert_eq!(int::<u8>("' 12 '", default), Ok(12));
    assert_eq!(int::<u8>("'0x10'", default), Ok(16));
    assert_eq!(int::<u8>("''", default), Ok(0));
    assert_eq!(int::<u8>("[]", default), Ok(0));
    assert_eq!(int::<u8>("null", default), Ok(0));
    assert_eq!(int::<u8>("true", default), Ok(1));
  }

  #[test]
  fn int_symbol_and_bigint_are_errors() {
    assert_eq!(
      int::<u8>("Symbol()", IntOptions::default()),
      Err("P: C is a Symbol and cannot be converted to a number.".into())
    );
    assert_eq!(
      int::<i64>("1n", IntOptions::default()),
      Err("P: C is a BigInt and cannot be converted to a number.".into())
    );
  }

  #[test]
  fn int_js_exception_is_an_error() {
    assert_eq!(
      int::<i32>("({valueOf(){throw 1}})", IntOptions::default()),
      Err("P: C threw an exception: 1".into())
    );
  }

  #[test]
  fn int_enforce_range_errors() {
    assert_eq!(
      int::<u8>("256", enforce()),
      Err("P: C is outside the range of the type.".into())
    );
    assert_eq!(
      int::<u8>("-1", enforce()),
      Err("P: C is outside the range of the type.".into())
    );
    assert_eq!(
      int::<i64>("2 ** 53", enforce()),
      Err("P: C is outside the range of the type.".into())
    );
    assert_eq!(
      int::<u64>("-0.5 - 1", enforce()),
      Err("P: C is outside the range of the type.".into())
    );
    for source in ["NaN", "Infinity", "-Infinity"] {
      assert_eq!(
        int::<u32>(source, enforce()),
        Err("P: C is not a finite number.".into()),
        "{source}"
      );
    }
  }

  #[test]
  fn int_enforce_range_truncates() {
    assert_eq!(int::<u8>("255.9", enforce()), Ok(255));
    assert_eq!(int::<i8>("-127.9", enforce()), Ok(-127));
    assert_eq!(int::<u64>("-0.5", enforce()), Ok(0));
    assert_eq!(int::<i64>("2 ** 53 - 1", enforce()), Ok(9007199254740991));
  }

  #[test]
  fn int_clamp() {
    assert_eq!(int::<u8>("2.5", clamp()), Ok(2));
    assert_eq!(int::<u8>("3.5", clamp()), Ok(4));
    assert_eq!(int::<u8>("-0.5", clamp()), Ok(0));
    assert_eq!(int::<u8>("300", clamp()), Ok(255));
    assert_eq!(int::<u8>("255.5", clamp()), Ok(255));
    assert_eq!(int::<u8>("-Infinity", clamp()), Ok(0));
    assert_eq!(int::<u8>("Infinity", clamp()), Ok(255));
    assert_eq!(int::<u8>("NaN", clamp()), Ok(0));
    assert_eq!(int::<i8>("-200", clamp()), Ok(-128));
    assert_eq!(int::<u64>("2 ** 60", clamp()), Ok(9007199254740991));
  }

  #[test]
  fn int_clamp_rejects_non_numbers_too() {
    assert_eq!(
      int::<u8>("Symbol()", clamp()),
      Err("P: C is a Symbol and cannot be converted to a number.".into())
    );
  }

  #[test]
  fn int_default_wraps_modulo() {
    let default = IntOptions::default();
    assert_eq!(int::<u8>("256", default), Ok(0));
    assert_eq!(int::<u8>("-1", default), Ok(255));
    assert_eq!(int::<i8>("128", default), Ok(-128));
    assert_eq!(int::<i8>("255", default), Ok(-1));
    assert_eq!(int::<u32>("4294967297", default), Ok(1));
    assert_eq!(int::<i32>("2147483648", default), Ok(-2147483648));
    assert_eq!(int::<u16>("65536.7", default), Ok(0));
    assert_eq!(int::<u64>("-1", default), Ok(u64::MAX));
    assert_eq!(int::<u64>("2 ** 64", default), Ok(0));
    assert_eq!(int::<u64>("2 ** 64 + 2 ** 12", default), Ok(4096));
    assert_eq!(int::<i64>("2 ** 63", default), Ok(i64::MIN));
    assert_eq!(
      int::<i64>("-(2 ** 63) - 2 ** 11", default),
      Ok(9223372036854773760)
    );
  }

  #[test]
  fn int_default_maps_non_finite_and_zero_to_zero() {
    let default = IntOptions::default();
    for source in ["NaN", "0", "-0", "Infinity", "-Infinity"] {
      assert_eq!(int::<i64>(source, default), Ok(0), "{source}");
      assert_eq!(int::<u8>(source, default), Ok(0), "{source}");
    }
  }

  #[test]
  fn int_enforce_wins_over_clamp() {
    let both = IntOptions {
      enforce_range: true,
      clamp: true,
    };
    assert_eq!(
      int::<u8>("300", both),
      Err("P: C is outside the range of the type.".into())
    );
  }

  #[test]
  fn float_rejects_non_finite() {
    assert_eq!(
      float::<f64>("NaN"),
      Err("P: C is not a finite number.".into())
    );
    assert_eq!(
      float::<f64>("-Infinity"),
      Err("P: C is not a finite number.".into())
    );
    assert_eq!(
      float::<f64>("'abc'"),
      Err("P: C is not a finite number.".into())
    );
    assert_eq!(
      float::<f32>("1e40"),
      Err("P: C is not a finite number.".into())
    );
    assert_eq!(
      float::<f32>("3.5e38"),
      Err("P: C is not a finite number.".into())
    );
    assert_eq!(
      float::<f64>("({valueOf(){throw new Error('boom')}})"),
      Err("P: C threw an exception: Error: boom".into())
    );
  }

  #[test]
  fn float_keeps_negative_zero_and_rounds_to_f32() {
    let zero = float::<f64>("-0").unwrap();
    assert!(zero.is_sign_negative());
    let zero = float::<f32>("-0").unwrap();
    assert!(zero.is_sign_negative());
    assert_eq!(float::<f32>("1e38"), Ok(1e38f32));
    assert_eq!(float::<f64>("1.5"), Ok(1.5));
    assert_eq!(float::<f32>("0.1"), Ok(0.1f32));
  }

  #[test]
  fn unrestricted_allows_non_finite() {
    let inf = float::<UnrestrictedFloat>("1e40").unwrap();
    assert_eq!(*inf, f32::INFINITY);
    let nan = float::<UnrestrictedFloat>("NaN").unwrap();
    assert!(nan.is_nan());
    let inf = float::<UnrestrictedDouble>("-Infinity").unwrap();
    assert_eq!(*inf, f64::NEG_INFINITY);
    assert_eq!(
      float::<UnrestrictedDouble>("({valueOf(){throw 1}})"),
      Err("P: C threw an exception: 1".into())
    );
  }
}
