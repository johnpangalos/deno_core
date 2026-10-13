// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversions from JavaScript values to WebIDL types.
//!
//! The converters in this module implement the
//! [JavaScript to IDL type mapping](https://webidl.spec.whatwg.org/#js-type-mapping).
//! Use [`WebIdlConverter`] directly, derive it for dictionaries and enums with
//! `#[derive(WebIDL)]`, or pass an op argument through `#[webidl]` in `#[op2]`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Deref;

use crate::v8;

const THREW: &str =
  "could not be converted because a JavaScript exception was thrown";

/// An error produced while converting a JavaScript value to a WebIDL type.
///
/// Displays as `"{prefix}: {context} {message}"`, e.g.
/// `Failed to execute 'fetch': Argument 1 is not a finite number`. Empty parts
/// are omitted, so an empty prefix produces `"{context} {message}"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  prefix: Cow<'static, str>,
  context: Cow<'static, str>,
  message: Cow<'static, str>,
}

impl WebIdlError {
  /// Creates an error from its prefix (what was being done), context (which
  /// value failed) and message (what was wrong with it).
  pub fn new(
    prefix: impl Into<Cow<'static, str>>,
    context: impl Into<Cow<'static, str>>,
    message: impl Into<Cow<'static, str>>,
  ) -> Self {
    Self {
      prefix: prefix.into(),
      context: context.into(),
      message: message.into(),
    }
  }
}

impl Display for WebIdlError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    if !self.prefix.is_empty() {
      write!(f, "{}: ", self.prefix)?;
    }
    if !self.context.is_empty() {
      write!(f, "{} ", self.context)?;
    }
    f.write_str(&self.message)
  }
}

impl std::error::Error for WebIdlError {}

/// Converts a JavaScript value into a WebIDL type.
///
/// `prefix` describes the operation being performed and `context` describes
/// the value being converted. `context` is only called when an error is built.
pub trait WebIdlConverter<'a>: Sized {
  /// Options that change how the value is converted. Use `()` if there are none.
  type Options: Default;

  /// Converts `value` to `Self`.
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

fn type_error<C>(
  prefix: &str,
  context: &C,
  message: impl Into<Cow<'static, str>>,
) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  WebIdlError::new(prefix.to_owned(), context(), message)
}

/// Builds the context for a value nested inside `parent`, e.g. `'fooBar' of 'Argument 1'`.
#[doc(hidden)]
pub fn nested_context(key: &str, parent: &str) -> Cow<'static, str> {
  Cow::Owned(format!("'{key}' of '{parent}'"))
}

// The helpers below run V8 operations inside a `TryCatch` so that a JavaScript
// exception becomes an `Err` (holding the exception's text) instead of being
// left pending.

/// The text of the exception caught by `tc`, or an empty string if there is no
/// exception or it cannot be converted to a string.
fn exception_text(tc: &mut v8::TryCatch<v8::HandleScope>) -> String {
  let Some(exception) = tc.exception() else {
    return String::new();
  };
  match exception.to_string(tc) {
    Some(text) => text.to_rust_string_lossy(tc),
    None => String::new(),
  }
}

/// Builds the error for a JavaScript exception thrown during conversion. Falls
/// back to a generic message when the exception has no text.
fn threw<C>(prefix: &str, context: &C, exception: String) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  if exception.is_empty() {
    type_error(prefix, context, THREW)
  } else {
    type_error(
      prefix,
      context,
      format!("could not be converted: {exception}"),
    )
  }
}

fn try_number(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
) -> Result<f64, String> {
  let tc = &mut v8::TryCatch::new(scope);
  value.number_value(tc).ok_or_else(|| exception_text(tc))
}

fn try_string<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
) -> Result<String, String> {
  let tc = &mut v8::TryCatch::new(scope);
  let string = value.to_string(tc).ok_or_else(|| exception_text(tc))?;
  Ok(string.to_rust_string_lossy(tc))
}

fn try_big_int<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
) -> Option<v8::Local<'a, v8::BigInt>> {
  let tc = &mut v8::TryCatch::new(scope);
  value.to_big_int(tc)
}

fn try_get<'a>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<'a, v8::Object>,
  key: v8::Local<'a, v8::Value>,
) -> Result<v8::Local<'a, v8::Value>, String> {
  let tc = &mut v8::TryCatch::new(scope);
  object.get(tc, key).ok_or_else(|| exception_text(tc))
}

fn try_call<'a>(
  scope: &mut v8::HandleScope<'a>,
  function: v8::Local<'a, v8::Function>,
  this: v8::Local<'a, v8::Value>,
) -> Result<v8::Local<'a, v8::Value>, String> {
  let tc = &mut v8::TryCatch::new(scope);
  function
    .call(tc, this, &[])
    .ok_or_else(|| exception_text(tc))
}

fn try_own_enumerable_keys<'a>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<'a, v8::Object>,
) -> Result<v8::Local<'a, v8::Array>, String> {
  let tc = &mut v8::TryCatch::new(scope);
  object
    .get_own_property_names(
      tc,
      v8::GetPropertyNamesArgs {
        mode: v8::KeyCollectionMode::OwnOnly,
        property_filter: v8::PropertyFilter::ONLY_ENUMERABLE,
        index_filter: v8::IndexFilter::IncludeIndices,
        key_conversion: v8::KeyConversionMode::ConvertToString,
      },
    )
    .ok_or_else(|| exception_text(tc))
}

fn key<'a>(
  scope: &mut v8::HandleScope<'a>,
  name: &str,
) -> Option<v8::Local<'a, v8::Value>> {
  v8::String::new(scope, name).map(Into::into)
}

/// `ToNumber` for the numeric converters, rejecting Symbols and BigInts.
fn to_number<C>(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<f64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_symbol() {
    return Err(type_error(prefix, context, "is a Symbol and not a number"));
  }
  if value.is_big_int() {
    return Err(type_error(prefix, context, "is a BigInt and not a number"));
  }
  try_number(scope, value)
    .map_err(|exception| threw(prefix, context, exception))
}

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

  /// Uses `ToBoolean`, so it never fails.
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

/// Options for the integer converters.
///
/// `enforce_range` and `clamp` correspond to the `[EnforceRange]` and `[Clamp]`
/// extended attributes. If both are set, `enforce_range` wins, as in the spec.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  pub clamp: bool,
  pub enforce_range: bool,
}

/// Implements `ConvertToInt`. Returns an integer that is already reduced to
/// the range of the target type, or (for the default conversion) reduced
/// modulo 2^64, so that `as` casts to the target type give the modular result.
fn convert_int<C>(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
  prefix: &str,
  context: &C,
  options: &IntOptions,
  lower: f64,
  upper: f64,
) -> Result<i128, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let x = to_number(scope, value, prefix, context)?;
  if options.enforce_range {
    if !x.is_finite() {
      return Err(type_error(prefix, context, "is not a finite number"));
    }
    let x = x.trunc();
    if !(lower..=upper).contains(&x) {
      return Err(type_error(
        prefix,
        context,
        format!("is outside the range {lower}..={upper}"),
      ));
    }
    return Ok(x as i128);
  }
  if options.clamp {
    if x.is_nan() {
      return Ok(0);
    }
    return Ok(x.clamp(lower, upper).round_ties_even() as i128);
  }
  if !x.is_finite() {
    return Ok(0);
  }
  // `%` on f64 is exact, so this reduces the truncated value modulo 2^64
  // without rounding, even for values beyond 2^53.
  let remainder = x.trunc() % 18446744073709551616.0;
  Ok((remainder as i128).rem_euclid(1i128 << 64))
}

macro_rules! impl_int_converter {
  ($($ty:ty => $lower:expr, $upper:expr;)*) => {
    $(
      impl<'a> WebIdlConverter<'a> for $ty {
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
          convert_int(scope, value, &prefix, &context, options, $lower, $upper)
            .map(|v| v as $ty)
        }
      }
    )*
  };
}

// The `as` cast in the converter truncates to the width of the target type,
// which is the modular reduction the default conversion requires.
impl_int_converter! {
  i8 => -128.0, 127.0;
  u8 => 0.0, 255.0;
  i16 => -32768.0, 32767.0;
  u16 => 0.0, 65535.0;
  i32 => -2147483648.0, 2147483647.0;
  u32 => 0.0, 4294967295.0;
  i64 => -9007199254740991.0, 9007199254740991.0;
  u64 => 0.0, 9007199254740991.0;
}

impl<'a> WebIdlConverter<'a> for f64 {
  type Options = ();

  /// Rejects NaN and the infinities. See [`UnrestrictedDouble`] to accept them.
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
      return Err(type_error(&prefix, &context, "is not a finite number"));
    }
    Ok(x)
  }
}

impl<'a> WebIdlConverter<'a> for f32 {
  type Options = ();

  /// Rejects NaN, the infinities, and finite values that round to an infinity
  /// when converted to `f32`. See [`UnrestrictedFloat`] to accept them.
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
      return Err(type_error(&prefix, &context, "is not a finite number"));
    }
    let y = x as f32;
    if !y.is_finite() {
      return Err(type_error(
        &prefix,
        &context,
        "is outside the range of a float",
      ));
    }
    Ok(y)
  }
}

/// A `unrestricted double`: like `f64`, but NaN and the infinities are accepted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnrestrictedDouble(pub f64);

impl Deref for UnrestrictedDouble {
  type Target = f64;

  fn deref(&self) -> &f64 {
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

/// A `unrestricted float`: like `f32`, but NaN and the infinities are accepted.
/// Finite values that are too large for `f32` become an infinity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnrestrictedFloat(pub f32);

impl Deref for UnrestrictedFloat {
  type Target = f32;

  fn deref(&self) -> &f32 {
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
    to_number(scope, value, &prefix, &context)
      .map(|x| UnrestrictedFloat(x as f32))
  }
}

/// Options for [`String`] and [`ByteString`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  /// Converts `null` to the empty string instead of `"null"`, like `[LegacyNullToEmptyString]`.
  pub treat_null_as_empty_string: bool,
}

/// A WebIDL `DOMString`, produced with `ToString`.
///
/// Rust strings cannot hold lone surrogates, so each lone surrogate in the
/// JavaScript string becomes U+FFFD REPLACEMENT CHARACTER.
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
    if options.treat_null_as_empty_string && value.is_null() {
      return Ok(String::new());
    }
    if value.is_symbol() {
      return Err(type_error(
        &prefix,
        &context,
        "is a Symbol and cannot be converted to a string",
      ));
    }
    try_string(scope, value)
      .map_err(|exception| threw(&prefix, &context, exception))
  }
}

/// A WebIDL `ByteString`: a string whose code units are all at most 0xFF.
///
/// Code unit `0x80`-`0xFF` values are kept as the Unicode characters with the
/// same number, so `"\xff"` is stored as `"ÿ"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteString(pub String);

impl Deref for ByteString {
  type Target = String;

  fn deref(&self) -> &String {
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
    let string =
      String::convert(scope, value, prefix.clone(), &context, options)?;
    // Lone surrogates were already replaced with U+FFFD, which is above 0xFF.
    if string.chars().any(|c| c > '\u{ff}') {
      return Err(type_error(
        &prefix,
        &context,
        "is not a valid ByteString: it contains a code unit greater than 0xFF",
      ));
    }
    Ok(ByteString(string))
  }
}

/// A WebIDL `bigint`, stored as a sign and little-endian 64-bit words.
///
/// Zero has no words, so `0n` is `sign: false` with an empty `words`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigInt {
  /// True if the value is negative.
  pub sign: bool,
  /// Magnitude, least significant word first.
  pub words: Vec<u64>,
}

impl BigInt {
  fn from_v8(value: v8::Local<v8::BigInt>) -> Self {
    let mut words = vec![0u64; value.word_count()];
    let (sign, _) = value.to_words_array(&mut words);
    BigInt { sign, words }
  }
}

impl<'a> WebIdlConverter<'a> for BigInt {
  type Options = ();

  /// Uses `ToBigInt`: BigInts convert exactly, booleans to 0 or 1, and strings
  /// are parsed as BigInt literals. Numbers, `undefined`, `null` and Symbols are
  /// rejected.
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
    if let Ok(big) = v8::Local::<v8::BigInt>::try_from(value) {
      return Ok(BigInt::from_v8(big));
    }
    if value.is_boolean() {
      let words = if value.boolean_value(scope) {
        vec![1]
      } else {
        vec![]
      };
      return Ok(BigInt { sign: false, words });
    }
    if value.is_number() || value.is_null_or_undefined() || value.is_symbol() {
      return Err(type_error(
        &prefix,
        &context,
        "cannot be converted to a BigInt",
      ));
    }
    try_big_int(scope, value)
      .map(BigInt::from_v8)
      .ok_or_else(|| {
        type_error(&prefix, &context, "cannot be converted to a BigInt")
      })
  }
}

/// Converts `null` and `undefined` to `None`, and anything else with `T`.
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

/// A WebIDL `sequence<T>`, read with the iterator protocol.
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
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(type_error(&prefix, &context, "is not an object"));
    };
    let iterator_key = v8::Symbol::get_iterator(scope);
    let method = try_get(scope, object, iterator_key.into())
      .map_err(|exception| threw(&prefix, &context, exception))?;
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
      return Err(type_error(&prefix, &context, "is not iterable"));
    };
    let iterator = try_call(scope, method, value)
      .map_err(|exception| threw(&prefix, &context, exception))?;
    let Ok(iterator) = v8::Local::<v8::Object>::try_from(iterator) else {
      return Err(type_error(
        &prefix,
        &context,
        "returned a non-object iterator",
      ));
    };
    let next_key =
      key(scope, "next").ok_or_else(|| type_error(&prefix, &context, THREW))?;
    let next = try_get(scope, iterator, next_key)
      .map_err(|exception| threw(&prefix, &context, exception))?;
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
      return Err(type_error(
        &prefix,
        &context,
        "iterator 'next' is not a function",
      ));
    };
    let done_key =
      key(scope, "done").ok_or_else(|| type_error(&prefix, &context, THREW))?;
    let value_key = key(scope, "value")
      .ok_or_else(|| type_error(&prefix, &context, THREW))?;

    let mut elements = Vec::new();
    loop {
      let result = try_call(scope, next, iterator.into())
        .map_err(|exception| threw(&prefix, &context, exception))?;
      let Ok(result) = v8::Local::<v8::Object>::try_from(result) else {
        return Err(type_error(
          &prefix,
          &context,
          "returned a non-object iterator result",
        ));
      };
      let done = try_get(scope, result, done_key)
        .map_err(|exception| threw(&prefix, &context, exception))?;
      if done.boolean_value(scope) {
        break;
      }
      let item = try_get(scope, result, value_key)
        .map_err(|exception| threw(&prefix, &context, exception))?;
      let index = elements.len();
      let parent = &context;
      let element_context =
        || Cow::Owned(format!("element {index} of '{}'", parent()));
      elements.push(T::convert(
        scope,
        item,
        prefix.clone(),
        element_context,
        options,
      )?);
    }
    Ok(elements)
  }
}

/// A WebIDL `record<DOMString, T>`: the enumerable own string-keyed properties.
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
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(type_error(&prefix, &context, "is not an object"));
    };
    let names = try_own_enumerable_keys(scope, object)
      .map_err(|exception| threw(&prefix, &context, exception))?;
    let mut map = HashMap::new();
    for index in 0..names.length() {
      let name = names
        .get_index(scope, index)
        .ok_or_else(|| type_error(&prefix, &context, THREW))?;
      let Ok(name) = v8::Local::<v8::String>::try_from(name) else {
        return Err(type_error(
          &prefix,
          &context,
          "has a Symbol key and cannot be converted to a string",
        ));
      };
      // `Get` uses the original key, so a lone surrogate still finds its
      // property. The lossy name is only used for the map and the context.
      let property = try_get(scope, object, name.into())
        .map_err(|exception| threw(&prefix, &context, exception))?;
      let name = name.to_rust_string_lossy(scope);
      let property_context = || nested_context(&name, &context());
      let converted =
        T::convert(scope, property, prefix.clone(), property_context, options)?;
      map.insert(name, converted);
    }
    Ok(map)
  }
}

/// Reads the object for a dictionary. Returns `None` for `null` and
/// `undefined`, which mean that every member is missing.
#[doc(hidden)]
pub fn dictionary_object<'a, C>(
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_null_or_undefined() {
    return Ok(None);
  }
  match v8::Local::<v8::Object>::try_from(value) {
    Ok(object) => Ok(Some(object)),
    Err(_) => Err(type_error(prefix, context, "is not an object")),
  }
}

/// Reads one dictionary member with `Get`. Returns `None` if the dictionary is
/// missing or the member is `undefined`.
#[doc(hidden)]
pub fn dictionary_member<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  object: Option<v8::Local<'a, v8::Object>>,
  name: &str,
  prefix: &str,
  context: &C,
) -> Result<Option<v8::Local<'a, v8::Value>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let Some(object) = object else {
    return Ok(None);
  };
  let key =
    key(scope, name).ok_or_else(|| type_error(prefix, context, THREW))?;
  let value = try_get(scope, object, key)
    .map_err(|exception| threw(prefix, context, exception))?;
  Ok((!value.is_undefined()).then_some(value))
}

/// Builds the error for a required dictionary member that is missing.
#[doc(hidden)]
pub fn missing_required_key(
  prefix: &str,
  context: &str,
  name: &str,
) -> WebIdlError {
  WebIdlError::new(
    prefix.to_owned(),
    context.to_owned(),
    format!("is missing required key '{name}'"),
  )
}

/// Builds the error for a JavaScript value that is not one of the enum's values.
#[doc(hidden)]
pub fn invalid_enum_value(
  prefix: &str,
  context: &str,
  value: &str,
  enum_name: &str,
) -> WebIdlError {
  WebIdlError::new(
    prefix.to_owned(),
    context.to_owned(),
    format!("'{value}' is not a valid value for enumeration '{enum_name}'"),
  )
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;

  use super::*;
  use crate::JsRuntime;

  fn eval<'a>(
    scope: &mut v8::HandleScope<'a>,
    source: &str,
  ) -> v8::Local<'a, v8::Value> {
    let source = v8::String::new(scope, source).unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    script.run(scope).unwrap()
  }

  fn convert<'a, T>(
    scope: &mut v8::HandleScope<'a>,
    source: &str,
    options: &T::Options,
  ) -> Result<T, WebIdlError>
  where
    T: WebIdlConverter<'a>,
  {
    let value = eval(scope, source);
    T::convert(
      scope,
      value,
      Cow::Borrowed("Failed to execute 'test'"),
      || Cow::Borrowed("Argument 1"),
      options,
    )
  }

  #[test]
  fn error_display() {
    let error = WebIdlError::new(
      "Failed to execute 'fetch'",
      "Argument 1",
      "is not a finite number",
    );
    assert_eq!(
      error.to_string(),
      "Failed to execute 'fetch': Argument 1 is not a finite number"
    );
    // Nested context is embedded as given.
    let nested = WebIdlError::new("p", "'foo' of 'Argument 1'", "is wrong");
    assert_eq!(nested.to_string(), "p: 'foo' of 'Argument 1' is wrong");
    // An empty prefix is omitted.
    let empty = WebIdlError::new("", "Argument 1", "is wrong");
    assert_eq!(empty.to_string(), "Argument 1 is wrong");
  }

  #[test]
  fn context_is_only_built_on_error() {
    let calls = Cell::new(0);
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let value = eval(scope, "5");
    let ok = u8::convert(
      scope,
      value,
      Cow::Borrowed("p"),
      || {
        calls.set(calls.get() + 1);
        Cow::Borrowed("Argument 1")
      },
      &Default::default(),
    );
    assert_eq!(ok.unwrap(), 5);
    assert_eq!(calls.get(), 0);
    let value = eval(scope, "Symbol()");
    let err = u8::convert(
      scope,
      value,
      Cow::Borrowed("p"),
      || {
        calls.set(calls.get() + 1);
        Cow::Borrowed("Argument 1")
      },
      &Default::default(),
    );
    assert!(err.is_err());
    assert_eq!(calls.get(), 1);
  }

  #[test]
  fn throwing_conversions_do_not_panic() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let value_of = "({ valueOf() { throw new Error('x') } })";
    assert!(convert::<u8>(scope, value_of, &Default::default()).is_err());
    assert!(convert::<f64>(scope, value_of, &()).is_err());
    assert!(convert::<BigInt>(scope, value_of, &()).is_err());
    let to_string = "({ toString() { throw new Error('x') } })";
    assert!(convert::<String>(scope, to_string, &Default::default()).is_err());
    assert!(convert::<f64>(scope, to_string, &()).is_err());
    let proxy = "new Proxy({}, { get() { throw new Error('x') } })";
    assert!(convert::<u8>(scope, proxy, &Default::default()).is_err());
    assert!(convert::<String>(scope, proxy, &Default::default()).is_err());
    assert!(convert::<Vec<u8>>(scope, proxy, &Default::default()).is_err());
    let next = "({ [Symbol.iterator]() { return { next() { throw new Error('x') } } } })";
    assert!(convert::<Vec<u8>>(scope, next, &Default::default()).is_err());
    let getter = "({ get a() { throw new Error('x') } })";
    assert!(
      convert::<HashMap<String, u8>>(scope, getter, &Default::default())
        .is_err()
    );
  }

  #[test]
  fn any_value_is_unchanged() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let value = convert::<v8::Local<v8::Value>>(scope, "[1]", &()).unwrap();
    assert!(value.is_array());
  }

  #[test]
  fn bool_uses_to_boolean() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    for source in ["0", "''", "NaN", "null", "undefined"] {
      assert!(!convert::<bool>(scope, source, &()).unwrap(), "{source}");
    }
    for source in ["({})", "'false'", "[]", "1", "Symbol()"] {
      assert!(convert::<bool>(scope, source, &()).unwrap(), "{source}");
    }
  }

  #[test]
  fn integers_default_conversion_wraps() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let o: IntOptions = Default::default();
    assert_eq!(convert::<u8>(scope, "256", &o).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "-1", &o).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "255.5", &o).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "'12'", &o).unwrap(), 12);
    assert_eq!(
      convert::<u8>(scope, "({ valueOf() { return 7 } })", &o).unwrap(),
      7
    );
    assert_eq!(convert::<u8>(scope, "NaN", &o).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "Infinity", &o).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "-0", &o).unwrap(), 0);
    assert_eq!(convert::<i8>(scope, "128", &o).unwrap(), -128);
    assert_eq!(convert::<i32>(scope, "2 ** 31", &o).unwrap(), -2147483648);
    assert_eq!(convert::<u64>(scope, "2 ** 64", &o).unwrap(), 0);
    assert_eq!(convert::<u64>(scope, "-1", &o).unwrap(), u64::MAX);
    assert_eq!(convert::<u64>(scope, "-3", &o).unwrap(), u64::MAX - 2);
    assert_eq!(convert::<i64>(scope, "-(2 ** 63)", &o).unwrap(), i64::MIN);
    assert!(convert::<u8>(scope, "1n", &o).is_err());
    assert!(convert::<u8>(scope, "Symbol()", &o).is_err());
  }

  #[test]
  fn integers_clamp() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let o = IntOptions {
      clamp: true,
      enforce_range: false,
    };
    assert_eq!(convert::<u8>(scope, "2.5", &o).unwrap(), 2);
    assert_eq!(convert::<u8>(scope, "3.5", &o).unwrap(), 4);
    assert_eq!(convert::<u8>(scope, "-0.5", &o).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "300", &o).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "-5", &o).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "NaN", &o).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "Infinity", &o).unwrap(), 255);
    assert_eq!(
      convert::<u64>(scope, "2 ** 60", &o).unwrap(),
      9007199254740991
    );
    assert_eq!(
      convert::<i64>(scope, "-(2 ** 60)", &o).unwrap(),
      -9007199254740991
    );
  }

  #[test]
  fn integers_enforce_range() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let o = IntOptions {
      clamp: false,
      enforce_range: true,
    };
    assert_eq!(convert::<u8>(scope, "255", &o).unwrap(), 255);
    assert!(convert::<u8>(scope, "256", &o).is_err());
    assert!(convert::<u8>(scope, "NaN", &o).is_err());
    assert!(convert::<u8>(scope, "Infinity", &o).is_err());
    assert!(convert::<u32>(scope, "-1", &o).is_err());
    assert!(convert::<u64>(scope, "2 ** 53", &o).is_err());
    assert_eq!(
      convert::<u64>(scope, "2 ** 53 - 1", &o).unwrap(),
      9007199254740991
    );
    // enforce_range takes precedence over clamp.
    let both = IntOptions {
      clamp: true,
      enforce_range: true,
    };
    assert!(convert::<u8>(scope, "300", &both).is_err());
  }

  #[test]
  fn floats() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert!(convert::<f64>(scope, "'abc'", &()).is_err());
    assert!(convert::<f64>(scope, "Infinity", &()).is_err());
    assert_eq!(convert::<f64>(scope, "null", &()).unwrap(), 0.0);
    assert!(convert::<f64>(scope, "undefined", &()).is_err());
    assert!(convert::<f64>(scope, "Symbol()", &()).is_err());
    assert!(convert::<f64>(scope, "1n", &()).is_err());
    assert!(convert::<f32>(scope, "1e40", &()).is_err());
    assert_eq!(convert::<f32>(scope, "1.5", &()).unwrap(), 1.5);
    assert!(convert::<UnrestrictedDouble>(scope, "'abc'", &())
      .unwrap()
      .is_nan());
    assert_eq!(
      *convert::<UnrestrictedDouble>(scope, "-Infinity", &()).unwrap(),
      f64::NEG_INFINITY
    );
    assert_eq!(
      *convert::<UnrestrictedFloat>(scope, "1e40", &()).unwrap(),
      f32::INFINITY
    );
    let negative_zero = convert::<f64>(scope, "-0", &()).unwrap();
    assert!(negative_zero.is_sign_negative());
    let negative_zero = convert::<UnrestrictedFloat>(scope, "-0", &()).unwrap();
    assert!(negative_zero.is_sign_negative());
  }

  #[test]
  fn strings() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let o: StringOptions = Default::default();
    assert!(convert::<String>(scope, "Symbol()", &o).is_err());
    assert_eq!(convert::<String>(scope, "null", &o).unwrap(), "null");
    assert_eq!(
      convert::<String>(scope, "undefined", &o).unwrap(),
      "undefined"
    );
    assert_eq!(convert::<String>(scope, "1.5", &o).unwrap(), "1.5");
    assert_eq!(
      convert::<String>(scope, "({ toString() { return 'x' } })", &o).unwrap(),
      "x"
    );
    assert_eq!(
      convert::<String>(scope, "'\\uD800'", &o).unwrap(),
      "\u{fffd}"
    );
    let o = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(convert::<String>(scope, "null", &o).unwrap(), "");
    assert_eq!(
      convert::<String>(scope, "undefined", &o).unwrap(),
      "undefined"
    );
  }

  #[test]
  fn byte_strings() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let o: StringOptions = Default::default();
    assert_eq!(convert::<ByteString>(scope, "'\\xff'", &o).unwrap().0, "ÿ");
    assert_eq!(
      convert::<ByteString>(scope, "'\\xff'", &o).unwrap().len(),
      2
    );
    assert!(convert::<ByteString>(scope, "'€'", &o).is_err());
    assert_eq!(convert::<ByteString>(scope, "''", &o).unwrap().0, "");
    let o = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(convert::<ByteString>(scope, "null", &o).unwrap().0, "");
  }

  #[test]
  fn big_ints() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let negative = convert::<BigInt>(scope, "-1n", &()).unwrap();
    assert_eq!(
      negative,
      BigInt {
        sign: true,
        words: vec![1]
      }
    );
    let big = convert::<BigInt>(scope, "2n ** 64n", &()).unwrap();
    assert_eq!(
      big,
      BigInt {
        sign: false,
        words: vec![0, 1]
      }
    );
    let zero = convert::<BigInt>(scope, "0n", &()).unwrap();
    assert!(!zero.sign && zero.words.iter().all(|w| *w == 0));
    let huge = convert::<BigInt>(scope, "-(2n ** 128n)", &()).unwrap();
    assert_eq!(
      huge,
      BigInt {
        sign: true,
        words: vec![0, 0, 1]
      }
    );
    assert_eq!(
      convert::<BigInt>(scope, "true", &()).unwrap().words,
      vec![1]
    );
    assert_eq!(
      convert::<BigInt>(scope, "'0x10'", &()).unwrap().words,
      vec![16]
    );
    assert_eq!(
      convert::<BigInt>(scope, "' 12 '", &()).unwrap().words,
      vec![12]
    );
    assert!(convert::<BigInt>(scope, "'1.5'", &()).is_err());
    assert!(convert::<BigInt>(scope, "'abc'", &()).is_err());
    assert!(convert::<BigInt>(scope, "1", &()).is_err());
    assert!(convert::<BigInt>(scope, "undefined", &()).is_err());
    assert!(convert::<BigInt>(scope, "null", &()).is_err());
    assert!(convert::<BigInt>(scope, "Symbol()", &()).is_err());
  }

  #[test]
  fn options_and_sequences() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert_eq!(
      convert::<Option<u8>>(scope, "null", &Default::default()).unwrap(),
      None
    );
    assert_eq!(
      convert::<Option<u8>>(scope, "undefined", &Default::default()).unwrap(),
      None
    );
    assert_eq!(
      convert::<Option<u8>>(scope, "3", &Default::default()).unwrap(),
      Some(3)
    );
    assert!(convert::<Vec<u8>>(scope, "'abc'", &Default::default()).is_err());
    assert!(convert::<Vec<u8>>(scope, "({})", &Default::default()).is_err());
    assert_eq!(
      convert::<Vec<u8>>(scope, "new Set([1, 2])", &Default::default())
        .unwrap(),
      vec![1, 2]
    );
    assert_eq!(
      convert::<Vec<Option<u8>>>(scope, "[1,,3]", &Default::default()).unwrap(),
      vec![Some(1), None, Some(3)]
    );
    let error = convert::<Vec<f64>>(scope, "[1, 'x', 3]", &()).unwrap_err();
    assert!(
      error.to_string().contains("element 1 of 'Argument 1'"),
      "{error}"
    );
  }

  #[test]
  fn records() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let empty = convert::<HashMap<String, u8>>(
      scope,
      "Object.create({ inherited: 1 })",
      &Default::default(),
    )
    .unwrap();
    assert!(empty.is_empty());
    let hidden = convert::<HashMap<String, u8>>(
      scope,
      "Object.defineProperty({}, 'x', { value: 1, enumerable: false })",
      &Default::default(),
    )
    .unwrap();
    assert!(hidden.is_empty());
    let proxied = convert::<HashMap<String, u8>>(
      scope,
      "new Proxy({}, { ownKeys() { return ['a'] }, getOwnPropertyDescriptor() { return { value: 1, enumerable: true, configurable: true } }, get() { return 4 } })",
      &Default::default(),
    )
    .unwrap();
    assert_eq!(proxied.get("a"), Some(&4));
    let map = convert::<HashMap<String, u8>>(
      scope,
      "({ a: 1, b: 2 })",
      &Default::default(),
    )
    .unwrap();
    assert_eq!(map.len(), 2);
    assert!(
      convert::<HashMap<String, u8>>(scope, "null", &Default::default())
        .is_err()
    );
    assert!(convert::<HashMap<String, u8>>(
      scope,
      "undefined",
      &Default::default()
    )
    .is_err());
    assert!(convert::<HashMap<String, u8>>(
      scope,
      "({ [Symbol()]: 1 })",
      &Default::default()
    )
    .is_err());
    assert!(convert::<HashMap<String, u8>>(
      scope,
      "Object.defineProperty({}, 'x', { get() { throw new Error('boom') }, enumerable: true })",
      &Default::default(),
    )
    .is_err());
    // `Get` uses the original key, so a lone surrogate reads its own property.
    let surrogate = convert::<HashMap<String, u8>>(
      scope,
      "({ '\\uD800': 5 })",
      &Default::default(),
    )
    .unwrap();
    assert_eq!(surrogate.get("\u{FFFD}"), Some(&5));
  }

  #[test]
  fn thrown_exceptions_keep_their_text() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let error = convert::<u8>(
      scope,
      "({ valueOf() { throw new Error('boom') } })",
      &Default::default(),
    )
    .unwrap_err();
    assert_eq!(
      error.to_string(),
      "Failed to execute 'test': Argument 1 could not be converted: Error: boom"
    );
    let error = convert::<HashMap<String, u8>>(
      scope,
      "Object.defineProperty({}, 'x', { get() { throw new Error('boom') }, enumerable: true })",
      &Default::default(),
    )
    .unwrap_err();
    assert_eq!(
      error.to_string(),
      "Failed to execute 'test': Argument 1 could not be converted: Error: boom"
    );
    // An exception whose `toString` throws has no text, so the generic message is used.
    let error = convert::<Vec<u8>>(
      scope,
      "({ [Symbol.iterator]() { throw { toString() { throw 1 } } } })",
      &Default::default(),
    )
    .unwrap_err();
    assert_eq!(
      error.to_string(),
      "Failed to execute 'test': Argument 1 could not be converted because a JavaScript exception was thrown"
    );
  }
}
