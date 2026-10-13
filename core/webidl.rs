// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversions from JavaScript values to Rust values, following the
//! [WebIDL JavaScript-to-IDL type mapping](https://webidl.spec.whatwg.org/#js-type-mapping).
//!
//! The [`WebIdlConverter`] trait is implemented for the IDL types that map
//! onto Rust types. Use the `#[webidl]` attribute on an
//! [`op2`][deno_core::op2] argument to convert it automatically, or call
//! [`WebIdlConverter::convert`] directly. Dictionaries and enumerations can be
//! described with `#[derive(WebIDL)]`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;

use crate::v8;

/// The largest integer that can be represented exactly by a JavaScript Number.
/// WebIDL uses this as the bound for `[EnforceRange]` and `[Clamp]` 64-bit
/// integer conversions.
const MAX_SAFE_INTEGER: i128 = (1 << 53) - 1;

/// 2^64 as an `f64`, used to reduce a Number modulo 2^64 without losing
/// precision.
const TWO_POW_64: f64 = 18446744073709551616.0;

/// An error produced when a JavaScript value can not be converted to a Rust
/// value.
///
/// The message is `"{prefix}: {context} {what}"`, for example
/// `"Failed to execute 'fetch': Argument 1 is not a finite number"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  prefix: Cow<'static, str>,
  context: Cow<'static, str>,
  what: Cow<'static, str>,
}

impl WebIdlError {
  /// Builds an error from the function `prefix`, the value `context` and a
  /// description of what was wrong. `context` is only invoked here, so
  /// building the context string costs nothing on the success path.
  pub fn new<C>(
    prefix: Cow<'static, str>,
    context: C,
    what: impl Into<Cow<'static, str>>,
  ) -> Self
  where
    C: FnOnce() -> Cow<'static, str>,
  {
    Self {
      prefix,
      context: context(),
      what: what.into(),
    }
  }
}

impl fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {} {}", self.prefix, self.context, self.what)
  }
}

impl std::error::Error for WebIdlError {}

/// Converts a JavaScript value to a Rust value using the WebIDL conversion
/// rules.
///
/// `prefix` names the function being called (for example
/// `"Failed to execute 'op_foo'"`), and `context` describes the value being
/// converted (for example `"Argument 1"`). Both are used to build the message
/// of a [`WebIdlError`].
pub trait WebIdlConverter<'a>: Sized {
  /// Options that change how the value is converted. Use `Default::default()`
  /// for the standard behavior.
  type Options: Default;

  /// Converts `value` to `Self`, or returns a [`WebIdlError`] describing why
  /// the value is not valid.
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

/// Options for integer conversions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  /// The `[Clamp]` extended attribute: out-of-range values are clamped to the
  /// range of the type, and fractional values are rounded to the nearest
  /// integer with ties going to the even integer. NaN becomes 0.
  ///
  /// When `enforce_range` is also set, `enforce_range` wins: the two
  /// attributes can't both apply, so the conversion behaves as if only
  /// `enforce_range` were set.
  pub clamp: bool,
  /// The `[EnforceRange]` extended attribute: NaN and infinities throw, values
  /// are truncated toward zero, and values outside the range of the type throw
  /// instead of wrapping around.
  pub enforce_range: bool,
}

/// Options for string conversions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  /// The `[LegacyNullToEmptyString]` extended attribute: `null` converts to
  /// the empty string instead of `"null"`. `undefined` still converts to
  /// `"undefined"`.
  pub treat_null_as_empty_string: bool,
}

/// An `unrestricted float`: accepts NaN and infinities, and values too large
/// for an `f32` become infinity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnrestrictedFloat(
  /// The converted value.
  pub f32,
);

impl Deref for UnrestrictedFloat {
  type Target = f32;

  fn deref(&self) -> &f32 {
    &self.0
  }
}

/// An `unrestricted double`: accepts NaN and infinities.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnrestrictedDouble(
  /// The converted value.
  pub f64,
);

impl Deref for UnrestrictedDouble {
  type Target = f64;

  fn deref(&self) -> &f64 {
    &self.0
  }
}

/// A `ByteString`: a string whose code units are all in the range 0 to 255.
/// Each code unit is stored as the `char` with the same value, so `"\xff"` in
/// JavaScript is stored as `"\u{ff}"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteString(
  /// The converted string. Every char is at most U+00FF.
  pub String,
);

impl Deref for ByteString {
  type Target = String;

  fn deref(&self) -> &String {
    &self.0
  }
}

/// A JavaScript `BigInt`, split into a sign and a magnitude.
///
/// `sign` is `true` for negative values. `words` holds the magnitude as 64-bit
/// words, least significant first, so `2n ** 64n` is `words: [0, 1]`. Zero is
/// `sign: false` with an empty `words` vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigInt {
  /// `true` if the value is negative.
  pub sign: bool,
  /// The magnitude as 64-bit words, least significant first. Empty for zero.
  pub words: Vec<u64>,
}

impl Deref for BigInt {
  type Target = Vec<u64>;

  fn deref(&self) -> &Vec<u64> {
    &self.words
  }
}

/// Builds an error whose context is `context()`.
fn error<C>(
  prefix: &str,
  context: &C,
  what: impl Into<Cow<'static, str>>,
) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  WebIdlError::new(Cow::Owned(prefix.to_owned()), context, what)
}

/// The error for a V8 call that returned `None`, which only happens when user
/// code threw while the value was being converted.
fn thrown<C>(prefix: &str, context: &C) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  error(prefix, context, "threw an exception during conversion")
}

/// ToNumber, rejecting the values that have no Number conversion.
fn to_number<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<f64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_symbol() {
    return Err(error(
      prefix,
      context,
      "is a Symbol and can not be converted to a number",
    ));
  }
  if value.is_big_int() {
    return Err(error(
      prefix,
      context,
      "is a BigInt and can not be converted to a number",
    ));
  }
  match value.to_number(scope) {
    Some(number) => Ok(number.value()),
    None => Err(thrown(prefix, context)),
  }
}

/// ConvertToInt from the WebIDL spec. Returns the converted integer, or the
/// description of why the value is out of range.
fn convert_to_int(
  number: f64,
  bits: u32,
  signed: bool,
  options: &IntOptions,
) -> Result<i128, Cow<'static, str>> {
  // 64-bit integers use the bounds the spec gives for JavaScript Numbers. The
  // wrapping conversion still uses the full 64-bit modulus.
  let (lower, upper): (i128, i128) = if bits == 64 {
    if signed {
      (-MAX_SAFE_INTEGER, MAX_SAFE_INTEGER)
    } else {
      (0, MAX_SAFE_INTEGER)
    }
  } else if signed {
    let half = 1i128 << (bits - 1);
    (-half, half - 1)
  } else {
    (0, (1i128 << bits) - 1)
  };

  if options.enforce_range {
    if !number.is_finite() {
      return Err("is not a finite number".into());
    }
    let truncated = number.trunc();
    if truncated < lower as f64 || truncated > upper as f64 {
      return Err(format!("is outside the range [{lower}, {upper}]").into());
    }
    return Ok(truncated as i128);
  }

  if options.clamp {
    if number.is_nan() {
      return Ok(0);
    }
    // Round ties to even. The clamped value is an integer, so the cast is exact.
    let clamped = number.clamp(lower as f64, upper as f64).round_ties_even();
    return Ok(clamped as i128);
  }

  if !number.is_finite() {
    return Ok(0);
  }
  // Reduce modulo 2^64 in f64 (exact), then take the result modulo 2^bits in
  // integer arithmetic. 2^bits divides 2^64, so this is the same as reducing
  // the original value modulo 2^bits.
  let modulo = 1i128 << bits;
  let reduced = ((number.trunc() % TWO_POW_64) as i128).rem_euclid(modulo);
  if signed && reduced >= modulo / 2 {
    Ok(reduced - modulo)
  } else {
    Ok(reduced)
  }
}

impl<'a> WebIdlConverter<'a> for v8::Local<'a, v8::Value> {
  type Options = ();

  /// Returns the value unchanged.
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

  /// ToBoolean. Never fails.
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

macro_rules! impl_webidl_int {
  ($($ty:ty => ($bits:expr, $signed:expr)),* $(,)?) => {
    $(
      impl<'a> WebIdlConverter<'a> for $ty {
        type Options = IntOptions;

        /// ConvertToInt, with the wrapping, `[EnforceRange]` and `[Clamp]`
        /// behavior selected by the options.
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
          let number = to_number(scope, value, &prefix, &context)?;
          let int = convert_to_int(number, $bits, $signed, options)
            .map_err(|what| error(&prefix, &context, what))?;
          Ok(int as $ty)
        }
      }
    )*
  };
}

impl_webidl_int! {
  i8 => (8, true),
  u8 => (8, false),
  i16 => (16, true),
  u16 => (16, false),
  i32 => (32, true),
  u32 => (32, false),
  i64 => (64, true),
  u64 => (64, false),
}

impl<'a> WebIdlConverter<'a> for f64 {
  type Options = ();

  /// A `double`: rejects NaN and the infinities.
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
      return Err(error(&prefix, &context, "is not a finite number"));
    }
    Ok(number)
  }
}

impl<'a> WebIdlConverter<'a> for f32 {
  type Options = ();

  /// A `float`: rejects NaN, the infinities, and finite values that round to
  /// an infinity as an `f32`.
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
      return Err(error(&prefix, &context, "is not a finite number"));
    }
    let float = number as f32;
    if !float.is_finite() {
      return Err(error(
        &prefix,
        &context,
        "is outside the range of a 32-bit float",
      ));
    }
    Ok(float)
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedDouble {
  type Options = ();

  /// An `unrestricted double`: accepts NaN and the infinities.
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

  /// An `unrestricted float`: accepts NaN and the infinities. Finite values
  /// too large for an `f32` become infinity.
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
    Ok(UnrestrictedFloat(number as f32))
  }
}

impl<'a> WebIdlConverter<'a> for String {
  type Options = StringOptions;

  /// A `DOMString`: ToString. Lone surrogates become U+FFFD.
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
    if options.treat_null_as_empty_string && value.is_null() {
      return Ok(String::new());
    }
    if value.is_symbol() {
      return Err(error(
        &prefix,
        &context,
        "is a Symbol and can not be converted to a string",
      ));
    }
    match value.to_string(scope) {
      Some(string) => Ok(string.to_rust_string_lossy(scope)),
      None => Err(thrown(&prefix, &context)),
    }
  }
}

impl<'a> WebIdlConverter<'a> for ByteString {
  type Options = StringOptions;

  /// A `ByteString`: a DOMString in which every code unit is at most 255.
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
    let string = <String as WebIdlConverter<'a>>::convert(
      scope,
      value,
      prefix.clone(),
      &context,
      options,
    )?;
    let mut bytes = String::with_capacity(string.len());
    for unit in string.encode_utf16() {
      if unit > 255 {
        return Err(error(
          &prefix,
          &context,
          "is not a valid ByteString: it contains a code unit greater than 255",
        ));
      }
      bytes.push(char::from(unit as u8));
    }
    Ok(ByteString(bytes))
  }
}

impl<'a> WebIdlConverter<'a> for BigInt {
  type Options = ();

  /// A `bigint`, using ToBigInt. Numbers, `undefined`, `null` and Symbols are
  /// rejected, but booleans and numeric strings are converted.
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
    if value.is_null_or_undefined() || value.is_number() || value.is_symbol() {
      return Err(error(&prefix, &context, "can not be converted to a BigInt"));
    }
    let Some(big) = value.to_big_int(scope) else {
      return Err(thrown(&prefix, &context));
    };
    let mut words = vec![0u64; big.word_count()];
    let (sign, words) = big.to_words_array(&mut words);
    Ok(BigInt {
      sign,
      words: words.to_vec(),
    })
  }
}

impl<'a, T> WebIdlConverter<'a> for Option<T>
where
  T: WebIdlConverter<'a>,
{
  type Options = <T as WebIdlConverter<'a>>::Options;

  /// A nullable type: `null` and `undefined` become `None`, anything else is
  /// converted as `T`.
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
  type Options = <T as WebIdlConverter<'a>>::Options;

  /// A `sequence<T>`: the values produced by the iterator of `value`. Each
  /// element is converted as `T`, and errors name the element's index.
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
    if !value.is_object() {
      return Err(error(
        &prefix,
        &context,
        "is not an object and can not be converted to a sequence",
      ));
    }
    let not_iterable = || {
      error(
        &prefix,
        &context,
        "is not iterable and can not be converted to a sequence",
      )
    };
    let object =
      v8::Local::<v8::Object>::try_from(value).map_err(|_| not_iterable())?;
    let iterator_key = v8::Symbol::get_iterator(scope);
    let method = object
      .get(scope, iterator_key.into())
      .ok_or_else(|| thrown(&prefix, &context))?;
    let method = v8::Local::<v8::Function>::try_from(method)
      .map_err(|_| not_iterable())?;
    let iterator = method
      .call(scope, value, &[])
      .ok_or_else(|| thrown(&prefix, &context))?;
    let iterator =
      v8::Local::<v8::Object>::try_from(iterator).map_err(|_| {
        error(
          &prefix,
          &context,
          "is not iterable: its iterator is not an object",
        )
      })?;

    let next_key = v8::String::new(scope, "next")
      .ok_or_else(|| thrown(&prefix, &context))?;
    let next = iterator
      .get(scope, next_key.into())
      .ok_or_else(|| thrown(&prefix, &context))?;
    let next = v8::Local::<v8::Function>::try_from(next).map_err(|_| {
      error(
        &prefix,
        &context,
        "is not iterable: its next is not a function",
      )
    })?;
    let done_key = v8::String::new(scope, "done")
      .ok_or_else(|| thrown(&prefix, &context))?;
    let value_key = v8::String::new(scope, "value")
      .ok_or_else(|| thrown(&prefix, &context))?;

    let mut items = Vec::new();
    loop {
      let result = next
        .call(scope, iterator.into(), &[])
        .ok_or_else(|| thrown(&prefix, &context))?;
      let result = v8::Local::<v8::Object>::try_from(result).map_err(|_| {
        error(
          &prefix,
          &context,
          "is not iterable: an iterator result is not an object",
        )
      })?;
      let done = result
        .get(scope, done_key.into())
        .ok_or_else(|| thrown(&prefix, &context))?;
      if done.boolean_value(scope) {
        break;
      }
      let item = result
        .get(scope, value_key.into())
        .ok_or_else(|| thrown(&prefix, &context))?;
      let index = items.len();
      let item_context =
        || Cow::Owned(format!("{}, index {}", context(), index));
      items.push(T::convert(
        scope,
        item,
        prefix.clone(),
        item_context,
        options,
      )?);
    }
    Ok(items)
  }
}

impl<'a, T> WebIdlConverter<'a> for HashMap<String, T>
where
  T: WebIdlConverter<'a>,
{
  type Options = <T as WebIdlConverter<'a>>::Options;

  /// A `record<DOMString, T>`: the enumerable own string-keyed properties of
  /// `value`. Each value is read with Get and converted as `T`, and errors
  /// name the key.
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
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(error(
        &prefix,
        &context,
        "is not an object and can not be converted to a record",
      ));
    };
    let keys = object
      .get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
          mode: v8::KeyCollectionMode::OwnOnly,
          property_filter: v8::PropertyFilter::ONLY_ENUMERABLE
            | v8::PropertyFilter::SKIP_SYMBOLS,
          index_filter: v8::IndexFilter::IncludeIndices,
          key_conversion: v8::KeyConversionMode::ConvertToString,
        },
      )
      .ok_or_else(|| thrown(&prefix, &context))?;

    let mut map = HashMap::with_capacity(keys.length() as usize);
    for i in 0..keys.length() {
      let key_value = keys
        .get_index(scope, i)
        .ok_or_else(|| thrown(&prefix, &context))?;
      let key = <String as WebIdlConverter<'a>>::convert(
        scope,
        key_value,
        prefix.clone(),
        &context,
        &StringOptions::default(),
      )?;
      let key_context = || Cow::Owned(format!("{}, key '{}'", context(), key));
      let property = object
        .get(scope, key_value)
        .ok_or_else(|| thrown(&prefix, &key_context))?;
      let converted =
        T::convert(scope, property, prefix.clone(), key_context, options)?;
      map.insert(key, converted);
    }
    Ok(map)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::op2;
  use crate::JsRuntime;
  use crate::RuntimeOptions;
  use crate::WebIDL;
  use pretty_assertions::assert_eq;

  fn eval<'s>(
    scope: &mut v8::HandleScope<'s>,
    code: &str,
  ) -> v8::Local<'s, v8::Value> {
    let source = v8::String::new(scope, code).unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    script.run(scope).unwrap()
  }

  /// Evaluates `code` and converts it as `T` with the prefix "Test" and the
  /// context "Argument 1".
  fn convert_js<'s, T>(
    scope: &mut v8::HandleScope<'s>,
    code: &str,
    options: &T::Options,
  ) -> Result<T, WebIdlError>
  where
    T: WebIdlConverter<'s>,
  {
    let value = eval(scope, code);
    T::convert(
      scope,
      value,
      Cow::Borrowed("Test"),
      || Cow::Borrowed("Argument 1"),
      options,
    )
  }

  fn int_flags(clamp: bool, enforce_range: bool) -> IntOptions {
    IntOptions {
      clamp,
      enforce_range,
    }
  }

  fn message<T>(result: Result<T, WebIdlError>) -> String {
    match result {
      Ok(_) => panic!("expected an error"),
      Err(error) => error.to_string(),
    }
  }

  #[test]
  fn test_error_display_and_lazy_context() {
    let mut called = false;
    let error = WebIdlError::new(
      Cow::Borrowed("Failed to execute 'fetch'"),
      || {
        called = true;
        Cow::Borrowed("Argument 1")
      },
      "is not a finite number",
    );
    assert!(called);
    assert_eq!(
      error.to_string(),
      "Failed to execute 'fetch': Argument 1 is not a finite number"
    );
    let _: &dyn std::error::Error = &error;
  }

  #[test]
  fn test_newtypes_deref() {
    assert_eq!(*UnrestrictedFloat(1.5), 1.5f32);
    assert_eq!(*UnrestrictedDouble(2.5), 2.5f64);
    assert_eq!(*ByteString("abc".into()), "abc");
    assert_eq!(
      BigInt {
        sign: false,
        words: vec![7]
      }
      .len(),
      1
    );
  }

  #[test]
  fn test_any_and_bool() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let value = eval(scope, "({a: 1})");
    let same = <v8::Local<v8::Value> as WebIdlConverter>::convert(
      scope,
      value,
      Cow::Borrowed("Test"),
      || Cow::Borrowed("Argument 1"),
      &(),
    )
    .unwrap();
    assert!(same.strict_equals(value));

    for code in ["0", "''", "null", "undefined", "NaN", "0n"] {
      assert!(!convert_js::<bool>(scope, code, &()).unwrap(), "{code}");
    }
    for code in ["({})", "'false'", "[]", "-0.5", "1n"] {
      assert!(convert_js::<bool>(scope, code, &()).unwrap(), "{code}");
    }
  }

  #[test]
  fn test_int_default_wraps() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = &IntOptions::default();
    assert_eq!(convert_js::<u8>(scope, "256", o).unwrap(), 0);
    assert_eq!(convert_js::<u8>(scope, "-1", o).unwrap(), 255);
    assert_eq!(convert_js::<u8>(scope, "257.9", o).unwrap(), 1);
    assert_eq!(convert_js::<i8>(scope, "128", o).unwrap(), -128);
    assert_eq!(convert_js::<i8>(scope, "255", o).unwrap(), -1);
    assert_eq!(convert_js::<u32>(scope, "2**32+5", o).unwrap(), 5);
    assert_eq!(convert_js::<u32>(scope, "-1", o).unwrap(), 4294967295);
    assert_eq!(convert_js::<u64>(scope, "-1", o).unwrap(), u64::MAX);
    assert_eq!(convert_js::<i64>(scope, "2**63", o).unwrap(), i64::MIN);
    assert_eq!(
      convert_js::<u64>(scope, "2**53+2", o).unwrap(),
      9007199254740994
    );
    for code in ["NaN", "0", "-0", "Infinity", "-Infinity"] {
      assert_eq!(convert_js::<i32>(scope, code, o).unwrap(), 0, "{code}");
    }
  }

  #[test]
  fn test_int_to_number_first() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = &IntOptions::default();
    assert_eq!(convert_js::<u16>(scope, "'42'", o).unwrap(), 42);
    assert_eq!(convert_js::<u16>(scope, "true", o).unwrap(), 1);
    assert_eq!(
      convert_js::<u16>(scope, "({valueOf() { return 7; }})", o).unwrap(),
      7
    );
    assert_eq!(
      message(convert_js::<u16>(scope, "Symbol('x')", o)),
      "Test: Argument 1 is a Symbol and can not be converted to a number"
    );
    assert_eq!(
      message(convert_js::<i16>(scope, "1n", o)),
      "Test: Argument 1 is a BigInt and can not be converted to a number"
    );
  }

  #[test]
  fn test_int_enforce_range() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = int_flags(false, true);
    assert_eq!(convert_js::<u8>(scope, "255.9", &o).unwrap(), 255);
    assert_eq!(
      message(convert_js::<u8>(scope, "256", &o)),
      "Test: Argument 1 is outside the range [0, 255]"
    );
    assert_eq!(
      message(convert_js::<u8>(scope, "-1", &o)),
      "Test: Argument 1 is outside the range [0, 255]"
    );
    assert_eq!(
      message(convert_js::<u8>(scope, "NaN", &o)),
      "Test: Argument 1 is not a finite number"
    );
    assert_eq!(
      message(convert_js::<i32>(scope, "-Infinity", &o)),
      "Test: Argument 1 is not a finite number"
    );
    assert_eq!(
      message(convert_js::<u64>(scope, "2**53", &o)),
      "Test: Argument 1 is outside the range [0, 9007199254740991]"
    );
    assert_eq!(
      message(convert_js::<i64>(scope, "2**53", &o)),
      "Test: Argument 1 is outside the range [-9007199254740991, 9007199254740991]"
    );
    // Both flags: enforce_range wins.
    let both = int_flags(true, true);
    assert_eq!(
      message(convert_js::<u8>(scope, "256", &both)),
      "Test: Argument 1 is outside the range [0, 255]"
    );
  }

  #[test]
  fn test_int_clamp() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = int_flags(true, false);
    assert_eq!(convert_js::<u8>(scope, "1.5", &o).unwrap(), 2);
    assert_eq!(convert_js::<u8>(scope, "2.5", &o).unwrap(), 2);
    assert_eq!(convert_js::<u8>(scope, "300", &o).unwrap(), 255);
    assert_eq!(convert_js::<u8>(scope, "-5", &o).unwrap(), 0);
    assert_eq!(convert_js::<u8>(scope, "Infinity", &o).unwrap(), 255);
    assert_eq!(convert_js::<u8>(scope, "NaN", &o).unwrap(), 0);
    assert_eq!(convert_js::<i8>(scope, "-0.5", &o).unwrap(), 0);
    assert_eq!(convert_js::<i8>(scope, "-1.5", &o).unwrap(), -2);
    assert_eq!(
      convert_js::<u64>(scope, "1e300", &o).unwrap(),
      9007199254740991
    );
    assert_eq!(
      convert_js::<i64>(scope, "-1e300", &o).unwrap(),
      -MAX_SAFE_INTEGER as i64
    );
  }

  #[test]
  fn test_float() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(convert_js::<f64>(scope, "1.5", &()).unwrap(), 1.5);
    assert_eq!(
      message(convert_js::<f64>(scope, "NaN", &())),
      "Test: Argument 1 is not a finite number"
    );
    assert_eq!(
      message(convert_js::<f64>(scope, "-Infinity", &())),
      "Test: Argument 1 is not a finite number"
    );
    assert_eq!(convert_js::<f32>(scope, "'2.5'", &()).unwrap(), 2.5);
    assert_eq!(
      message(convert_js::<f32>(scope, "1e40", &())),
      "Test: Argument 1 is outside the range of a 32-bit float"
    );
    assert_eq!(
      message(convert_js::<f32>(scope, "Infinity", &())),
      "Test: Argument 1 is not a finite number"
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
    assert_eq!(
      convert_js::<UnrestrictedFloat>(scope, "1e40", &())
        .unwrap()
        .0,
      f32::INFINITY
    );
    assert!(convert_js::<UnrestrictedFloat>(scope, "NaN", &())
      .unwrap()
      .is_nan());
  }

  #[test]
  fn test_string() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = &StringOptions::default();
    assert_eq!(convert_js::<String>(scope, "5", o).unwrap(), "5");
    assert_eq!(convert_js::<String>(scope, "null", o).unwrap(), "null");
    assert_eq!(
      convert_js::<String>(scope, "undefined", o).unwrap(),
      "undefined"
    );
    assert_eq!(
      convert_js::<String>(scope, "({toString() { return 'x'; }})", o).unwrap(),
      "x"
    );
    assert_eq!(
      convert_js::<String>(scope, "'\\uD800'", o).unwrap(),
      "\u{FFFD}"
    );
    assert_eq!(
      message(convert_js::<String>(scope, "Symbol('x')", o)),
      "Test: Argument 1 is a Symbol and can not be converted to a string"
    );
    let empty = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(convert_js::<String>(scope, "null", &empty).unwrap(), "");
    assert_eq!(
      convert_js::<String>(scope, "undefined", &empty).unwrap(),
      "undefined"
    );
  }

  #[test]
  fn test_byte_string() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = &StringOptions::default();
    assert_eq!(
      convert_js::<ByteString>(scope, "'\\xff'", o).unwrap(),
      ByteString("\u{ff}".into())
    );
    assert_eq!(
      convert_js::<ByteString>(scope, "''", o).unwrap(),
      ByteString(String::new())
    );
    assert_eq!(
      message(convert_js::<ByteString>(scope, "'€'", o)),
      "Test: Argument 1 is not a valid ByteString: it contains a code unit greater than 255"
    );
    let empty = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(
      convert_js::<ByteString>(scope, "null", &empty).unwrap(),
      ByteString(String::new())
    );
  }

  #[test]
  fn test_bigint() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      convert_js::<BigInt>(scope, "-5n", &()).unwrap(),
      BigInt {
        sign: true,
        words: vec![5]
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
      convert_js::<BigInt>(scope, "0n", &()).unwrap(),
      BigInt {
        sign: false,
        words: vec![]
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
      convert_js::<BigInt>(scope, "'10'", &()).unwrap(),
      BigInt {
        sign: false,
        words: vec![10]
      }
    );
    for code in ["5", "undefined", "null", "Symbol('x')"] {
      assert!(convert_js::<BigInt>(scope, code, &()).is_err(), "{code}");
    }
    assert_eq!(
      message(convert_js::<BigInt>(scope, "5", &())),
      "Test: Argument 1 can not be converted to a BigInt"
    );
  }

  #[test]
  fn test_option() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      convert_js::<Option<u8>>(scope, "null", &IntOptions::default()).unwrap(),
      None
    );
    assert_eq!(
      convert_js::<Option<u8>>(scope, "undefined", &IntOptions::default())
        .unwrap(),
      None
    );
    assert_eq!(
      convert_js::<Option<u8>>(scope, "3", &IntOptions::default()).unwrap(),
      Some(3)
    );
  }

  #[test]
  fn test_sequence() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = &IntOptions::default();
    assert_eq!(
      convert_js::<Vec<u8>>(scope, "[1, 2, 3]", o).unwrap(),
      vec![1, 2, 3]
    );
    assert_eq!(
      convert_js::<Vec<u8>>(scope, "[]", o).unwrap(),
      Vec::<u8>::new()
    );
    assert_eq!(
      convert_js::<Vec<u8>>(scope, "[1,,3]", o).unwrap(),
      vec![1, 0, 3]
    );
    assert_eq!(
      convert_js::<Vec<u8>>(scope, "new Set([4, 5])", o).unwrap(),
      vec![4, 5]
    );
    assert_eq!(
      convert_js::<Vec<u8>>(scope, "(function*() { yield 6; yield 7; })()", o)
        .unwrap(),
      vec![6, 7]
    );
    assert_eq!(
      convert_js::<Vec<u8>>(
        scope,
        "({[Symbol.iterator]() { let i = 0; return { next() { i++; return i > 2 ? { done: true } : { value: i * 10 }; } }; } })",
        o
      )
      .unwrap(),
      vec![10, 20]
    );
    for code in [
      "({length: 2, 0: 1, 1: 2})",
      "5",
      "'abc'",
      "null",
      "undefined",
    ] {
      assert!(convert_js::<Vec<u8>>(scope, code, o).is_err(), "{code}");
    }
    assert_eq!(
      message(convert_js::<Vec<u8>>(
        scope,
        "[1, 2, 300]",
        &int_flags(false, true)
      )),
      "Test: Argument 1, index 2 is outside the range [0, 255]"
    );
    assert_eq!(
      message(convert_js::<Vec<u8>>(
        scope,
        "({[Symbol.iterator]() { return { next() { throw new Error('boom'); } }; } })",
        o
      )),
      "Test: Argument 1 threw an exception during conversion"
    );
  }

  #[test]
  fn test_record() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let o = &IntOptions::default();
    let map = convert_js::<HashMap<String, u8>>(
      scope,
      "({a: 1, b: 2, [Symbol('s')]: 3})",
      o,
    )
    .unwrap();
    assert_eq!(map.len(), 2);
    assert_eq!(map["a"], 1);
    assert_eq!(map["b"], 2);

    // Inherited and non-enumerable properties are excluded.
    let map = convert_js::<HashMap<String, u8>>(
      scope,
      "(() => { const o = Object.create({x: 1}); o.y = 2; Object.defineProperty(o, 'hidden', {value: 3, enumerable: false}); return o; })()",
      o,
    )
    .unwrap();
    assert_eq!(map.keys().collect::<Vec<_>>(), vec!["y"]);

    // Getters are invoked.
    let map =
      convert_js::<HashMap<String, u8>>(scope, "({get g() { return 9; }})", o)
        .unwrap();
    assert_eq!(map["g"], 9);

    assert!(convert_js::<HashMap<String, u8>>(scope, "5", o).is_err());
    assert_eq!(
      message(convert_js::<HashMap<String, u8>>(
        scope,
        "({a: 'x'})",
        &int_flags(false, true)
      )),
      "Test: Argument 1, key 'a' is not a finite number"
    );
  }

  #[test]
  fn test_js_exception_is_error() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let scope = &mut v8::TryCatch::new(scope);
    let o = &IntOptions::default();
    assert_eq!(
      message(convert_js::<u8>(
        scope,
        "({valueOf() { throw new Error('boom'); }})",
        o
      )),
      "Test: Argument 1 threw an exception during conversion"
    );
    assert_eq!(
      message(convert_js::<String>(
        scope,
        "({toString() { throw new Error('boom'); }})",
        &StringOptions::default()
      )),
      "Test: Argument 1 threw an exception during conversion"
    );
    assert_eq!(
      message(convert_js::<HashMap<String, u8>>(
        scope,
        "({get a() { throw new Error('boom'); }})",
        o
      )),
      "Test: Argument 1, key 'a' threw an exception during conversion"
    );
  }

  #[derive(WebIDL)]
  #[webidl(dictionary)]
  struct Dict<'a> {
    foo_bar: u8,
    #[webidl(default = 5)]
    count: u8,
    #[webidl(required)]
    name: String,
    #[options(clamp = true)]
    clamped: Option<u8>,
    #[webidl(rename = "other", default = String::from("x"))]
    renamed: String,
    value: Option<v8::Local<'a, v8::Value>>,
  }

  #[derive(Debug, PartialEq, WebIDL)]
  #[webidl(enum)]
  enum Choice {
    FooBar,
    #[webidl(rename = "plain")]
    Other,
  }

  #[derive(Debug, PartialEq, WebIDL)]
  #[webidl(dictionary)]
  struct Inner {
    x: u8,
  }

  #[derive(Debug, PartialEq, WebIDL)]
  #[webidl(dictionary)]
  struct Outer {
    choice: Option<Choice>,
    list: Vec<Choice>,
    inner: Option<Inner>,
    map: Option<HashMap<String, Inner>>,
  }

  #[test]
  fn test_dictionary_members() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let d = convert_js::<Dict>(
      scope,
      "({fooBar: 1, name: 'n', clamped: 300, other: 'o', value: 7})",
      &(),
    )
    .unwrap();
    assert_eq!(d.foo_bar, 1);
    assert_eq!(d.count, 5);
    assert_eq!(d.name, "n");
    assert_eq!(d.clamped, Some(255));
    assert_eq!(d.renamed, "o");
    assert!(d.value.unwrap().is_number());

    let d = convert_js::<Dict>(scope, "({fooBar: 1, name: 'n'})", &()).unwrap();
    assert_eq!(d.count, 5);
    assert_eq!(d.renamed, "x");
    assert_eq!(d.clamped, None);
    assert!(d.value.is_none());

    // null is a value, not missing: Option members become None and a
    // non-nullable u8 converts null with ToNumber.
    let d = convert_js::<Dict>(
      scope,
      "({fooBar: null, name: 'n', clamped: null, value: null})",
      &(),
    )
    .unwrap();
    assert_eq!(d.foo_bar, 0);
    assert_eq!(d.clamped, None);
    assert!(d.value.is_none());
  }

  #[test]
  fn test_dictionary_errors() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let scope = &mut v8::TryCatch::new(scope);
    assert_eq!(
      message(convert_js::<Dict>(scope, "({fooBar: 1})", &())),
      "Test: Argument 1 can not be converted to 'Dict' because 'name' is required in 'Dict'"
    );
    assert_eq!(
      message(convert_js::<Dict>(scope, "null", &())),
      "Test: Argument 1 can not be converted to 'Dict' because 'fooBar' is required in 'Dict'"
    );
    for code in ["5", "'s'", "true"] {
      assert_eq!(
        message(convert_js::<Dict>(scope, code, &())),
        "Test: Argument 1 can not be converted to 'Dict' because it is not an object",
        "{code}"
      );
    }
    assert_eq!(
      message(convert_js::<Dict>(
        scope,
        "({fooBar: 1, name: 'n', count: Symbol('s')})",
        &()
      )),
      "Test: Argument 1 for member 'count' of 'Dict' is a Symbol and can not be converted to a number"
    );
    assert_eq!(
      message(convert_js::<Dict>(
        scope,
        "({fooBar: 1, get name() { throw new Error('boom'); }})",
        &()
      )),
      "Test: Argument 1 for member 'name' of 'Dict' threw an exception during conversion"
    );
  }

  #[test]
  fn test_dictionary_get_order() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    eval(scope, "globalThis.log = [];");
    let code = "({get value() { log.push('value'); return 1; }, get other() { log.push('other'); return 'o'; }, get name() { log.push('name'); return 'n'; }, get fooBar() { log.push('fooBar'); return 1; }, get count() { log.push('count'); return 2; }, get clamped() { log.push('clamped'); return 3; }})";
    convert_js::<Dict>(scope, code, &()).unwrap();
    let log = eval(scope, "log.join(',')");
    assert_eq!(
      log.to_rust_string_lossy(scope),
      "clamped,count,fooBar,name,other,value"
    );
  }

  #[test]
  fn test_nested_derived_types() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let outer = convert_js::<Outer>(
      scope,
      "({choice: 'plain', list: ['plain', 'foo-bar'], inner: {x: 3}, map: {a: {x: 1}}})",
      &(),
    )
    .unwrap();
    assert_eq!(outer.choice, Some(Choice::Other));
    assert_eq!(outer.list, vec![Choice::Other, Choice::FooBar]);
    assert_eq!(outer.inner, Some(Inner { x: 3 }));
    assert_eq!(
      outer.map,
      Some(HashMap::from([("a".to_string(), Inner { x: 1 })]))
    );

    // A sequence member can't be null.
    assert!(
      convert_js::<Outer>(scope, "({choice: null, list: null})", &(),).is_err()
    );
  }

  #[test]
  fn test_enum() {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let scope = &mut runtime.handle_scope();
    let scope = &mut v8::TryCatch::new(scope);
    assert_eq!(
      convert_js::<Choice>(scope, "'foo-bar'", &()).unwrap(),
      Choice::FooBar
    );
    assert_eq!(
      convert_js::<Choice>(scope, "'plain'", &()).unwrap(),
      Choice::Other
    );
    for value in ["Foo-Bar", "foo_bar", "FooBar"] {
      assert_eq!(
        message(convert_js::<Choice>(scope, &format!("'{value}'"), &())),
        format!(
          "Test: Argument 1 is not a valid value for enumeration 'Choice': '{value}'"
        ),
      );
    }
    assert_eq!(
      message(convert_js::<Choice>(scope, "Symbol('s')", &())),
      "Test: Argument 1 is a Symbol and can not be converted to a string"
    );
    assert_eq!(
      convert_js::<Option<Choice>>(scope, "null", &()).unwrap(),
      None
    );
  }

  #[derive(Debug, WebIDL)]
  #[webidl(dictionary)]
  struct OpDict {
    #[webidl(default = 1)]
    foo_bar: u8,
  }

  #[derive(Debug, WebIDL)]
  #[webidl(enum)]
  enum OpMode {
    ReadOnly,
    #[webidl(rename = "rw")]
    ReadWrite,
  }

  #[op2]
  #[string]
  fn op_webidl_test(
    #[webidl] x: u8,
    #[webidl] dict: OpDict,
    #[webidl] mode: OpMode,
  ) -> String {
    format!("{x} {} {mode:?}", dict.foo_bar)
  }

  #[op2(async)]
  #[allow(clippy::unused_async)]
  async fn op_webidl_async(#[webidl] x: u8) -> u8 {
    x
  }

  deno_core::extension!(
    webidl_test_ext,
    ops = [op_webidl_test, op_webidl_async]
  );

  fn run_js(runtime: &mut JsRuntime, code: &str) -> String {
    let result = runtime.execute_script("test.js", code.to_string()).unwrap();
    let scope = &mut runtime.handle_scope();
    let value = v8::Local::new(scope, result);
    value.to_string(scope).unwrap().to_rust_string_lossy(scope)
  }

  /// Runs `code` and returns the message of the exception it throws, or
  /// "no error" if it doesn't throw.
  fn js_error(runtime: &mut JsRuntime, call: &str) -> String {
    run_js(
      runtime,
      &format!(
        "(() => {{ try {{ {call}; return 'no error'; }} catch (e) {{ return `${{e.name}}: ${{e.message}}`; }} }})()"
      ),
    )
  }

  #[test]
  fn test_op_webidl_arguments() {
    let mut runtime = JsRuntime::new(RuntimeOptions {
      extensions: vec![webidl_test_ext::init_ops()],
      ..Default::default()
    });
    assert_eq!(
      run_js(
        &mut runtime,
        "Deno.core.ops.op_webidl_test(300, {fooBar: 2}, 'rw')"
      ),
      "44 2 ReadWrite"
    );
    assert_eq!(
      run_js(
        &mut runtime,
        "Deno.core.ops.op_webidl_test(5, null, 'read-only')"
      ),
      "5 1 ReadOnly"
    );
    assert_eq!(
      js_error(&mut runtime, "Deno.core.ops.op_webidl_test(Symbol(), {}, 'rw')"),
      "TypeError: Failed to execute 'op_webidl_test': Argument 1 is a Symbol and can not be converted to a number"
    );
    assert_eq!(
      js_error(&mut runtime, "Deno.core.ops.op_webidl_test(5, 'x', 'rw')"),
      "TypeError: Failed to execute 'op_webidl_test': Argument 2 can not be converted to 'OpDict' because it is not an object"
    );
    assert_eq!(
      js_error(&mut runtime, "Deno.core.ops.op_webidl_test(5, {}, 'nope')"),
      "TypeError: Failed to execute 'op_webidl_test': Argument 3 is not a valid value for enumeration 'OpMode': 'nope'"
    );
  }

  #[tokio::test]
  async fn test_op_webidl_async_arguments() {
    let mut runtime = JsRuntime::new(RuntimeOptions {
      extensions: vec![webidl_test_ext::init_ops()],
      ..Default::default()
    });
    runtime
      .execute_script(
        "test.js",
        "globalThis.result = 'pending'; \
         Deno.core.ops.op_webidl_async(300).then((v) => { globalThis.result = `ok ${v}`; }); \
         Deno.core.ops.op_webidl_async(Symbol()).catch((e) => { globalThis.error = `${e.name}: ${e.message}`; });",
      )
      .unwrap();
    runtime.run_event_loop(Default::default()).await.unwrap();
    assert_eq!(run_js(&mut runtime, "globalThis.result"), "ok 44");
    assert_eq!(
      run_js(&mut runtime, "globalThis.error"),
      "TypeError: Failed to execute 'op_webidl_async': Argument 1 is a Symbol and can not be converted to a number"
    );
  }
}
