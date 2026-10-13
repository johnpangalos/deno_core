// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversions from JavaScript values to Rust values, following the WebIDL
//! JavaScript-to-IDL type mapping
//! (<https://webidl.spec.whatwg.org/#js-type-mapping>).
//!
//! Supported converters: `any` (`v8::Local<v8::Value>`), `bool`, the integer
//! types `i8` to `u64`, `f32`/`f64`, [`UnrestrictedFloat`]/[`UnrestrictedDouble`],
//! `String` (DOMString), [`ByteString`], [`BigInt`], `Option<T>` (nullable),
//! `Vec<T>` (sequence) and `HashMap<String, T>` (record).
//!
//! Deliberate departure from the spec: DOMString conversion replaces lone
//! surrogates with U+FFFD, because a Rust `String` cannot hold them.
//!
//! JavaScript that runs during a conversion (`valueOf`, `toString`, getters,
//! iterator `next` methods, proxy traps) can throw. Those exceptions are caught
//! and returned as a [`WebIdlError`] carrying the exception's message. They never
//! panic and never stay pending on the scope.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;

/// A conversion from a JavaScript value into a Rust value, following the
/// WebIDL JavaScript type mapping.
///
/// `prefix` names the API being called (for example `Failed to execute 'fetch'`)
/// and `context` names the argument or member being converted. `context` is only
/// called when an error is built.
pub trait WebIdlConverter<'a>: Sized {
  /// Options that change how the value is converted, for example [`IntOptions`].
  type Options: Default;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>;
}

/// An error produced when a JavaScript value can not be converted.
///
/// Its `Display` output is `"{prefix}: {context} {kind}"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  pub prefix: Cow<'static, str>,
  pub context: Cow<'static, str>,
  pub kind: WebIdlErrorKind,
}

impl WebIdlError {
  pub fn new(
    prefix: Cow<'static, str>,
    context: &impl Fn() -> Cow<'static, str>,
    kind: WebIdlErrorKind,
  ) -> Self {
    Self {
      prefix,
      context: context(),
      kind,
    }
  }
}

impl fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {} {}", self.prefix, self.context, self.kind)
  }
}

impl std::error::Error for WebIdlError {}

/// The reason a conversion failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebIdlErrorKind {
  /// The value can not be converted to the named type.
  ConvertToConverterType(&'static str),
  /// A number that must be finite was NaN or infinite.
  IntNotFinite,
  /// A number was outside the range accepted by `enforce_range`.
  IntRange { lower_bound: i64, upper_bound: i64 },
  /// A ByteString contained a code unit above 0xFF.
  InvalidByteString,
  /// The value is not an object with an iterator.
  NotIterable,
  /// A dictionary member that is required is missing.
  DictionaryCannotConvertKey {
    converter: &'static str,
    key: &'static str,
  },
  /// A string is not one of the enum's values.
  InvalidEnumVariant {
    converter: &'static str,
    variant: String,
  },
  /// A JavaScript exception was thrown during the conversion. Holds its message.
  Other(String),
}

impl fmt::Display for WebIdlErrorKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      WebIdlErrorKind::ConvertToConverterType(converter) => {
        write!(f, "can not be converted to {converter}")
      }
      WebIdlErrorKind::IntNotFinite => write!(f, "is not a finite number"),
      WebIdlErrorKind::IntRange {
        lower_bound,
        upper_bound,
      } => write!(
        f,
        "is outside the accepted range of {lower_bound} to {upper_bound}, inclusive"
      ),
      WebIdlErrorKind::InvalidByteString => {
        write!(f, "is not a valid ByteString")
      }
      WebIdlErrorKind::NotIterable => {
        write!(f, "can not be converted to sequence")
      }
      WebIdlErrorKind::DictionaryCannotConvertKey { converter, key } => write!(
        f,
        "can not be converted to '{converter}' because '{key}' is required in '{converter}'"
      ),
      WebIdlErrorKind::InvalidEnumVariant {
        converter,
        variant,
      } => write!(
        f,
        "can not be converted to '{converter}' because '{variant}' is not a valid enum value"
      ),
      WebIdlErrorKind::Other(message) => {
        write!(f, "threw an exception: {message}")
      }
    }
  }
}

/// Options for the integer converters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntOptions {
  /// Clamp out-of-range numbers to the type's bounds, rounding half to even.
  pub clamp: bool,
  /// Reject NaN, infinities and numbers out of range instead of wrapping.
  /// Takes precedence over `clamp`.
  pub enforce_range: bool,
}

/// Options for the string converters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StringOptions {
  /// Convert `null` to the empty string instead of `"null"`.
  pub treat_null_as_empty_string: bool,
}

/// An `unrestricted float`: any number, including NaN and infinities.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnrestrictedFloat(pub f32);

impl Deref for UnrestrictedFloat {
  type Target = f32;

  fn deref(&self) -> &f32 {
    &self.0
  }
}

/// An `unrestricted double`: any number, including NaN and infinities.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnrestrictedDouble(pub f64);

impl Deref for UnrestrictedDouble {
  type Target = f64;

  fn deref(&self) -> &f64 {
    &self.0
  }
}

/// A `ByteString`: a string whose characters are all in the range U+0000 to U+00FF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteString(pub String);

impl Deref for ByteString {
  type Target = String;

  fn deref(&self) -> &String {
    &self.0
  }
}

/// A `bigint`, stored as a sign and a little-endian magnitude of 64-bit words.
///
/// `sign` is true only for negative values. Zero always has `sign: false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigInt {
  pub sign: bool,
  pub words: Vec<u64>,
}

/// Returns the JavaScript exception caught by `tc` as a [`WebIdlError`].
/// Taking the exception clears it, so nothing stays pending on the scope.
fn caught_error(
  tc: &mut v8::TryCatch<v8::HandleScope>,
  prefix: Cow<'static, str>,
  context: &impl Fn() -> Cow<'static, str>,
) -> WebIdlError {
  let message = match tc.exception() {
    Some(exception) => {
      let message = v8::Exception::create_message(tc, exception);
      let message = message.get(tc).to_rust_string_lossy(tc);
      match message.strip_prefix("Uncaught ") {
        Some(stripped) => stripped.to_string(),
        None => message,
      }
    }
    None => "unknown exception".to_string(),
  };
  WebIdlError::new(prefix, context, WebIdlErrorKind::Other(message))
}

/// Creates a JavaScript string for a property name or message.
fn js_str<'a>(
  scope: &mut v8::HandleScope<'a>,
  text: &str,
) -> v8::Local<'a, v8::String> {
  match v8::String::new(scope, text) {
    Some(string) => string,
    None => v8::String::empty(scope),
  }
}

/// ToNumber. Symbols and BigInts are rejected before JavaScript runs.
fn to_number<C>(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
  prefix: Cow<'static, str>,
  context: &C,
) -> Result<f64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_symbol() || value.is_big_int() {
    return Err(WebIdlError::new(
      prefix,
      context,
      WebIdlErrorKind::ConvertToConverterType("a number"),
    ));
  }
  let tc = &mut v8::TryCatch::new(scope);
  match value.to_number(tc) {
    Some(number) => Ok(number.value()),
    None => Err(caught_error(tc, prefix, context)),
  }
}

/// ToPrimitive with hint "number", for objects only. Tries `valueOf` and then
/// `toString`, the same order as OrdinaryToPrimitive.
fn to_primitive<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: Cow<'static, str>,
  context: &C,
) -> Result<v8::Local<'a, v8::Value>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let Ok(object) = value.try_cast::<v8::Object>() else {
    return Ok(value);
  };
  let tc = &mut v8::TryCatch::new(scope);
  for method in ["valueOf", "toString"] {
    let key = js_str(tc, method);
    let Some(property) = object.get(tc, key.into()) else {
      return Err(caught_error(tc, prefix.clone(), context));
    };
    let Ok(function) = property.try_cast::<v8::Function>() else {
      continue;
    };
    let Some(result) = function.call(tc, value, &[]) else {
      return Err(caught_error(tc, prefix.clone(), context));
    };
    if !result.is_object() {
      return Ok(result);
    }
  }
  Err(WebIdlError::new(
    prefix.clone(),
    context,
    WebIdlErrorKind::ConvertToConverterType("a BigInt"),
  ))
}

/// Converts a JavaScript value to a DOMString. Lone surrogates become U+FFFD.
fn convert_to_string<C>(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
  prefix: Cow<'static, str>,
  context: &C,
  options: &StringOptions,
) -> Result<String, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_symbol() {
    return Err(WebIdlError::new(
      prefix,
      context,
      WebIdlErrorKind::ConvertToConverterType("a string"),
    ));
  }
  if options.treat_null_as_empty_string && value.is_null() {
    return Ok(String::new());
  }
  let tc = &mut v8::TryCatch::new(scope);
  match value.to_string(tc) {
    Some(string) => Ok(string.to_rust_string_lossy(tc)),
    None => Err(caught_error(tc, prefix, context)),
  }
}

/// Bounds of an integer type, used by `enforce_range` and `clamp`.
/// 64-bit types are limited to the integers a JavaScript number represents exactly.
fn int_bounds(bits: u32, signed: bool) -> (i128, i128) {
  if bits == 64 {
    let max_safe = (1i128 << 53) - 1;
    return if signed {
      (-max_safe, max_safe)
    } else {
      (0, max_safe)
    };
  }
  if signed {
    (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
  } else {
    (0, (1i128 << bits) - 1)
  }
}

/// Returns `x` modulo 2^bits, as an unsigned value. `x` must be finite and
/// truncated to an integer.
fn wrap_to_bits(x: f64, bits: u32) -> u64 {
  // fmod is exact, so the remainder has magnitude below 2^bits and is not rounded.
  let remainder = x % 2f64.powi(bits as i32);
  let mask = if bits == 64 {
    u64::MAX
  } else {
    (1u64 << bits) - 1
  };
  if remainder < 0.0 {
    // The magnitude is below 2^64, so the cast is exact. Negating it in
    // wrapping u64 arithmetic gives 2^64 - |r|, and masking leaves 2^bits - |r|.
    ((-remainder) as u64).wrapping_neg() & mask
  } else {
    (remainder as u64) & mask
  }
}

/// The shared integer conversion (ConvertToInt in WebIDL).
fn convert_to_int<C>(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
  prefix: Cow<'static, str>,
  context: &C,
  options: &IntOptions,
  bits: u32,
  signed: bool,
) -> Result<i128, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let number = to_number(scope, value, prefix.clone(), context)?;
  let (lower, upper) = int_bounds(bits, signed);

  if options.enforce_range {
    if !number.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::IntNotFinite,
      ));
    }
    let truncated = number.trunc();
    if !(lower as f64..=upper as f64).contains(&truncated) {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::IntRange {
          lower_bound: lower as i64,
          upper_bound: upper as i64,
        },
      ));
    }
    return Ok(truncated as i128);
  }

  if options.clamp {
    if number.is_nan() {
      return Ok(0);
    }
    let clamped = number.clamp(lower as f64, upper as f64);
    return Ok(clamped.round_ties_even() as i128);
  }

  if !number.is_finite() {
    return Ok(0);
  }
  let raw = wrap_to_bits(number.trunc(), bits);
  Ok(if signed {
    let shift = 64 - bits;
    (((raw << shift) as i64) >> shift) as i128
  } else {
    raw as i128
  })
}

/// Converts a v8 BigInt into a [`BigInt`], normalising zero to a positive sign.
fn bigint_from_v8(big_int: v8::Local<v8::BigInt>) -> BigInt {
  let mut buffer = vec![0u64; big_int.word_count()];
  let (negative, magnitude) = big_int.to_words_array(&mut buffer);
  let words = magnitude.to_vec();
  let sign = negative && words.iter().any(|word| *word != 0);
  BigInt { sign, words }
}

/// Returns the object that a dictionary's members are read from. `undefined`
/// and `null` give `None`, so every member is missing. Used by
/// `#[derive(WebIDL)]`.
pub fn dictionary_object<'a>(
  value: v8::Local<'a, v8::Value>,
  prefix: Cow<'static, str>,
  context: &impl Fn() -> Cow<'static, str>,
) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError> {
  if value.is_null_or_undefined() {
    return Ok(None);
  }
  match value.try_cast::<v8::Object>() {
    Ok(object) => Ok(Some(object)),
    Err(_) => Err(WebIdlError::new(
      prefix,
      context,
      WebIdlErrorKind::ConvertToConverterType("dictionary"),
    )),
  }
}

/// Reads one dictionary member with [[Get]]. Returns `None` when the object is
/// missing or the member is `undefined`. A throwing getter becomes an error.
/// Used by `#[derive(WebIDL)]`.
pub fn dictionary_member<'a>(
  scope: &mut v8::HandleScope<'a>,
  object: Option<v8::Local<'a, v8::Object>>,
  key: &'static str,
  prefix: Cow<'static, str>,
  context: &impl Fn() -> Cow<'static, str>,
) -> Result<Option<v8::Local<'a, v8::Value>>, WebIdlError> {
  let Some(object) = object else {
    return Ok(None);
  };
  let tc = &mut v8::TryCatch::new(scope);
  let key = js_str(tc, key);
  let Some(value) = object.get(tc, key.into()) else {
    return Err(caught_error(tc, prefix, context));
  };
  if value.is_undefined() {
    Ok(None)
  } else {
    Ok(Some(value))
  }
}

impl<'a> WebIdlConverter<'a> for v8::Local<'a, v8::Value> {
  type Options = ();

  fn convert<C>(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &(),
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
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value.boolean_value(scope))
  }
}

macro_rules! impl_int_converter {
  ($($t:ty => ($bits:expr, $signed:expr)),* $(,)?) => {
    $(
      impl<'a> WebIdlConverter<'a> for $t {
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
          convert_to_int(scope, value, prefix, &context, options, $bits, $signed)
            .map(|v| v as $t)
        }
      }
    )*
  };
}

impl_int_converter!(
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
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let number = to_number(scope, value, prefix.clone(), &context)?;
    if !number.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::IntNotFinite,
      ));
    }
    Ok(number)
  }
}

impl<'a> WebIdlConverter<'a> for f32 {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let number = to_number(scope, value, prefix.clone(), &context)?;
    let float = number as f32;
    // A finite double can still round to an infinite f32, such as 1e40.
    if !number.is_finite() || float.is_infinite() {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::IntNotFinite,
      ));
    }
    Ok(float)
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedDouble {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    to_number(scope, value, prefix, &context).map(UnrestrictedDouble)
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedFloat {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    to_number(scope, value, prefix, &context)
      .map(|number| UnrestrictedFloat(number as f32))
  }
}

impl<'a> WebIdlConverter<'a> for String {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &StringOptions,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    convert_to_string(scope, value, prefix, &context, options)
  }
}

impl<'a> WebIdlConverter<'a> for ByteString {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &StringOptions,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    if value.is_symbol() {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("a ByteString"),
      ));
    }
    let string =
      convert_to_string(scope, value, prefix.clone(), &context, options)?;
    // The string is UTF-16 in JavaScript. A lone surrogate becomes U+FFFD here,
    // which is above 0xFF and so is rejected, just as the spec rejects it.
    if string.chars().any(|c| c > '\u{FF}') {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::InvalidByteString,
      ));
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
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let value = to_primitive(scope, value, prefix.clone(), &context)?;
    if let Ok(big_int) = value.try_cast::<v8::BigInt>() {
      return Ok(bigint_from_v8(big_int));
    }
    if value.is_boolean() {
      return Ok(BigInt {
        sign: false,
        words: if value.is_true() { vec![1] } else { vec![] },
      });
    }
    if value.is_string() {
      let tc = &mut v8::TryCatch::new(scope);
      return match value.to_big_int(tc) {
        Some(big_int) => Ok(bigint_from_v8(big_int)),
        None => Err(caught_error(tc, prefix, &context)),
      };
    }
    Err(WebIdlError::new(
      prefix,
      &context,
      WebIdlErrorKind::ConvertToConverterType("a BigInt"),
    ))
  }
}

impl<'a, T> WebIdlConverter<'a> for Option<T>
where
  T: WebIdlConverter<'a>,
{
  type Options = T::Options;

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
    if value.is_null_or_undefined() {
      return Ok(None);
    }
    T::convert(scope, value, prefix, context, options).map(Some)
  }
}

impl<'a, T> WebIdlConverter<'a> for Vec<T>
where
  T: WebIdlConverter<'a>,
{
  type Options = T::Options;

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
    let Ok(object) = value.try_cast::<v8::Object>() else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::NotIterable,
      ));
    };
    let iterator_symbol = v8::Symbol::get_iterator(scope);
    let tc = &mut v8::TryCatch::new(scope);

    let Some(method) = object.get(tc, iterator_symbol.into()) else {
      return Err(caught_error(tc, prefix, &context));
    };
    let Ok(method) = method.try_cast::<v8::Function>() else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::NotIterable,
      ));
    };
    let Some(iterator) = method.call(tc, value, &[]) else {
      return Err(caught_error(tc, prefix, &context));
    };
    let Ok(iterator) = iterator.try_cast::<v8::Object>() else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::NotIterable,
      ));
    };

    // Per the spec, `next` is read once, before the loop starts.
    let next_key = js_str(tc, "next");
    let Some(next) = iterator.get(tc, next_key.into()) else {
      return Err(caught_error(tc, prefix, &context));
    };
    let Ok(next) = next.try_cast::<v8::Function>() else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::NotIterable,
      ));
    };
    let done_key = js_str(tc, "done");
    let value_key = js_str(tc, "value");

    let mut elements = Vec::new();
    loop {
      let Some(result) = next.call(tc, iterator.into(), &[]) else {
        return Err(caught_error(tc, prefix, &context));
      };
      let Ok(result) = result.try_cast::<v8::Object>() else {
        return Err(WebIdlError::new(
          prefix,
          &context,
          WebIdlErrorKind::NotIterable,
        ));
      };
      let Some(done) = result.get(tc, done_key.into()) else {
        return Err(caught_error(tc, prefix, &context));
      };
      if done.boolean_value(tc) {
        break;
      }
      let Some(element) = result.get(tc, value_key.into()) else {
        return Err(caught_error(tc, prefix, &context));
      };
      let index = elements.len();
      let element_context =
        || Cow::Owned(format!("{}, index {}", context(), index));
      elements.push(T::convert(
        tc,
        element,
        prefix.clone(),
        element_context,
        options,
      )?);
    }
    Ok(elements)
  }
}

impl<'a, T> WebIdlConverter<'a> for HashMap<String, T>
where
  T: WebIdlConverter<'a>,
{
  type Options = T::Options;

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
    let Ok(object) = value.try_cast::<v8::Object>() else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("a record"),
      ));
    };
    let tc = &mut v8::TryCatch::new(scope);
    let Some(keys) = object.get_own_property_names(
      tc,
      v8::GetPropertyNamesArgs {
        mode: v8::KeyCollectionMode::OwnOnly,
        property_filter: v8::PropertyFilter::ALL_PROPERTIES,
        index_filter: v8::IndexFilter::IncludeIndices,
        key_conversion: v8::KeyConversionMode::ConvertToString,
      },
    ) else {
      return Err(caught_error(tc, prefix, &context));
    };
    let enumerable_key = js_str(tc, "enumerable");

    let mut record = HashMap::new();
    for index in 0..keys.length() {
      let Some(key) = keys.get_index(tc, index) else {
        return Err(caught_error(tc, prefix, &context));
      };
      let Ok(name) = key.try_cast::<v8::Name>() else {
        continue;
      };
      let Some(descriptor) = object.get_own_property_descriptor(tc, name)
      else {
        return Err(caught_error(tc, prefix, &context));
      };
      let Ok(descriptor) = descriptor.try_cast::<v8::Object>() else {
        continue;
      };
      let Some(enumerable) = descriptor.get(tc, enumerable_key.into()) else {
        return Err(caught_error(tc, prefix, &context));
      };
      if !enumerable.is_true() {
        continue;
      }
      if key.is_symbol() {
        return Err(WebIdlError::new(
          prefix,
          &context,
          WebIdlErrorKind::ConvertToConverterType("a string"),
        ));
      }
      let key_name = String::convert(
        tc,
        key,
        prefix.clone(),
        &context,
        &StringOptions::default(),
      )?;
      let Some(property) = object.get(tc, key) else {
        return Err(caught_error(tc, prefix, &context));
      };
      let entry = T::convert(
        tc,
        property,
        prefix.clone(),
        || Cow::Owned(format!("{}, key '{}'", context(), key_name)),
        options,
      )?;
      record.insert(key_name, entry);
    }
    Ok(record)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;
  use crate::WebIDL;

  const PREFIX: &str = "Failed to execute 'test'";

  /// Evaluates `source` and converts the result as a single argument.
  fn convert_js<'a, T>(
    scope: &mut v8::HandleScope<'a>,
    source: &str,
    options: &T::Options,
  ) -> Result<T, WebIdlError>
  where
    T: WebIdlConverter<'a>,
  {
    let value = eval(scope, source);
    T::convert(scope, value, PREFIX.into(), || "Argument 1".into(), options)
  }

  fn eval<'a>(
    scope: &mut v8::HandleScope<'a>,
    source: &str,
  ) -> v8::Local<'a, v8::Value> {
    let source = v8::String::new(scope, source).unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    script.run(scope).unwrap()
  }

  /// Returns the error message, failing the test if the conversion succeeded.
  fn message<T>(result: Result<T, WebIdlError>) -> String {
    match result {
      Ok(_) => panic!("expected the conversion to fail"),
      Err(error) => error.to_string(),
    }
  }

  fn arg_error(description: &str) -> String {
    format!("{PREFIX}: Argument 1 {description}")
  }

  #[test]
  fn error_display_is_exact() {
    let error = WebIdlError::new(
      "Failed to execute 'fetch'".into(),
      &|| "Argument 1".into(),
      WebIdlErrorKind::IntNotFinite,
    );
    assert_eq!(
      error.to_string(),
      "Failed to execute 'fetch': Argument 1 is not a finite number"
    );
    fn assert_send_sync<T: Send + Sync + 'static + std::error::Error>() {}
    assert_send_sync::<WebIdlError>();
  }

  #[test]
  fn context_is_only_built_on_error() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let value = eval(scope, "5");
    let result = u8::convert(
      scope,
      value,
      PREFIX.into(),
      || -> Cow<'static, str> { panic!("context built on success") },
      &IntOptions::default(),
    );
    assert_eq!(result.unwrap(), 5);
  }

  #[test]
  fn any_returns_value_unchanged() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let value = convert_js::<v8::Local<v8::Value>>(scope, "'x'", &()).unwrap();
    assert!(value.is_string());
  }

  #[test]
  fn bool_is_to_boolean() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    for source in ["0", "''", "null", "undefined", "NaN", "0n"] {
      assert!(!convert_js::<bool>(scope, source, &()).unwrap(), "{source}");
    }
    for source in ["'false'", "({})", "[]", "1"] {
      assert!(convert_js::<bool>(scope, source, &()).unwrap(), "{source}");
    }
  }

  #[test]
  fn int_defaults() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions::default();
    assert_eq!(convert_js::<u8>(scope, "256", &options).unwrap(), 0);
    assert_eq!(convert_js::<u8>(scope, "-1", &options).unwrap(), 255);
    assert_eq!(convert_js::<u8>(scope, "257.9", &options).unwrap(), 1);
    assert_eq!(convert_js::<i8>(scope, "128", &options).unwrap(), -128);
    assert_eq!(convert_js::<i8>(scope, "255", &options).unwrap(), -1);
    assert_eq!(convert_js::<i8>(scope, "-129", &options).unwrap(), 127);
    assert_eq!(convert_js::<i16>(scope, "32768", &options).unwrap(), -32768);
    assert_eq!(
      convert_js::<u32>(scope, "2 ** 32 + 5", &options).unwrap(),
      5
    );
    assert_eq!(
      convert_js::<i64>(scope, "2 ** 63", &options).unwrap(),
      i64::MIN
    );
    assert_eq!(convert_js::<u64>(scope, "-1", &options).unwrap(), u64::MAX);
    assert_eq!(
      convert_js::<u64>(scope, "1e20", &options).unwrap(),
      7766279631452241920
    );
    assert_eq!(
      convert_js::<u64>(scope, "-3", &options).unwrap(),
      u64::MAX - 2
    );
    for source in ["NaN", "Infinity", "-Infinity", "-0"] {
      assert_eq!(convert_js::<u16>(scope, source, &options).unwrap(), 0);
    }
    assert_eq!(convert_js::<u8>(scope, "'  12 '", &options).unwrap(), 12);
    assert_eq!(convert_js::<u8>(scope, "'abc'", &options).unwrap(), 0);
    assert_eq!(convert_js::<u8>(scope, "null", &options).unwrap(), 0);
    assert_eq!(convert_js::<u8>(scope, "true", &options).unwrap(), 1);
    assert_eq!(convert_js::<u8>(scope, "undefined", &options).unwrap(), 0);
  }

  #[test]
  fn int_default_errors() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions::default();
    assert_eq!(
      message(convert_js::<u8>(scope, "1n", &options)),
      arg_error("can not be converted to a number")
    );
    assert_eq!(
      message(convert_js::<i32>(scope, "Symbol()", &options)),
      arg_error("can not be converted to a number")
    );
    let thrown = message(convert_js::<u8>(
      scope,
      "({valueOf(){throw new Error('boom')}})",
      &options,
    ));
    assert!(thrown.starts_with(PREFIX), "{thrown}");
    assert!(thrown.contains("boom"), "{thrown}");
    // The exception was swallowed, so the scope can run more code.
    let value = eval(scope, "1 + 1");
    assert_eq!(value.number_value(scope), Some(2.0));
  }

  #[test]
  fn int_enforce_range() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions {
      enforce_range: true,
      clamp: false,
    };
    assert_eq!(
      message(convert_js::<u8>(scope, "256", &options)),
      arg_error("is outside the accepted range of 0 to 255, inclusive")
    );
    assert!(convert_js::<u8>(scope, "-1", &options).is_err());
    assert_eq!(convert_js::<u8>(scope, "255.9", &options).unwrap(), 255);
    assert_eq!(convert_js::<u8>(scope, "-0.9", &options).unwrap(), 0);
    assert_eq!(
      message(convert_js::<u8>(scope, "NaN", &options)),
      arg_error("is not a finite number")
    );
    assert_eq!(
      message(convert_js::<u8>(scope, "Infinity", &options)),
      arg_error("is not a finite number")
    );
    assert!(convert_js::<i64>(scope, "2 ** 53", &options).is_err());
    assert_eq!(
      convert_js::<i64>(scope, "2 ** 53 - 1", &options).unwrap(),
      (1i64 << 53) - 1
    );
    assert!(convert_js::<u64>(scope, "-1", &options).is_err());
    let both = IntOptions {
      enforce_range: true,
      clamp: true,
    };
    assert!(convert_js::<u8>(scope, "300", &both).is_err());
  }

  #[test]
  fn int_clamp() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions {
      enforce_range: false,
      clamp: true,
    };
    assert_eq!(convert_js::<u8>(scope, "300", &options).unwrap(), 255);
    assert_eq!(convert_js::<u8>(scope, "-5", &options).unwrap(), 0);
    assert_eq!(convert_js::<u8>(scope, "2.5", &options).unwrap(), 2);
    assert_eq!(convert_js::<u8>(scope, "3.5", &options).unwrap(), 4);
    assert_eq!(convert_js::<u8>(scope, "NaN", &options).unwrap(), 0);
    assert_eq!(convert_js::<u8>(scope, "Infinity", &options).unwrap(), 255);
    assert_eq!(convert_js::<i8>(scope, "-2.5", &options).unwrap(), -2);
    assert_eq!(convert_js::<i8>(scope, "-200", &options).unwrap(), -128);
    assert_eq!(
      convert_js::<i64>(scope, "1e20", &options).unwrap(),
      (1i64 << 53) - 1
    );
    assert_eq!(
      convert_js::<i64>(scope, "-1e20", &options).unwrap(),
      -((1i64 << 53) - 1)
    );
    assert_eq!(convert_js::<u64>(scope, "-Infinity", &options).unwrap(), 0);
  }

  #[test]
  fn int_symbol_and_bigint_are_rejected() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions {
      enforce_range: false,
      clamp: true,
    };
    assert!(convert_js::<u8>(scope, "1n", &options).is_err());
    assert!(convert_js::<u8>(scope, "Symbol()", &options).is_err());
  }

  #[test]
  fn floats() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      message(convert_js::<f64>(scope, "NaN", &())),
      arg_error("is not a finite number")
    );
    assert!(convert_js::<f64>(scope, "Infinity", &()).is_err());
    assert_eq!(convert_js::<f64>(scope, "'1.5'", &()).unwrap(), 1.5);
    assert!(convert_js::<f32>(scope, "1e40", &()).is_err());
    assert_eq!(convert_js::<f32>(scope, "3.4e38", &()).unwrap(), 3.4e38f32);
    assert_eq!(
      message(convert_js::<f32>(scope, "NaN", &())),
      arg_error("is not a finite number")
    );
    assert_eq!(
      convert_js::<UnrestrictedFloat>(scope, "1e40", &())
        .unwrap()
        .0,
      f32::INFINITY
    );
    assert!(convert_js::<UnrestrictedDouble>(scope, "NaN", &())
      .unwrap()
      .is_nan());
    assert_eq!(
      convert_js::<UnrestrictedDouble>(scope, "-Infinity", &())
        .unwrap()
        .0,
      f64::NEG_INFINITY
    );
    assert!(convert_js::<UnrestrictedFloat>(scope, "NaN", &())
      .unwrap()
      .is_nan());
    assert!(convert_js::<UnrestrictedFloat>(scope, "1n", &()).is_err());
    assert!(convert_js::<f64>(scope, "1n", &()).is_err());
    assert!(convert_js::<f32>(scope, "Symbol()", &()).is_err());
    assert!(convert_js::<UnrestrictedDouble>(scope, "Symbol()", &()).is_err());
  }

  #[test]
  fn strings() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = StringOptions::default();
    let empty = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(
      message(convert_js::<String>(scope, "Symbol()", &options)),
      arg_error("can not be converted to a string")
    );
    assert_eq!(
      convert_js::<String>(scope, "null", &options).unwrap(),
      "null"
    );
    assert_eq!(convert_js::<String>(scope, "null", &empty).unwrap(), "");
    assert_eq!(
      convert_js::<String>(scope, "undefined", &options).unwrap(),
      "undefined"
    );
    assert_eq!(convert_js::<String>(scope, "1.5", &options).unwrap(), "1.5");
    assert_eq!(
      convert_js::<String>(scope, "'\\u{1F600}'", &options).unwrap(),
      "\u{1F600}"
    );
    assert_eq!(
      convert_js::<String>(scope, "'\\uD800'", &options).unwrap(),
      "\u{FFFD}"
    );
    let thrown = message(convert_js::<String>(
      scope,
      "({toString(){throw new Error('boom')}})",
      &options,
    ));
    assert!(thrown.contains("boom"), "{thrown}");
  }

  #[test]
  fn byte_strings() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = StringOptions::default();
    assert_eq!(
      message(convert_js::<ByteString>(scope, "'\\u0100'", &options)),
      arg_error("is not a valid ByteString")
    );
    assert_eq!(
      convert_js::<ByteString>(scope, "'\\xFF'", &options).unwrap(),
      ByteString("\u{FF}".to_string())
    );
    assert_eq!(
      convert_js::<ByteString>(scope, "'\\xE9'", &options)
        .unwrap()
        .as_str(),
      "\u{E9}"
    );
    assert_eq!(
      convert_js::<ByteString>(scope, "'\\0'", &options)
        .unwrap()
        .0,
      "\0"
    );
    assert!(convert_js::<ByteString>(scope, "'\\uD800'", &options).is_err());
    assert_eq!(
      message(convert_js::<ByteString>(scope, "Symbol()", &options)),
      arg_error("can not be converted to a ByteString")
    );
  }

  #[test]
  fn bigints() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      convert_js::<BigInt>(scope, "({valueOf(){return 5n}})", &()).unwrap(),
      BigInt {
        sign: false,
        words: vec![5]
      }
    );
    let thrown = message(convert_js::<BigInt>(
      scope,
      "({valueOf(){throw new Error('boom')}})",
      &(),
    ));
    assert!(thrown.contains("boom"), "{thrown}");
    let zero = convert_js::<BigInt>(scope, "0n", &()).unwrap();
    assert!(!zero.sign);
    assert!(zero.words.iter().all(|word| *word == 0));
    assert_eq!(
      convert_js::<BigInt>(scope, "-1n", &()).unwrap(),
      BigInt {
        sign: true,
        words: vec![1]
      }
    );
    assert_eq!(
      convert_js::<BigInt>(scope, "2n ** 64n", &()).unwrap(),
      BigInt {
        sign: false,
        words: vec![0, 1]
      }
    );
    assert_eq!(
      convert_js::<BigInt>(scope, "-(2n ** 128n)", &()).unwrap(),
      BigInt {
        sign: true,
        words: vec![0, 0, 1]
      }
    );
    assert_eq!(
      convert_js::<BigInt>(scope, "true", &()).unwrap(),
      BigInt {
        sign: false,
        words: vec![1]
      }
    );
    assert_eq!(
      convert_js::<BigInt>(scope, "false", &()).unwrap(),
      convert_js::<BigInt>(scope, "0n", &()).unwrap()
    );
    assert_eq!(
      convert_js::<BigInt>(scope, "'123'", &()).unwrap(),
      BigInt {
        sign: false,
        words: vec![123]
      }
    );
    assert!(convert_js::<BigInt>(scope, "'abc'", &()).is_err());
    assert_eq!(
      message(convert_js::<BigInt>(scope, "1", &())),
      arg_error("can not be converted to a BigInt")
    );
    assert!(convert_js::<BigInt>(scope, "null", &()).is_err());
    assert!(convert_js::<BigInt>(scope, "undefined", &()).is_err());
    assert!(convert_js::<BigInt>(scope, "Symbol()", &()).is_err());
  }

  #[test]
  fn options_wrapper() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions::default();
    assert_eq!(
      convert_js::<Option<u8>>(scope, "null", &options).unwrap(),
      None
    );
    assert_eq!(
      convert_js::<Option<u8>>(scope, "undefined", &options).unwrap(),
      None
    );
    assert_eq!(
      convert_js::<Option<u8>>(scope, "5", &options).unwrap(),
      Some(5)
    );
  }

  #[test]
  fn sequences() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions::default();
    assert_eq!(
      message(convert_js::<Vec<u8>>(scope, "'abc'", &options)),
      arg_error("can not be converted to sequence")
    );
    assert_eq!(
      message(convert_js::<Vec<u8>>(scope, "({length: 2})", &options)),
      arg_error("can not be converted to sequence")
    );
    assert_eq!(
      message(convert_js::<Vec<u8>>(scope, "null", &options)),
      arg_error("can not be converted to sequence")
    );
    assert_eq!(
      convert_js::<Vec<Option<u8>>>(scope, "[1,,3]", &options).unwrap(),
      vec![Some(1), None, Some(3)]
    );
    assert_eq!(
      convert_js::<Vec<u8>>(scope, "new Set([1, 2])", &options).unwrap(),
      vec![1, 2]
    );
    assert_eq!(
      convert_js::<Vec<u8>>(
        scope,
        "(function* () { yield 1; yield 2; })()",
        &options
      )
      .unwrap(),
      vec![1, 2]
    );
    let custom = "({[Symbol.iterator]() { let i = 0; return { next() { \
                  i++; return { done: i > 2, value: i * 10 }; } }; }})";
    assert_eq!(
      convert_js::<Vec<u8>>(scope, custom, &options).unwrap(),
      vec![10, 20]
    );
    let throwing = "({[Symbol.iterator]() { return { next() { \
                    throw new Error('boom'); } }; }})";
    let thrown = message(convert_js::<Vec<u8>>(scope, throwing, &options));
    assert!(thrown.contains("boom"), "{thrown}");
    let element = message(convert_js::<Vec<f64>>(scope, "[1, 'x']", &()));
    assert_eq!(
      element,
      format!("{PREFIX}: Argument 1, index 1 is not a finite number")
    );
    assert!(element.contains(", index 1"));
  }

  #[test]
  fn records() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions::default();
    assert_eq!(
      message(convert_js::<HashMap<String, u8>>(scope, "null", &options)),
      arg_error("can not be converted to a record")
    );
    assert!(convert_js::<HashMap<String, u8>>(scope, "5", &options).is_err());
    assert_eq!(
      convert_js::<HashMap<String, u8>>(scope, "({a: 1, b: 2})", &options)
        .unwrap(),
      HashMap::from([("a".to_string(), 1), ("b".to_string(), 2)])
    );
    assert_eq!(
      convert_js::<HashMap<String, u8>>(scope, "[1, 2]", &options).unwrap(),
      HashMap::from([("0".to_string(), 1), ("1".to_string(), 2)])
    );
    assert_eq!(
      convert_js::<HashMap<String, u8>>(
        scope,
        "Object.defineProperty({a: 1}, 'hidden', {value: 2, enumerable: false})",
        &options
      )
      .unwrap(),
      HashMap::from([("a".to_string(), 1)])
    );
    assert_eq!(
      message(convert_js::<HashMap<String, u8>>(
        scope,
        "({[Symbol('x')]: 1})",
        &options
      )),
      arg_error("can not be converted to a string")
    );
    let getter = message(convert_js::<HashMap<String, u8>>(
      scope,
      "({get a() { throw new Error('boom'); }})",
      &options,
    ));
    assert!(getter.contains("boom"), "{getter}");
    let proxy = message(convert_js::<HashMap<String, u8>>(
      scope,
      "new Proxy({a: 1}, {getOwnPropertyDescriptor() { throw new Error('boom'); }})",
      &options,
    ));
    assert!(proxy.contains("boom"), "{proxy}");
  }

  #[allow(dead_code)]
  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Defaults {
    #[webidl(default = 7)]
    a: u8,
    b: Option<u8>,
  }

  #[allow(dead_code)]
  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Plain {
    a: u8,
  }

  #[allow(dead_code)]
  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Required {
    #[webidl(required)]
    key: u8,
  }

  #[allow(dead_code)]
  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Order {
    #[webidl(default = 0)]
    a: u8,
    #[webidl(default = 0)]
    aa: u8,
    #[webidl(default = 0)]
    b: u8,
  }

  #[allow(dead_code)]
  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Raw {
    r#type: String,
  }

  fn unreachable_default() -> u8 {
    panic!("the default was evaluated");
  }

  #[allow(dead_code)]
  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Lazy {
    #[webidl(default = unreachable_default())]
    a: u8,
  }

  #[allow(dead_code)]
  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Clamped {
    #[options(clamp = true)]
    a: u8,
  }

  #[allow(dead_code)]
  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(enum)]
  enum Choice {
    FooBar,
    #[webidl(rename = "custom")]
    Renamed,
  }

  /// Returns the keys that the getter log recorded, joined by commas.
  fn log_of(scope: &mut v8::HandleScope) -> String {
    eval(scope, "log.join(',')").to_rust_string_lossy(scope)
  }

  #[test]
  fn dictionary_null_and_undefined_make_members_missing() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let defaults = Defaults { a: 7, b: None };
    assert_eq!(
      convert_js::<Defaults>(scope, "null", &()).unwrap(),
      defaults
    );
    assert_eq!(
      convert_js::<Defaults>(scope, "undefined", &()).unwrap(),
      defaults
    );
    assert_eq!(
      convert_js::<Defaults>(scope, "({a: undefined, b: null})", &()).unwrap(),
      defaults
    );
    assert_eq!(
      convert_js::<Defaults>(scope, "({b: 3})", &()).unwrap(),
      Defaults { a: 7, b: Some(3) }
    );
  }

  #[test]
  fn dictionary_rejects_non_objects() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    for source in ["5", "'x'", "true", "Symbol()", "1n"] {
      assert_eq!(
        message(convert_js::<Defaults>(scope, source, &())),
        arg_error("can not be converted to dictionary"),
        "{source}"
      );
    }
  }

  #[test]
  fn dictionary_required_and_missing_members_name_the_key() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      message(convert_js::<Required>(scope, "({})", &())),
      arg_error(
        "can not be converted to 'Required' because 'key' is required in 'Required'"
      )
    );
    assert_eq!(
      message(convert_js::<Required>(scope, "({key: undefined})", &())),
      arg_error(
        "can not be converted to 'Required' because 'key' is required in 'Required'"
      )
    );
    assert_eq!(
      message(convert_js::<Plain>(scope, "null", &())),
      arg_error(
        "can not be converted to 'Plain' because 'a' is required in 'Plain'"
      )
    );
  }

  #[test]
  fn dictionary_reads_members_in_key_order_once() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let source = "(function () {
      const log = [];
      globalThis.log = log;
      const object = {};
      for (const key of ['b', 'a', 'aa']) {
        Object.defineProperty(object, key, {
          enumerable: true,
          get() { log.push(key); return 1; },
        });
      }
      return object;
    })()";
    assert_eq!(
      convert_js::<Order>(scope, source, &()).unwrap(),
      Order { a: 1, aa: 1, b: 1 }
    );
    assert_eq!(log_of(scope), "a,aa,b");
  }

  #[test]
  fn dictionary_reads_inherited_members_and_raw_identifiers() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      convert_js::<Plain>(scope, "Object.create({a: 4})", &()).unwrap(),
      Plain { a: 4 }
    );
    assert_eq!(
      convert_js::<Raw>(scope, "({type: 'x'})", &()).unwrap(),
      Raw {
        r#type: "x".to_string()
      }
    );
  }

  #[test]
  fn dictionary_default_is_only_evaluated_when_missing() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      convert_js::<Lazy>(scope, "({a: 2})", &()).unwrap(),
      Lazy { a: 2 }
    );
  }

  #[test]
  fn dictionary_getter_errors_and_member_context() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      message(convert_js::<Plain>(
        scope,
        "({get a() { throw new Error('boom'); }})",
        &()
      )),
      format!(
        "{PREFIX}: 'a' of 'Plain' (Argument 1) threw an exception: Error: boom"
      )
    );
    let member = message(convert_js::<Plain>(scope, "({a: Symbol()})", &()));
    assert!(
      member.starts_with(&format!("{PREFIX}: 'a' of 'Plain' (Argument 1) "))
    );
  }

  #[test]
  fn dictionary_options_clamp_members() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      convert_js::<Clamped>(scope, "({a: 300})", &()).unwrap(),
      Clamped { a: 255 }
    );
  }

  #[test]
  fn enum_matches_kebab_case_values_exactly() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      convert_js::<Choice>(scope, "'foo-bar'", &()).unwrap(),
      Choice::FooBar
    );
    assert_eq!(
      convert_js::<Choice>(scope, "'custom'", &()).unwrap(),
      Choice::Renamed
    );
    assert_eq!(
      convert_js::<Choice>(scope, "({toString() { return 'foo-bar'; }})", &())
        .unwrap(),
      Choice::FooBar
    );
    for (source, value) in [
      ("'FooBar'", "FooBar"),
      ("'foo_bar'", "foo_bar"),
      ("''", ""),
      ("'renamed'", "renamed"),
      ("1", "1"),
    ] {
      assert_eq!(
        message(convert_js::<Choice>(scope, source, &())),
        arg_error(&format!(
          "can not be converted to 'Choice' because '{value}' is not a valid enum value"
        )),
        "{source}"
      );
    }
    assert_eq!(
      message(convert_js::<Choice>(scope, "Symbol()", &())),
      arg_error("can not be converted to a string")
    );
  }
}
