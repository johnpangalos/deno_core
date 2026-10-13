// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! WebIDL converters for primitive types: `any`, `boolean`, the integer and
//! floating point types, `DOMString`, `ByteString` and `bigint`.

use std::borrow::Cow;

use super::fail;
use super::to_big_int;
use super::to_dom_string;
use super::to_number;
use super::BigInt;
use super::ByteString;
use super::ContextFn;
use super::IntOptions;
use super::StringOptions;
use super::UnrestrictedDouble;
use super::UnrestrictedFloat;
use super::WebIdlConverter;
use super::WebIdlError;
use super::WebIdlErrorKind;

/// 2^64, the modulus used when wrapping integers.
const TWO_POW_64: f64 = 18446744073709551616.0;

/// The largest integer exactly representable as an f64 (2^53 - 1).
const MAX_SAFE_INTEGER: i128 = (1 << 53) - 1;

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

/// Returns the inclusive `(lower, upper)` bounds used by the integer
/// converters. 64-bit types are limited to the safe integer range.
fn int_bounds(bits: u32, signed: bool) -> (i128, i128) {
  if bits == 64 {
    return if signed {
      (-MAX_SAFE_INTEGER, MAX_SAFE_INTEGER)
    } else {
      (0, MAX_SAFE_INTEGER)
    };
  }
  if signed {
    (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
  } else {
    (0, (1i128 << bits) - 1)
  }
}

/// ConvertToInt from the WebIDL specification. The result is either in range
/// or, in the default mode, reduced modulo 2^64. The caller casts it to the
/// target width, which wraps it to the right number of bits.
fn convert_to_int<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: ContextFn<'_>,
  (bits, signed): (u32, bool),
  options: &IntOptions,
) -> Result<i128, WebIdlError> {
  let x = to_number(scope, value, prefix, context)?;
  let (lower, upper) = int_bounds(bits, signed);

  if options.enforce_range {
    if !x.is_finite() {
      return Err(fail(prefix, context, WebIdlErrorKind::NotFinite));
    }
    let x = x.trunc();
    if x < lower as f64 || x > upper as f64 {
      let kind = WebIdlErrorKind::OutOfRange { lower, upper };
      return Err(fail(prefix, context, kind));
    }
    return Ok(x as i128);
  }

  if options.clamp {
    if x.is_nan() {
      return Ok(0);
    }
    let clamped = x.clamp(lower as f64, upper as f64);
    return Ok(clamped.round_ties_even() as i128);
  }

  if !x.is_finite() {
    return Ok(0);
  }
  // `%` on f64 is an exact fmod, and the result fits in an i128.
  Ok((x.trunc() % TWO_POW_64) as i128)
}

/// Implements [`WebIdlConverter`] for integer types. Each entry is the type
/// followed by its bit width and signedness.
macro_rules! impl_int {
  ($($t:ty => ($bits:expr, $signed:expr)),* $(,)?) => {
    $(
      impl<'a> WebIdlConverter<'a> for $t {
        type Options = IntOptions;

        fn convert<C>(
          scope: &mut v8::HandleScope<'a>,
          value: v8::Local<'a, v8::Value>,
          prefix: Cow<'static, str>,
          context: C,
          options: &Self::Options,
        ) -> Result<Self, WebIdlError>
        where
          C: Fn() -> Cow<'static, str>,
        {
          convert_to_int(
            scope,
            value,
            &prefix,
            &context,
            ($bits, $signed),
            options,
          )
          .map(|v| v as $t)
        }
      }
    )*
  };
}

impl_int!(
  i8 => (8, true),
  u8 => (8, false),
  i16 => (16, true),
  u16 => (16, false),
  i32 => (32, true),
  u32 => (32, false),
  i64 => (64, true),
  u64 => (64, false),
);

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
    let x = to_number(scope, value, &prefix, &context)?;
    if !x.is_finite() {
      return Err(fail(&prefix, &context, WebIdlErrorKind::NotFinite));
    }
    Ok(x)
  }
}

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
    let x = to_number(scope, value, &prefix, &context)?;
    // Rounding to f32 can overflow a finite double to infinity, which is
    // also rejected.
    let y = x as f32;
    if !y.is_finite() {
      return Err(fail(&prefix, &context, WebIdlErrorKind::NotFinite));
    }
    Ok(y)
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
    to_number(scope, value, &prefix, &context).map(UnrestrictedDouble)
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
    to_number(scope, value, &prefix, &context)
      .map(|x| UnrestrictedFloat(x as f32))
  }
}

impl<'a> WebIdlConverter<'a> for String {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    to_dom_string(scope, value, &prefix, &context, options)
  }
}

impl<'a> WebIdlConverter<'a> for ByteString {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let string = to_dom_string(scope, value, &prefix, &context, options)?;
    if let Some(c) = string.chars().find(|c| (*c as u32) > 0xFF) {
      let kind = WebIdlErrorKind::InvalidByteString(c as u32);
      return Err(fail(&prefix, &context, kind));
    }
    Ok(ByteString(string))
  }
}

impl<'a> WebIdlConverter<'a> for BigInt {
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
    to_big_int(scope, value, &prefix, &context)
  }
}
