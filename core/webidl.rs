// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversion of JavaScript values to WebIDL types, following the
//! JavaScript-to-IDL algorithms of the WebIDL specification
//! (<https://webidl.spec.whatwg.org/#js-type-mapping>).
//!
//! Ops can take WebIDL arguments with `#[webidl]`, and dictionaries and enums
//! can implement [`WebIdlConverter`] with `#[derive(WebIDL)]`.
//!
//! When the conversion runs user code that throws (for example a `valueOf` or
//! a getter), the exception is rethrown, as the specification requires, and
//! the converter returns a [`WebIdlError`] whose kind is
//! [`WebIdlErrorKind::JsException`].

// Prefixes are passed around as `&Cow` so that cloning a borrowed one doesn't
// allocate.
#![allow(clippy::ptr_arg)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;

/// An error produced while converting a JavaScript value to a WebIDL type.
#[derive(Debug)]
pub struct WebIdlError {
  /// Where the conversion happened, for example "Failed to execute 'fetch'".
  pub prefix: Cow<'static, str>,
  /// What was being converted, for example "Argument 1".
  pub context: Cow<'static, str>,
  /// What went wrong.
  pub kind: WebIdlErrorKind,
}

impl WebIdlError {
  pub fn new(
    prefix: Cow<'static, str>,
    context: impl Fn() -> Cow<'static, str>,
    kind: WebIdlErrorKind,
  ) -> Self {
    Self {
      prefix,
      context: context(),
      kind,
    }
  }

  pub fn other<T: std::error::Error + Send + Sync + 'static>(
    prefix: Cow<'static, str>,
    context: impl Fn() -> Cow<'static, str>,
    other: T,
  ) -> Self {
    Self::new(prefix, context, WebIdlErrorKind::Other(Box::new(other)))
  }

  /// Whether a JavaScript exception was thrown during the conversion. That
  /// exception has been rethrown in the scope that was passed to the
  /// converter, so callers should not throw another one.
  pub fn is_js_exception(&self) -> bool {
    matches!(self.kind, WebIdlErrorKind::JsException)
  }
}

impl fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {} ", self.prefix, self.context)?;
    match &self.kind {
      WebIdlErrorKind::ConvertToConverterType(ty) => {
        write!(f, "can not be converted to {ty}")
      }
      WebIdlErrorKind::NotFinite => write!(f, "is not a finite number"),
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
      WebIdlErrorKind::DictionaryCannotConvertKey { converter, key } => {
        write!(
          f,
          "can not be converted to '{converter}' because '{key}' is required in '{converter}'"
        )
      }
      WebIdlErrorKind::InvalidEnumVariant { converter, variant } => {
        write!(
          f,
          "can not be converted to '{converter}': '{variant}' is not a valid enum value"
        )
      }
      WebIdlErrorKind::JsException => {
        write!(f, "threw an exception while being converted")
      }
      WebIdlErrorKind::Other(other) => write!(f, "{other}"),
    }
  }
}

impl std::error::Error for WebIdlError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match &self.kind {
      WebIdlErrorKind::Other(other) => Some(&**other),
      _ => None,
    }
  }
}

#[derive(Debug)]
pub enum WebIdlErrorKind {
  /// The value is not of a kind that can be converted to the given type.
  ConvertToConverterType(&'static str),
  /// The value is NaN or infinite, and the type does not allow it.
  NotFinite,
  /// The value is outside the range allowed by `[EnforceRange]`.
  IntRange {
    lower_bound: f64,
    upper_bound: f64,
  },
  /// The string contains a code unit above 255.
  InvalidByteString,
  /// A required dictionary member is missing.
  DictionaryCannotConvertKey {
    converter: &'static str,
    key: &'static str,
  },
  /// The string is not one of the enumeration's values.
  InvalidEnumVariant {
    converter: &'static str,
    variant: String,
  },
  /// JavaScript code run by the conversion threw an exception, which has been
  /// rethrown.
  JsException,
  Other(Box<dyn std::error::Error + Send + Sync + 'static>),
}

/// Converts a JavaScript value to a WebIDL type.
pub trait WebIdlConverter<'a>: Sized {
  /// Extended attributes that change how the value is converted, such as
  /// `[Clamp]`.
  type Options: Default;

  /// Converts `value`. `prefix` and `context` describe where the value came
  /// from (for example "Failed to execute 'fetch'" and "Argument 1"), and
  /// are used in error messages.
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

/// Runs a V8 operation that may run JavaScript. If it throws, the exception is
/// rethrown and a [`WebIdlErrorKind::JsException`] error is returned.
fn throwable<'a, T>(
  scope: &mut v8::HandleScope<'a>,
  prefix: &Cow<'static, str>,
  context: &impl Fn() -> Cow<'static, str>,
  f: impl FnOnce(&mut v8::HandleScope<'a>) -> Option<T>,
) -> Result<T, WebIdlError> {
  let tc = &mut v8::TryCatch::new(scope);
  match f(tc) {
    Some(value) => Ok(value),
    None => {
      if tc.has_caught() {
        tc.rethrow();
      }
      Err(WebIdlError::new(
        prefix.clone(),
        context,
        WebIdlErrorKind::JsException,
      ))
    }
  }
}

fn get<'a>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<'a, v8::Object>,
  key: &str,
  prefix: &Cow<'static, str>,
  context: &impl Fn() -> Cow<'static, str>,
) -> Result<v8::Local<'a, v8::Value>, WebIdlError> {
  let key = v8::String::new(scope, key).unwrap();
  throwable(scope, prefix, context, |scope| {
    object.get(scope, key.into())
  })
}

fn to_number<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &Cow<'static, str>,
  context: &impl Fn() -> Cow<'static, str>,
) -> Result<f64, WebIdlError> {
  if let Ok(number) = v8::Local::<v8::Number>::try_from(value) {
    return Ok(number.value());
  }
  let number =
    throwable(scope, prefix, context, |scope| value.to_number(scope))?;
  Ok(number.value())
}

/// Gets the object a dictionary is converted from: `None` for undefined and
/// null, which convert as if every member were missing.
#[doc(hidden)]
pub fn dictionary_object<'a>(
  value: v8::Local<'a, v8::Value>,
  prefix: &Cow<'static, str>,
  context: &impl Fn() -> Cow<'static, str>,
) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError> {
  if value.is_null_or_undefined() {
    return Ok(None);
  }
  match v8::Local::<v8::Object>::try_from(value) {
    Ok(object) => Ok(Some(object)),
    Err(_) => Err(WebIdlError::new(
      prefix.clone(),
      context,
      WebIdlErrorKind::ConvertToConverterType("dictionary"),
    )),
  }
}

/// Gets a dictionary member, returning `None` when it is undefined.
#[doc(hidden)]
pub fn dictionary_member<'a>(
  scope: &mut v8::HandleScope<'a>,
  object: Option<v8::Local<'a, v8::Object>>,
  key: &str,
  prefix: &Cow<'static, str>,
  context: &impl Fn() -> Cow<'static, str>,
) -> Result<Option<v8::Local<'a, v8::Value>>, WebIdlError> {
  let Some(object) = object else {
    return Ok(None);
  };
  let value = get(scope, object, key, prefix, context)?;
  Ok(if value.is_undefined() {
    None
  } else {
    Some(value)
  })
}

// any
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

// boolean
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

/// Extended attributes for integer types.
#[derive(Debug, Default, Clone, Copy)]
pub struct IntOptions {
  /// `[Clamp]`
  pub clamp: bool,
  /// `[EnforceRange]`
  pub enforce_range: bool,
}

/// ConvertToInt (<https://webidl.spec.whatwg.org/#abstract-opdef-converttoint>).
/// The result is in the range of the integer type with the given bit length
/// and signedness.
fn convert_to_int<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: Cow<'static, str>,
  context: &impl Fn() -> Cow<'static, str>,
  options: &IntOptions,
  bit_length: u32,
  signed: bool,
) -> Result<i128, WebIdlError> {
  const MAX_SAFE_INTEGER: f64 = 9007199254740991.0;

  let (lower_bound, upper_bound) = if bit_length == 64 {
    (
      if signed { -MAX_SAFE_INTEGER } else { 0.0 },
      MAX_SAFE_INTEGER,
    )
  } else if signed {
    (
      -(2f64.powi(bit_length as i32 - 1)),
      2f64.powi(bit_length as i32 - 1) - 1.0,
    )
  } else {
    (0.0, 2f64.powi(bit_length as i32) - 1.0)
  };

  let mut x = to_number(scope, value, &prefix, context)?;
  // Normalize -0 to +0.
  if x == 0.0 {
    x = 0.0;
  }

  if options.enforce_range {
    if !x.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::NotFinite,
      ));
    }
    let x = x.trunc();
    if x < lower_bound || x > upper_bound {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::IntRange {
          lower_bound,
          upper_bound,
        },
      ));
    }
    return Ok(x as i128);
  }

  if !x.is_nan() && options.clamp {
    return Ok(x.clamp(lower_bound, upper_bound).round_ties_even() as i128);
  }

  if !x.is_finite() || x == 0.0 {
    return Ok(0);
  }

  // x modulo 2^bitLength. The remainder of a float division is exact, so `m`
  // is an integer in (-2^bitLength, 2^bitLength).
  let m = (x.trunc() % 2f64.powi(bit_length as i32)) as i128;
  let m = m.rem_euclid(1 << bit_length);
  if signed && m >= 1 << (bit_length - 1) {
    Ok(m - (1 << bit_length))
  } else {
    Ok(m)
  }
}

macro_rules! impl_ints {
  ($($t:ty: $bits:literal, $signed:literal;)*) => {
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
          let x =
            convert_to_int(scope, value, prefix, &context, options, $bits, $signed)?;
          Ok(x as $t)
        }
      }
    )*
  };
}

impl_ints! {
  i8: 8, true;
  u8: 8, false;
  i16: 16, true;
  u16: 16, false;
  i32: 32, true;
  u32: 32, false;
  i64: 64, true;
  u64: 64, false;
}

// float
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
    // Rounds to the nearest single-precision value, ties to even, and
    // overflows to infinity where the specification has 2^128.
    let y = x as f32;
    if !y.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::NotFinite,
      ));
    }
    Ok(y)
  }
}

/// `unrestricted float`: a single-precision float that may be NaN or infinite.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct UnrestrictedFloat(pub f32);

impl Deref for UnrestrictedFloat {
  type Target = f32;

  fn deref(&self) -> &Self::Target {
    &self.0
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
    let x = to_number(scope, value, &prefix, &context)?;
    Ok(UnrestrictedFloat(x as f32))
  }
}

// double
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
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::NotFinite,
      ));
    }
    Ok(x)
  }
}

/// `unrestricted double`: a double that may be NaN or infinite.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct UnrestrictedDouble(pub f64);

impl Deref for UnrestrictedDouble {
  type Target = f64;

  fn deref(&self) -> &Self::Target {
    &self.0
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

/// `bigint`: an arbitrary-precision integer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BigInt {
  /// True if the value is negative.
  pub sign: bool,
  /// The magnitude, as 64-bit words in little-endian order.
  pub words: Vec<u64>,
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
    let bigint =
      throwable(scope, &prefix, &context, |scope| value.to_big_int(scope))?;
    let mut words = vec![0; bigint.word_count()];
    let (sign, _) = bigint.to_words_array(&mut words);
    Ok(BigInt { sign, words })
  }
}

/// Extended attributes for string types.
#[derive(Debug, Default, Clone, Copy)]
pub struct StringOptions {
  /// `[LegacyNullToEmptyString]`
  pub treat_null_as_empty_string: bool,
}

fn convert_to_string<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &Cow<'static, str>,
  context: &impl Fn() -> Cow<'static, str>,
  options: &StringOptions,
) -> Result<Option<v8::Local<'a, v8::String>>, WebIdlError> {
  if value.is_null() && options.treat_null_as_empty_string {
    return Ok(None);
  }
  if let Ok(string) = v8::Local::<v8::String>::try_from(value) {
    return Ok(Some(string));
  }
  throwable(scope, prefix, context, |scope| value.to_string(scope)).map(Some)
}

// DOMString. Lone surrogates, which Rust strings can't hold, are replaced with
// U+FFFD.
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
    Ok(
      match convert_to_string(scope, value, &prefix, &context, options)? {
        Some(string) => string.to_rust_string_lossy(scope),
        None => String::new(),
      },
    )
  }
}

/// `ByteString`: a string whose code units are all at most 255. Each code
/// unit is stored as the character with the same code point (U+0000 to
/// U+00FF).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ByteString(pub String);

impl Deref for ByteString {
  type Target = String;

  fn deref(&self) -> &Self::Target {
    &self.0
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
    let Some(string) =
      convert_to_string(scope, value, &prefix, &context, options)?
    else {
      return Ok(ByteString(String::new()));
    };
    if !string.contains_only_onebyte() {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::InvalidByteString,
      ));
    }
    Ok(ByteString(string.to_rust_string_lossy(scope)))
  }
}

// nullable
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Option<T> {
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
      Ok(None)
    } else {
      T::convert(scope, value, prefix, context, options).map(Some)
    }
  }
}

// sequence
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Vec<T> {
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
    let not_iterable = |prefix, context: &C| {
      WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::ConvertToConverterType("sequence"),
      )
    };

    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(not_iterable(prefix, &context));
    };

    let iterator_symbol = v8::Symbol::get_iterator(scope);
    let method = throwable(scope, &prefix, &context, |scope| {
      object.get(scope, iterator_symbol.into())
    })?;
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
      return Err(not_iterable(prefix, &context));
    };

    let iterator = throwable(scope, &prefix, &context, |scope| {
      method.call(scope, object.into(), &[])
    })?;
    let Ok(iterator) = v8::Local::<v8::Object>::try_from(iterator) else {
      return Err(not_iterable(prefix, &context));
    };
    let next = get(scope, iterator, "next", &prefix, &context)?;
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
      return Err(not_iterable(prefix, &context));
    };

    let mut sequence = vec![];
    loop {
      let result = throwable(scope, &prefix, &context, |scope| {
        next.call(scope, iterator.into(), &[])
      })?;
      let Ok(result) = v8::Local::<v8::Object>::try_from(result) else {
        return Err(not_iterable(prefix, &context));
      };
      let done = get(scope, result, "done", &prefix, &context)?;
      if done.boolean_value(scope) {
        break;
      }
      let item = get(scope, result, "value", &prefix, &context)?;
      let index = sequence.len();
      let item = T::convert(
        scope,
        item,
        prefix.clone(),
        || format!("{}, index {index}", context()).into(),
        options,
      )?;
      sequence.push(item);
    }

    Ok(sequence)
  }
}

// record<DOMString, T>
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for HashMap<String, T> {
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
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::ConvertToConverterType("record"),
      ));
    };

    let keys = throwable(scope, &prefix, &context, |scope| {
      object.get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
          mode: v8::KeyCollectionMode::OwnOnly,
          property_filter: v8::PropertyFilter::ALL_PROPERTIES,
          index_filter: v8::IndexFilter::IncludeIndices,
          key_conversion: v8::KeyConversionMode::ConvertToString,
        },
      )
    })?;

    let mut record = HashMap::new();
    for i in 0..keys.length() {
      let key =
        throwable(scope, &prefix, &context, |scope| keys.get_index(scope, i))?;
      let name = v8::Local::<v8::Name>::try_from(key)
        .expect("property keys are strings or symbols");
      let descriptor = throwable(scope, &prefix, &context, |scope| {
        object.get_own_property_descriptor(scope, name)
      })?;
      let Ok(descriptor) = v8::Local::<v8::Object>::try_from(descriptor) else {
        continue;
      };
      let enumerable = get(scope, descriptor, "enumerable", &prefix, &context)?;
      if !enumerable.boolean_value(scope) {
        continue;
      }

      let typed_key = String::convert(
        scope,
        key,
        prefix.clone(),
        || format!("Key in {}", context()).into(),
        &StringOptions::default(),
      )?;
      let value =
        throwable(scope, &prefix, &context, |scope| object.get(scope, key))?;
      let typed_value = T::convert(
        scope,
        value,
        prefix.clone(),
        || format!("'{typed_key}' of {}", context()).into(),
        options,
      )?;
      record.insert(typed_key, typed_value);
    }

    Ok(record)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;
  use crate::WebIDL;

  /// Evaluates `$code` and converts the result to `$ty`.
  macro_rules! convert {
    ($runtime:expr, $ty:ty, $code:expr) => {
      convert!($runtime, $ty, $code, Default::default())
    };
    ($runtime:expr, $ty:ty, $code:expr, $options:expr) => {{
      let value = $runtime.execute_script("", $code.to_string()).unwrap();
      let scope = &mut $runtime.handle_scope();
      let value = v8::Local::new(scope, value);
      <$ty as WebIdlConverter>::convert(
        scope,
        value,
        "prefix".into(),
        || "context".into(),
        &$options,
      )
    }};
  }

  /// Converts the result of `$code` to `$ty`, expecting the conversion to
  /// rethrow an exception, and returns the exception's message.
  macro_rules! convert_exception {
    ($runtime:expr, $ty:ty, $code:expr) => {{
      let value = $runtime.execute_script("", $code.to_string()).unwrap();
      let scope = &mut $runtime.handle_scope();
      let tc = &mut v8::TryCatch::new(scope);
      let value = v8::Local::new(tc, value);
      let err = <$ty as WebIdlConverter>::convert(
        tc,
        value,
        "prefix".into(),
        || "context".into(),
        &Default::default(),
      )
      .unwrap_err();
      assert!(err.is_js_exception(), "{err}");
      assert!(tc.has_caught());
      let exception = tc.exception().unwrap();
      exception.to_rust_string_lossy(tc)
    }};
  }

  fn message<T: std::fmt::Debug>(result: Result<T, WebIdlError>) -> String {
    result.unwrap_err().to_string()
  }

  #[test]
  fn any() {
    let mut runtime = JsRuntime::new(Default::default());
    let value = runtime.execute_script("", "({ a: 1 })").unwrap();
    let scope = &mut runtime.handle_scope();
    let value = v8::Local::new(scope, value);
    let converted = v8::Local::<v8::Value>::convert(
      scope,
      value,
      "prefix".into(),
      || "context".into(),
      &(),
    )
    .unwrap();
    assert!(converted.strict_equals(value));
  }

  #[test]
  fn boolean() {
    let mut runtime = JsRuntime::new(Default::default());
    for (code, expected) in [
      ("true", true),
      ("false", false),
      ("0", false),
      ("-0", false),
      ("NaN", false),
      ("1", true),
      ("''", false),
      ("'false'", true),
      ("null", false),
      ("undefined", false),
      ("({})", true),
      ("0n", false),
      ("Symbol()", true),
    ] {
      assert_eq!(convert!(runtime, bool, code).unwrap(), expected, "{code}");
    }
  }

  #[test]
  fn integers() {
    let mut runtime = JsRuntime::new(Default::default());

    for (code, expected) in [
      ("0", 0),
      ("-0", 0),
      ("1.9", 1),
      ("-1.9", 255),
      ("255", 255),
      ("256", 0),
      ("257.5", 1),
      ("-1", 255),
      ("NaN", 0),
      ("Infinity", 0),
      ("-Infinity", 0),
      ("'12'", 12),
      ("'foo'", 0),
      ("null", 0),
      ("undefined", 0),
      ("true", 1),
      ("({ valueOf() { return 7 } })", 7),
    ] {
      assert_eq!(convert!(runtime, u8, code).unwrap(), expected, "{code}");
    }

    for (code, expected) in [("127", 127), ("128", -128), ("-129", 127)] {
      assert_eq!(convert!(runtime, i8, code).unwrap(), expected, "{code}");
    }
    for (code, expected) in [("65536", 0), ("-1", 65535)] {
      assert_eq!(convert!(runtime, u16, code).unwrap(), expected, "{code}");
    }
    for (code, expected) in [("32768", -32768), ("-32769", 32767)] {
      assert_eq!(convert!(runtime, i16, code).unwrap(), expected, "{code}");
    }
    for (code, expected) in [("-1", u32::MAX), ("2 ** 32 + 5", 5)] {
      assert_eq!(convert!(runtime, u32, code).unwrap(), expected, "{code}");
    }
    for (code, expected) in [
      ("2 ** 31", i32::MIN),
      ("-(2 ** 31) - 1", i32::MAX),
      ("1e20", 1661992960),
    ] {
      assert_eq!(convert!(runtime, i32, code).unwrap(), expected, "{code}");
    }
    for (code, expected) in [
      ("-1", u64::MAX),
      ("2 ** 64", 0),
      ("2 ** 53", 1 << 53),
      ("2 ** 64 + 2 ** 12", 1 << 12),
    ] {
      assert_eq!(convert!(runtime, u64, code).unwrap(), expected, "{code}");
    }
    for (code, expected) in [
      ("-1", -1),
      ("2 ** 63", i64::MIN),
      ("-(2 ** 63)", i64::MIN),
      ("2 ** 64 - 2 ** 11", -(1 << 11)),
    ] {
      assert_eq!(convert!(runtime, i64, code).unwrap(), expected, "{code}");
    }
  }

  #[test]
  fn integers_clamp() {
    let mut runtime = JsRuntime::new(Default::default());
    let clamp = IntOptions {
      clamp: true,
      enforce_range: false,
    };
    for (code, expected) in [
      ("300", 255),
      ("-5", 0),
      ("2.5", 2),
      ("3.5", 4),
      ("-0.5", 0),
      ("NaN", 0),
      ("Infinity", 255),
      ("-Infinity", 0),
    ] {
      assert_eq!(
        convert!(runtime, u8, code, clamp).unwrap(),
        expected,
        "{code}"
      );
    }
    for (code, expected) in [("200", 127), ("-200", -128), ("-2.5", -2)] {
      assert_eq!(
        convert!(runtime, i8, code, clamp).unwrap(),
        expected,
        "{code}"
      );
    }
    for (code, expected) in [
      ("2 ** 60", 9007199254740991),
      ("-(2 ** 60)", -9007199254740991),
    ] {
      assert_eq!(
        convert!(runtime, i64, code, clamp).unwrap(),
        expected,
        "{code}"
      );
    }
  }

  #[test]
  fn integers_enforce_range() {
    let mut runtime = JsRuntime::new(Default::default());
    let enforce_range = IntOptions {
      clamp: false,
      enforce_range: true,
    };
    for (code, expected) in [("255.9", 255), ("-0.5", 0), ("'7'", 7)] {
      assert_eq!(
        convert!(runtime, u8, code, enforce_range).unwrap(),
        expected,
        "{code}"
      );
    }
    assert_eq!(
      message(convert!(runtime, u8, "256", enforce_range)),
      "prefix: context is outside the accepted range of 0 to 255, inclusive"
    );
    assert_eq!(
      message(convert!(runtime, u8, "-1", enforce_range)),
      "prefix: context is outside the accepted range of 0 to 255, inclusive"
    );
    assert_eq!(
      message(convert!(runtime, i8, "NaN", enforce_range)),
      "prefix: context is not a finite number"
    );
    assert!(matches!(
      convert!(runtime, u32, "Infinity", enforce_range)
        .unwrap_err()
        .kind,
      WebIdlErrorKind::NotFinite
    ));
    assert_eq!(
      convert!(runtime, i64, "-(2 ** 53) + 1", enforce_range).unwrap(),
      -9007199254740991
    );
    assert_eq!(
      message(convert!(runtime, i64, "2 ** 53", enforce_range)),
      "prefix: context is outside the accepted range of -9007199254740991 to 9007199254740991, inclusive"
    );
    assert!(convert!(runtime, u64, "-1", enforce_range).is_err());
  }

  #[test]
  fn integers_exception() {
    let mut runtime = JsRuntime::new(Default::default());
    assert_eq!(
      convert_exception!(
        runtime,
        u8,
        "({ valueOf() { throw new Error('boom') } })"
      ),
      "Error: boom"
    );
    assert!(
      convert_exception!(runtime, u32, "Symbol()").starts_with("TypeError")
    );
    assert!(convert_exception!(runtime, i64, "1n").starts_with("TypeError"));
  }

  #[test]
  fn floats() {
    let mut runtime = JsRuntime::new(Default::default());
    assert_eq!(convert!(runtime, f32, "1.5").unwrap(), 1.5);
    assert_eq!(convert!(runtime, f32, "0.1").unwrap(), 0.1f32);
    assert!(convert!(runtime, f32, "-0").unwrap().is_sign_negative());
    assert_eq!(convert!(runtime, f32, "3.4028235e38").unwrap(), f32::MAX);
    assert_eq!(
      message(convert!(runtime, f32, "1e40")),
      "prefix: context is not a finite number"
    );
    assert!(convert!(runtime, f32, "NaN").is_err());
    assert!(convert!(runtime, f32, "-Infinity").is_err());

    assert!(convert!(runtime, UnrestrictedFloat, "NaN")
      .unwrap()
      .is_nan());
    assert_eq!(
      *convert!(runtime, UnrestrictedFloat, "1e40").unwrap(),
      f32::INFINITY
    );
    assert_eq!(
      *convert!(runtime, UnrestrictedFloat, "-Infinity").unwrap(),
      f32::NEG_INFINITY
    );
    assert_eq!(*convert!(runtime, UnrestrictedFloat, "'2.5'").unwrap(), 2.5);

    assert_eq!(convert!(runtime, f64, "0.1").unwrap(), 0.1);
    assert_eq!(convert!(runtime, f64, "1e300").unwrap(), 1e300);
    assert_eq!(convert!(runtime, f64, "'  42  '").unwrap(), 42.0);
    assert_eq!(
      message(convert!(runtime, f64, "Infinity")),
      "prefix: context is not a finite number"
    );
    assert!(convert!(runtime, f64, "'foo'").is_err());

    assert!(convert!(runtime, UnrestrictedDouble, "NaN")
      .unwrap()
      .is_nan());
    assert_eq!(
      *convert!(runtime, UnrestrictedDouble, "Infinity").unwrap(),
      f64::INFINITY
    );

    assert_eq!(
      convert_exception!(runtime, f64, "({ valueOf() { throw 1 } })"),
      "1"
    );
  }

  #[test]
  fn bigint() {
    let mut runtime = JsRuntime::new(Default::default());
    assert_eq!(
      convert!(runtime, BigInt, "0n").unwrap(),
      BigInt {
        sign: false,
        words: vec![]
      }
    );
    assert_eq!(
      convert!(runtime, BigInt, "-(2n ** 64n + 5n)").unwrap(),
      BigInt {
        sign: true,
        words: vec![5, 1]
      }
    );
    assert_eq!(
      convert!(runtime, BigInt, "'123'").unwrap(),
      BigInt {
        sign: false,
        words: vec![123]
      }
    );
    assert_eq!(
      convert!(runtime, BigInt, "true").unwrap(),
      BigInt {
        sign: false,
        words: vec![1]
      }
    );
    assert_eq!(
      convert!(runtime, BigInt, "({ valueOf() { return 7n } })").unwrap(),
      BigInt {
        sign: false,
        words: vec![7]
      }
    );
    assert!(convert_exception!(runtime, BigInt, "1").starts_with("TypeError"));
    assert!(
      convert_exception!(runtime, BigInt, "undefined").starts_with("TypeError")
    );
    assert!(
      convert_exception!(runtime, BigInt, "'1.5'").starts_with("SyntaxError")
    );
  }

  #[test]
  fn string() {
    let mut runtime = JsRuntime::new(Default::default());
    for (code, expected) in [
      ("'foo'", "foo"),
      ("''", ""),
      ("1", "1"),
      ("-0", "0"),
      ("null", "null"),
      ("undefined", "undefined"),
      ("true", "true"),
      ("10n", "10"),
      ("({ toString() { return 'bar' } })", "bar"),
      ("'\\u{1F600}'", "\u{1F600}"),
      ("'\\ud800'", "\u{FFFD}"),
    ] {
      assert_eq!(convert!(runtime, String, code).unwrap(), expected, "{code}");
    }

    let null_to_empty = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(
      convert!(runtime, String, "null", null_to_empty).unwrap(),
      ""
    );
    assert_eq!(
      convert!(runtime, String, "undefined", null_to_empty).unwrap(),
      "undefined"
    );

    assert!(
      convert_exception!(runtime, String, "Symbol()").starts_with("TypeError")
    );
    assert_eq!(
      convert_exception!(
        runtime,
        String,
        "({ toString() { throw new Error('boom') } })"
      ),
      "Error: boom"
    );
  }

  #[test]
  fn byte_string() {
    let mut runtime = JsRuntime::new(Default::default());
    assert_eq!(*convert!(runtime, ByteString, "'abc'").unwrap(), "abc");
    assert_eq!(*convert!(runtime, ByteString, "'\\xff'").unwrap(), "\u{ff}");
    assert_eq!(*convert!(runtime, ByteString, "1").unwrap(), "1");
    assert_eq!(
      message(convert!(runtime, ByteString, "'\\u0100'")),
      "prefix: context is not a valid ByteString"
    );
    assert!(convert!(runtime, ByteString, "'\\u{1F600}'").is_err());

    let null_to_empty = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(
      *convert!(runtime, ByteString, "null", null_to_empty).unwrap(),
      ""
    );
    assert_eq!(*convert!(runtime, ByteString, "null").unwrap(), "null");
  }

  #[test]
  fn nullable() {
    let mut runtime = JsRuntime::new(Default::default());
    assert_eq!(convert!(runtime, Option<u8>, "null").unwrap(), None);
    assert_eq!(convert!(runtime, Option<u8>, "undefined").unwrap(), None);
    assert_eq!(convert!(runtime, Option<u8>, "3").unwrap(), Some(3));
    assert_eq!(
      convert!(runtime, Option<String>, "0").unwrap(),
      Some("0".into())
    );
    // Options are passed to the inner converter.
    assert_eq!(
      convert!(
        runtime,
        Option<u8>,
        "1000",
        IntOptions {
          clamp: true,
          enforce_range: false
        }
      )
      .unwrap(),
      Some(255)
    );
    assert!(convert!(runtime, Option<f64>, "NaN").is_err());
  }

  #[test]
  fn sequence() {
    let mut runtime = JsRuntime::new(Default::default());
    assert_eq!(
      convert!(runtime, Vec<u8>, "[1, 2, 3]").unwrap(),
      vec![1, 2, 3]
    );
    assert_eq!(convert!(runtime, Vec<u8>, "[]").unwrap(), Vec::<u8>::new());
    assert_eq!(
      convert!(runtime, Vec<String>, "new Set(['a', 'b'])").unwrap(),
      vec!["a", "b"]
    );
    assert_eq!(
      convert!(runtime, Vec<u8>, "(function* () { yield 1; yield 2 })()")
        .unwrap(),
      vec![1, 2]
    );
    assert_eq!(
      convert!(runtime, Vec<Vec<u8>>, "[[1], [2, 3]]").unwrap(),
      vec![vec![1], vec![2, 3]]
    );
    // Holes are undefined.
    assert_eq!(
      convert!(runtime, Vec<Option<u8>>, "[1, , 3]").unwrap(),
      vec![Some(1), None, Some(3)]
    );
    // Options are passed to the element converter.
    assert_eq!(
      convert!(
        runtime,
        Vec<u8>,
        "[-1, 1000]",
        IntOptions {
          clamp: true,
          enforce_range: false
        }
      )
      .unwrap(),
      vec![0, 255]
    );

    for code in ["'abc'", "1", "null", "undefined", "({})", "({ length: 1 })"] {
      assert_eq!(
        message(convert!(runtime, Vec<u8>, code)),
        "prefix: context can not be converted to sequence",
        "{code}"
      );
    }
    assert_eq!(
      message(convert!(runtime, Vec<f64>, "[1, 'x']")),
      "prefix: context, index 1 is not a finite number"
    );
    assert_eq!(
      message(convert!(runtime, Vec<Vec<f64>>, "[[1], [2, NaN]]")),
      "prefix: context, index 1, index 1 is not a finite number"
    );

    // The iteration protocol is used, even for arrays.
    assert_eq!(
      convert!(
        runtime,
        Vec<u8>,
        "const a = [1, 2]; a[Symbol.iterator] = function* () { yield 3 }; a"
      )
      .unwrap(),
      vec![3]
    );
    assert_eq!(
      convert_exception!(
        runtime,
        Vec<u8>,
        "({ [Symbol.iterator]() { throw new Error('boom') } })"
      ),
      "Error: boom"
    );
    assert_eq!(
      convert_exception!(
        runtime,
        Vec<u8>,
        "(function* () { yield 1; throw new Error('boom') })()"
      ),
      "Error: boom"
    );
  }

  #[test]
  fn record() {
    let mut runtime = JsRuntime::new(Default::default());
    assert_eq!(
      convert!(runtime, HashMap<String, u8>, "({ a: 1, b: '2', 3: 3 })")
        .unwrap(),
      HashMap::from([("a".into(), 1), ("b".into(), 2), ("3".into(), 3)])
    );
    assert_eq!(
      convert!(runtime, HashMap<String, u8>, "[5]").unwrap(),
      HashMap::from([("0".into(), 5)])
    );
    // Only own enumerable properties are converted.
    assert_eq!(
      convert!(
        runtime,
        HashMap<String, u8>,
        "const o = Object.create({ inherited: 1 });
         Object.defineProperty(o, 'hidden', { value: 2, enumerable: false });
         o.shown = 3;
         o"
      )
      .unwrap(),
      HashMap::from([("shown".into(), 3)])
    );
    // Options are passed to the value converter.
    assert_eq!(
      convert!(
        runtime,
        HashMap<String, u8>,
        "({ a: 1000 })",
        IntOptions {
          clamp: true,
          enforce_range: false
        }
      )
      .unwrap(),
      HashMap::from([("a".into(), 255)])
    );

    for code in ["'abc'", "1", "null", "undefined"] {
      assert_eq!(
        message(convert!(runtime, HashMap<String, u8>, code)),
        "prefix: context can not be converted to record",
        "{code}"
      );
    }
    assert_eq!(
      message(convert!(runtime, HashMap<String, f64>, "({ a: 1, b: 'x' })")),
      "prefix: 'b' of context is not a finite number"
    );
    // Enumerable symbol keys can't be converted to strings.
    assert!(convert_exception!(
      runtime,
      HashMap<String, u8>,
      "({ [Symbol('a')]: 1 })"
    )
    .starts_with("TypeError"));
    assert_eq!(
      convert_exception!(
        runtime,
        HashMap<String, u8>,
        "({ get a() { throw new Error('boom') } })"
      ),
      "Error: boom"
    );
  }

  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Dict {
    a: u8,
    #[options(clamp = true)]
    b: Vec<u16>,
    #[webidl(default = Some(3))]
    c: Option<u32>,
    #[webidl(rename = "e")]
    d: u16,
    #[webidl(default = HashMap::new())]
    f: HashMap<String, u32>,
    #[webidl(required)]
    g: Option<u32>,
    some_long_name: Option<String>,
    #[webidl(default = 1)]
    #[options(enforce_range = true)]
    r#type: u8,
  }

  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct Empty {
    a: Option<bool>,
    #[webidl(default = true)]
    b: bool,
  }

  #[test]
  fn dictionary() {
    let mut runtime = JsRuntime::new(Default::default());
    assert_eq!(
      convert!(
        runtime,
        Dict,
        "({ a: 1, b: [-1, 70000], c: 4, e: 5, f: { x: 6 }, g: 7, someLongName: 8, type: 9 })"
      )
      .unwrap(),
      Dict {
        a: 1,
        b: vec![0, 65535],
        c: Some(4),
        d: 5,
        f: HashMap::from([("x".into(), 6)]),
        g: Some(7),
        some_long_name: Some("8".into()),
        r#type: 9,
      }
    );
    // Missing and undefined members take their defaults.
    assert_eq!(
      convert!(
        runtime,
        Dict,
        "({ a: 1, b: [], c: undefined, d: 10, e: 5, g: null })"
      )
      .unwrap(),
      Dict {
        a: 1,
        b: vec![],
        c: Some(3),
        d: 5,
        f: HashMap::new(),
        g: None,
        some_long_name: None,
        r#type: 1,
      }
    );
    // Members are read from the prototype chain.
    assert_eq!(
      convert!(
        runtime,
        Dict,
        "Object.create({ a: 1, b: [], e: 5, g: 1, c: null })"
      )
      .unwrap()
      .c,
      None
    );

    assert_eq!(
      message(convert!(runtime, Dict, "({ b: [], e: 5, g: 1 })")),
      "prefix: context can not be converted to 'Dict' because 'a' is required in 'Dict'"
    );
    assert_eq!(
      message(convert!(runtime, Dict, "({ a: 1, b: [], e: 5 })")),
      "prefix: context can not be converted to 'Dict' because 'g' is required in 'Dict'"
    );
    assert_eq!(
      message(convert!(runtime, Dict, "({ a: 1, b: [], e: 5, g: undefined })")),
      "prefix: context can not be converted to 'Dict' because 'g' is required in 'Dict'"
    );
    assert_eq!(
      message(convert!(runtime, Dict, "({ a: 1, b: [], e: 5, g: 1, type: 256 })")),
      "prefix: 'type' of 'Dict' (context) is outside the accepted range of 0 to 255, inclusive"
    );
    assert!(convert_exception!(
      runtime,
      Dict,
      "({ a: 1, b: [1, Symbol()], e: 5, g: 1 })"
    )
    .starts_with("TypeError"));
    assert_eq!(
      message(convert!(runtime, Dict, "1")),
      "prefix: context can not be converted to dictionary"
    );
    assert!(convert!(runtime, Dict, "undefined").is_err());

    // Without required members, undefined and null are empty dictionaries.
    for code in ["undefined", "null", "({})", "[]", "() => {}"] {
      assert_eq!(
        convert!(runtime, Empty, code).unwrap(),
        Empty { a: None, b: true },
        "{code}"
      );
    }
    for code in ["1", "'a'", "true", "Symbol()", "1n"] {
      assert!(convert!(runtime, Empty, code).is_err(), "{code}");
    }
  }

  #[test]
  fn dictionary_member_order() {
    let mut runtime = JsRuntime::new(Default::default());
    convert!(
      runtime,
      Dict,
      "globalThis.order = [];
       new Proxy({ a: 1, b: [], e: 5, g: 1 }, {
         get(target, key) {
           order.push(key);
           return target[key];
         }
       })"
    )
    .unwrap();
    let order = runtime.execute_script("", "order.join()").unwrap();
    {
      let scope = &mut runtime.handle_scope();
      let order = v8::Local::new(scope, order).to_rust_string_lossy(scope);
      assert_eq!(order, "a,b,c,e,f,g,someLongName,type");
    }

    assert_eq!(
      convert_exception!(
        runtime,
        Dict,
        "({ a: 1, get b() { throw new Error('boom') } })"
      ),
      "Error: boom"
    );
  }

  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(dictionary)]
  struct WithAny<'s> {
    value: v8::Local<'s, v8::Value>,
  }

  #[test]
  fn dictionary_with_lifetime() {
    let mut runtime = JsRuntime::new(Default::default());
    let value = runtime.execute_script("", "({ value: 'v' })").unwrap();
    let scope = &mut runtime.handle_scope();
    let value = v8::Local::new(scope, value);
    let dict =
      WithAny::convert(scope, value, "prefix".into(), || "context".into(), &())
        .unwrap();
    assert_eq!(dict.value.to_rust_string_lossy(scope), "v");
  }

  #[derive(WebIDL, Debug, PartialEq)]
  #[webidl(enum)]
  enum Enumeration {
    FooBar,
    Baz,
    #[webidl(rename = "hello")]
    World,
  }

  #[test]
  fn enumeration() {
    let mut runtime = JsRuntime::new(Default::default());
    assert_eq!(
      convert!(runtime, Enumeration, "'foo-bar'").unwrap(),
      Enumeration::FooBar
    );
    assert_eq!(
      convert!(runtime, Enumeration, "'baz'").unwrap(),
      Enumeration::Baz
    );
    assert_eq!(
      convert!(runtime, Enumeration, "({ toString: () => 'hello' })").unwrap(),
      Enumeration::World
    );
    assert_eq!(
      message(convert!(runtime, Enumeration, "'world'")),
      "prefix: context can not be converted to 'Enumeration': 'world' is not a valid enum value"
    );
    assert!(convert!(runtime, Enumeration, "'FooBar'").is_err());
    assert!(convert!(runtime, Enumeration, "undefined").is_err());
    assert!(convert_exception!(runtime, Enumeration, "Symbol()")
      .starts_with("TypeError"));
  }
}
