// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversions from JavaScript values to Rust types, following the
//! [WebIDL JavaScript-to-IDL type mapping](https://webidl.spec.whatwg.org/#js-type-mapping).
//!
//! The `#[derive(WebIDL)]` macro (re-exported as [`crate::WebIDL`]) generates
//! [`WebIdlConverter`] implementations for dictionaries and enums, and the
//! `#[webidl]` argument attribute of `#[op2]` uses the same trait.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;

use crate::v8;

/// An error produced when a JavaScript value can not be converted to a
/// WebIDL type. Displays as `"{prefix}: {context} {kind}"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIdlError {
  /// The prefix, such as `Failed to execute 'fetch'`.
  pub prefix: Cow<'static, str>,
  /// The context, such as `Argument 1`.
  pub context: Cow<'static, str>,
  /// What was wrong with the value.
  pub kind: WebIdlErrorKind,
}

impl WebIdlError {
  /// Creates a new error from its parts.
  pub fn new(
    prefix: Cow<'static, str>,
    context: Cow<'static, str>,
    kind: WebIdlErrorKind,
  ) -> Self {
    Self {
      prefix,
      context,
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

/// The reason why a JavaScript value could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebIdlErrorKind {
  /// The value is NaN or infinite where a finite number is required.
  NotFinite,
  /// Any other failure, described by the text that follows the context.
  Other(Cow<'static, str>),
}

impl fmt::Display for WebIdlErrorKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      WebIdlErrorKind::NotFinite => f.write_str("is not a finite number"),
      WebIdlErrorKind::Other(message) => f.write_str(message),
    }
  }
}

/// Converts a JavaScript value into a WebIDL type.
pub trait WebIdlConverter<'a>: Sized {
  /// Options that change how the value is converted (for example
  /// `[Clamp]` or `[EnforceRange]` on integers).
  type Options: Default;

  /// Converts `value`. `prefix` and `context` are used to build the error
  /// message; `context` is only called when an error is constructed.
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

/// Options for the integer converters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  /// `[Clamp]`: out-of-range values are clamped to the range of the type,
  /// and values are rounded to the nearest integer (ties to even).
  pub clamp: bool,
  /// `[EnforceRange]`: NaN, infinite and out-of-range values are an error.
  /// Takes precedence over `clamp` when both are set.
  pub enforce_range: bool,
}

/// Options for the string converters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  /// `[LegacyNullToEmptyString]`: `null` converts to the empty string.
  pub treat_null_as_empty_string: bool,
}

/// A WebIDL `unrestricted float`, which may be NaN or infinite.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UnrestrictedFloat(pub f32);

impl Deref for UnrestrictedFloat {
  type Target = f32;

  fn deref(&self) -> &f32 {
    &self.0
  }
}

/// A WebIDL `unrestricted double`, which may be NaN or infinite.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UnrestrictedDouble(pub f64);

impl Deref for UnrestrictedDouble {
  type Target = f64;

  fn deref(&self) -> &f64 {
    &self.0
  }
}

/// A WebIDL `ByteString`: a string whose code units are all at most 0xFF.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ByteString(pub String);

impl Deref for ByteString {
  type Target = String;

  fn deref(&self) -> &String {
    &self.0
  }
}

/// A WebIDL `bigint`: the sign and the little-endian 64-bit magnitude words.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BigInt {
  /// True when the value is negative.
  pub sign: bool,
  /// The magnitude, least significant word first. Empty for `0n`.
  pub words: Vec<u64>,
}

fn other(message: impl Into<Cow<'static, str>>) -> WebIdlErrorKind {
  WebIdlErrorKind::Other(message.into())
}

/// Builds an error. `context` is only called here, on the error path.
fn error<C>(prefix: &str, context: &C, kind: WebIdlErrorKind) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  WebIdlError::new(Cow::Owned(prefix.to_owned()), context(), kind)
}

/// Describes the exception caught by `tc`.
fn exception_message(tc: &mut v8::TryCatch<v8::HandleScope>) -> String {
  let Some(exception) = tc.exception() else {
    return "unknown exception".to_owned();
  };
  match exception.to_string(tc) {
    Some(message) => message.to_rust_string_lossy(tc),
    None => "unknown exception".to_owned(),
  }
}

/// Runs a V8 operation that may throw. A thrown exception is caught (so no
/// exception is left pending on the scope) and returned as an error.
fn try_catch<'a, T, C>(
  scope: &mut v8::HandleScope<'a>,
  prefix: &str,
  context: &C,
  f: impl FnOnce(&mut v8::HandleScope<'a>) -> Option<T>,
) -> Result<T, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let tc = &mut v8::TryCatch::new(scope);
  match f(tc) {
    Some(value) => Ok(value),
    None => {
      let message = exception_message(tc);
      Err(error(
        prefix,
        context,
        other(format!("threw an exception: {message}")),
      ))
    }
  }
}

/// ToNumber.
fn to_number<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<f64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  try_catch(scope, prefix, context, |s| value.to_number(s))
    .map(|number| number.value())
}

/// ToString, converting lone surrogates to U+FFFD.
fn to_string_lossy<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
) -> Result<String, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let string = try_catch(scope, prefix, context, |s| value.to_string(s))?;
  Ok(string.to_rust_string_lossy(scope))
}

/// Shared integer conversion (ConvertToInt). Returns the integer before it is
/// narrowed to the target type; `as` casts from the result implement the
/// modular wrap-around of the default conversion.
fn convert_to_integer<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
  options: &IntOptions,
  (lower, upper): (f64, f64),
) -> Result<i128, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let x = to_number(scope, value, prefix, context)?;
  if options.enforce_range {
    if !x.is_finite() {
      return Err(error(prefix, context, WebIdlErrorKind::NotFinite));
    }
    let x = x.trunc();
    if x < lower || x > upper {
      return Err(error(
        prefix,
        context,
        other(format!("is outside the range [{lower}, {upper}]")),
      ));
    }
    Ok(x as i128)
  } else if options.clamp {
    if x.is_nan() {
      return Ok(0);
    }
    Ok(x.clamp(lower, upper).round_ties_even() as i128)
  } else {
    if !x.is_finite() {
      return Ok(0);
    }
    let x = x.trunc();
    // Doubles of this magnitude are multiples of 2^64, so they wrap to zero
    // for every integer width used here.
    if x.abs() >= 2f64.powi(127) {
      return Ok(0);
    }
    Ok(x as i128)
  }
}

macro_rules! impl_integer {
  ($($ty:ty => ($lower:expr, $upper:expr)),* $(,)?) => {$(
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
        let integer = convert_to_integer(
          scope,
          value,
          &prefix,
          &context,
          options,
          ($lower, $upper),
        )?;
        Ok(integer as $ty)
      }
    }
  )*};
}

impl_integer! {
  i8 => (-128.0, 127.0),
  u8 => (0.0, 255.0),
  i16 => (-32768.0, 32767.0),
  u16 => (0.0, 65535.0),
  i32 => (-2147483648.0, 2147483647.0),
  u32 => (0.0, 4294967295.0),
  i64 => (-9007199254740991.0, 9007199254740991.0),
  u64 => (0.0, 9007199254740991.0),
}

/// Any JavaScript value, returned unchanged.
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

/// A JavaScript boolean, using ToBoolean. Never fails.
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

/// A WebIDL `double`: ToNumber, and NaN or infinite values are an error.
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
      return Err(error(&prefix, &context, WebIdlErrorKind::NotFinite));
    }
    Ok(x)
  }
}

/// A WebIDL `float`: rounded to f32, and an error if that overflows.
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
    let y = x as f32;
    if !x.is_finite() || !y.is_finite() {
      return Err(error(&prefix, &context, WebIdlErrorKind::NotFinite));
    }
    Ok(y)
  }
}

/// A WebIDL `unrestricted double`: NaN and infinities are accepted.
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

/// A WebIDL `unrestricted float`: rounds to f32, overflowing to infinity.
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

/// A WebIDL `DOMString`: ToString, with lone surrogates replaced by U+FFFD.
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
    to_string_lossy(scope, value, &prefix, &context)
  }
}

/// A WebIDL `ByteString`: ToString, and an error if any code unit is above
/// 0xFF.
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
    // Lone surrogates were already replaced by U+FFFD, which is above 0xFF.
    if string.chars().any(|c| c as u32 > 0xFF) {
      return Err(error(&prefix, &context, other("is not a valid ByteString")));
    }
    Ok(ByteString(string))
  }
}

/// A WebIDL `bigint`, following ToBigInt.
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
    let big_int = try_catch(scope, &prefix, &context, |s| value.to_big_int(s))?;
    let mut buffer = vec![0u64; big_int.word_count()];
    let (sign, words) = big_int.to_words_array(&mut buffer);
    Ok(BigInt {
      sign,
      words: words.to_vec(),
    })
  }
}

/// A WebIDL nullable type: `null` and `undefined` are `None`.
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

/// A WebIDL `sequence<T>`: any iterable object, read with the iterator
/// protocol.
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
    let not_iterable =
      || error(&prefix, &context, other("is not an iterable object"));
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(error(&prefix, &context, other("is not an object")));
    };
    let iterator_key: v8::Local<v8::Value> =
      v8::Symbol::get_iterator(scope).into();
    let method =
      try_catch(scope, &prefix, &context, |s| object.get(s, iterator_key))?;
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
      return Err(not_iterable());
    };
    let iterator =
      try_catch(scope, &prefix, &context, |s| method.call(s, value, &[]))?;
    let Ok(iterator) = v8::Local::<v8::Object>::try_from(iterator) else {
      return Err(not_iterable());
    };
    let next_key: v8::Local<v8::Value> =
      v8::String::new(scope, "next").unwrap().into();
    let next =
      try_catch(scope, &prefix, &context, |s| iterator.get(s, next_key))?;
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
      return Err(not_iterable());
    };
    let done_key: v8::Local<v8::Value> =
      v8::String::new(scope, "done").unwrap().into();
    let value_key: v8::Local<v8::Value> =
      v8::String::new(scope, "value").unwrap().into();

    let mut items = Vec::new();
    loop {
      let result = try_catch(scope, &prefix, &context, |s| {
        next.call(s, iterator.into(), &[])
      })?;
      let Ok(result) = v8::Local::<v8::Object>::try_from(result) else {
        return Err(error(
          &prefix,
          &context,
          other("is not an iterator result object"),
        ));
      };
      let done =
        try_catch(scope, &prefix, &context, |s| result.get(s, done_key))?;
      if done.boolean_value(scope) {
        break;
      }
      let item =
        try_catch(scope, &prefix, &context, |s| result.get(s, value_key))?;
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

/// A WebIDL `record<DOMString, T>`: the own enumerable string-keyed
/// properties of an object. An enumerable Symbol-keyed property is an error.
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
      return Err(error(&prefix, &context, other("is not an object")));
    };
    let args = v8::GetPropertyNamesArgs {
      mode: v8::KeyCollectionMode::OwnOnly,
      property_filter: v8::PropertyFilter::ONLY_ENUMERABLE,
      index_filter: v8::IndexFilter::IncludeIndices,
      key_conversion: v8::KeyConversionMode::KeepNumbers,
    };
    let keys = try_catch(scope, &prefix, &context, |s| {
      object.get_own_property_names(s, args)
    })?;
    let mut record = HashMap::new();
    for i in 0..keys.length() {
      let key = try_catch(scope, &prefix, &context, |s| keys.get_index(s, i))?;
      // The Get uses the original key: a lone surrogate would otherwise be
      // replaced by U+FFFD and read the wrong property.
      let name = to_string_lossy(scope, key, &prefix, &context)?;
      let member = try_catch(scope, &prefix, &context, |s| object.get(s, key))?;
      let key_context = || Cow::Owned(format!("{}, key '{}'", context(), name));
      let converted = <T as WebIdlConverter<'a>>::convert(
        scope,
        member,
        prefix.clone(),
        key_context,
        options,
      )?;
      record.insert(name, converted);
    }
    Ok(record)
  }
}

/// Returns the object that dictionary members are read from. `undefined`
/// and `null` yield `None`, so every member is treated as missing. Used by
/// `#[derive(WebIDL)]`.
pub fn dictionary_object<'a, C>(
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &C,
  dictionary: &str,
) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_null_or_undefined() {
    return Ok(None);
  }
  match v8::Local::<v8::Object>::try_from(value) {
    Ok(object) => Ok(Some(object)),
    Err(_) => Err(error(
      prefix,
      context,
      other(format!(
        "can not be converted to '{dictionary}' because it is not an object"
      )),
    )),
  }
}

/// Reads dictionary member `key` with Get. Returns `None` when the member is
/// missing or `undefined`. Used by `#[derive(WebIDL)]`.
pub fn dictionary_member<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  object: Option<v8::Local<'a, v8::Object>>,
  key: &str,
  prefix: &str,
  context: &C,
) -> Result<Option<v8::Local<'a, v8::Value>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let Some(object) = object else {
    return Ok(None);
  };
  let Some(key) = v8::String::new(scope, key) else {
    return Err(error(prefix, context, other("has an invalid member name")));
  };
  let member =
    try_catch(scope, prefix, context, |s| object.get(s, key.into()))?;
  if member.is_undefined() {
    Ok(None)
  } else {
    Ok(Some(member))
  }
}

/// The error for a required dictionary member that is missing. Used by
/// `#[derive(WebIDL)]`.
pub fn missing_dictionary_member<C>(
  prefix: &str,
  context: &C,
  dictionary: &str,
  key: &str,
) -> WebIdlError
where
  C: Fn() -> Cow<'static, str>,
{
  error(
    prefix,
    context,
    other(format!(
      "can not be converted to '{dictionary}' because '{key}' is required in '{dictionary}'"
    )),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;
  use crate::RuntimeOptions;

  const PREFIX: &str = "Failed to execute 'test'";
  const CONTEXT: &str = "Argument 1";

  /// Evaluates `js` in a fresh runtime and passes the resulting value to `f`.
  fn with_value<R>(
    js: &str,
    f: impl for<'s> FnOnce(&mut v8::HandleScope<'s>, v8::Local<'s, v8::Value>) -> R,
  ) -> R {
    let mut runtime = JsRuntime::new(RuntimeOptions::default());
    let global = runtime.execute_script("<test>", js.to_owned()).unwrap();
    let scope = &mut runtime.handle_scope();
    let value = v8::Local::new(scope, global);
    f(scope, value)
  }

  /// Converts the result of evaluating `js` to `T`.
  fn convert<T, O>(js: &str, options: &O) -> Result<T, String>
  where
    T: for<'a> WebIdlConverter<'a, Options = O>,
  {
    with_value(js, |scope, value| {
      <T as WebIdlConverter<'_>>::convert(
        scope,
        value,
        Cow::Borrowed(PREFIX),
        || Cow::Borrowed(CONTEXT),
        options,
      )
      .map_err(|e| e.to_string())
    })
  }

  fn ok<T>(js: &str) -> T
  where
    T: for<'a> WebIdlConverter<'a, Options = ()> + std::fmt::Debug,
  {
    convert::<T, _>(js, &()).unwrap()
  }

  fn int<T>(js: &str, options: IntOptions) -> Result<T, String>
  where
    T: for<'a> WebIdlConverter<'a, Options = IntOptions>,
  {
    convert::<T, _>(js, &options)
  }

  fn string(js: &str, options: StringOptions) -> Result<String, String> {
    convert::<String, _>(js, &options)
  }

  const CLAMP: IntOptions = IntOptions {
    clamp: true,
    enforce_range: false,
  };
  const ENFORCE: IntOptions = IntOptions {
    clamp: false,
    enforce_range: true,
  };

  #[test]
  fn error_display_and_empty_parts() {
    let err = WebIdlError::new(
      Cow::Borrowed("Failed to execute 'fetch'"),
      Cow::Borrowed("Argument 1"),
      WebIdlErrorKind::NotFinite,
    );
    assert_eq!(
      err.to_string(),
      "Failed to execute 'fetch': Argument 1 is not a finite number"
    );
    let empty =
      WebIdlError::new(Cow::Borrowed(""), Cow::Borrowed(""), other("is wrong"));
    assert_eq!(empty.to_string(), ":  is wrong");
    let _: &dyn std::error::Error = &empty;
  }

  #[test]
  fn error_context_only_built_on_error() {
    use std::cell::Cell;
    let calls = Cell::new(0);
    let context = || {
      calls.set(calls.get() + 1);
      Cow::Borrowed(CONTEXT)
    };
    let prefix = Cow::Borrowed(PREFIX);
    let ok = with_value("5", |scope, value| {
      <u8 as WebIdlConverter<'_>>::convert(
        scope,
        value,
        prefix.clone(),
        context,
        &IntOptions::default(),
      )
    });
    assert_eq!(ok, Ok(5));
    assert_eq!(calls.get(), 0);
    let err = error(&prefix, &context, WebIdlErrorKind::NotFinite);
    assert_eq!(err.context, CONTEXT);
    assert_eq!(calls.get(), 1);
    let _ = error(&prefix, &context, WebIdlErrorKind::NotFinite);
    assert_eq!(calls.get(), 2);
  }

  #[test]
  fn any_is_returned_unchanged() {
    let is_object = with_value("({a: 1})", |scope, value| {
      <v8::Local<v8::Value> as WebIdlConverter<'_>>::convert(
        scope,
        value,
        Cow::Borrowed(PREFIX),
        || Cow::Borrowed(CONTEXT),
        &(),
      )
      .unwrap()
      .is_object()
    });
    assert!(is_object);
  }

  #[test]
  fn bool_uses_to_boolean() {
    for js in ["0", "''", "null", "undefined", "NaN"] {
      assert!(!ok::<bool>(js), "{js}");
    }
    for js in ["'0'", "'false'", "({})", "[]", "1"] {
      assert!(ok::<bool>(js), "{js}");
    }
  }

  #[test]
  fn integer_convert_to_int() {
    let d = IntOptions::default();
    assert_eq!(int::<i32>("'12'", d), Ok(12));
    assert_eq!(int::<u8>("true", d), Ok(1));
    assert_eq!(int::<u8>("null", d), Ok(0));
    assert_eq!(int::<i32>("({valueOf: () => 5})", d), Ok(5));
    assert_eq!(int::<i32>("-0", d), Ok(0));
    assert_eq!(int::<i32>("NaN", d), Ok(0));
    assert_eq!(int::<i32>("Infinity", d), Ok(0));
    assert_eq!(int::<u8>("3.9", d), Ok(3));
    assert_eq!(int::<u8>("-3.9", d), Ok(253));
    assert_eq!(int::<u8>("256", d), Ok(0));
    assert_eq!(int::<u8>("-1", d), Ok(255));
    assert_eq!(int::<u32>("2 ** 32 + 5", d), Ok(5));
    assert_eq!(int::<i8>("128", d), Ok(-128));
    assert_eq!(int::<i8>("255", d), Ok(-1));
    assert_eq!(int::<i8>("-129", d), Ok(127));
    assert_eq!(int::<u64>("-1", d), Ok(u64::MAX));
    assert_eq!(int::<i64>("2 ** 63", d), Ok(i64::MIN));
    assert_eq!(int::<u64>("1e300", d), Ok(0));
  }

  #[test]
  fn integer_bigint_is_an_error() {
    let err = int::<i32>("1n", IntOptions::default()).unwrap_err();
    assert!(err.starts_with(
      "Failed to execute 'test': Argument 1 threw an exception: TypeError:"
    ));
  }

  #[test]
  fn integer_value_of_throws() {
    let err = int::<i32>(
      r#"({valueOf() { throw new Error("boom") }})"#,
      IntOptions::default(),
    )
    .unwrap_err();
    assert_eq!(
      err,
      "Failed to execute 'test': Argument 1 threw an exception: Error: boom"
    );
  }

  #[test]
  fn integer_enforce_range() {
    assert_eq!(int::<i8>("127.9", ENFORCE), Ok(127));
    assert_eq!(int::<i8>("-128.5", ENFORCE), Ok(-128));
    assert_eq!(
      int::<i8>("128", ENFORCE).unwrap_err(),
      "Failed to execute 'test': Argument 1 is outside the range [-128, 127]"
    );
    assert_eq!(
      int::<i8>("-129", ENFORCE).unwrap_err(),
      "Failed to execute 'test': Argument 1 is outside the range [-128, 127]"
    );
    assert_eq!(
      int::<i32>("NaN", ENFORCE).unwrap_err(),
      "Failed to execute 'test': Argument 1 is not a finite number"
    );
    assert_eq!(
      int::<i32>("-Infinity", ENFORCE).unwrap_err(),
      "Failed to execute 'test': Argument 1 is not a finite number"
    );
    assert_eq!(int::<i64>("2 ** 53 - 1", ENFORCE), Ok(9007199254740991));
    assert!(int::<i64>("2 ** 53", ENFORCE).is_err());
    assert!(int::<u64>("2 ** 53", ENFORCE).is_err());
    let both = IntOptions {
      clamp: true,
      enforce_range: true,
    };
    assert!(int::<u8>("300", both).is_err());
  }

  #[test]
  fn integer_clamp() {
    assert_eq!(int::<u8>("300", CLAMP), Ok(255));
    assert_eq!(int::<u8>("-5", CLAMP), Ok(0));
    assert_eq!(int::<u8>("2.5", CLAMP), Ok(2));
    assert_eq!(int::<u8>("3.5", CLAMP), Ok(4));
    assert_eq!(int::<i8>("-2.5", CLAMP), Ok(-2));
    assert_eq!(int::<i8>("Infinity", CLAMP), Ok(127));
    assert_eq!(int::<i8>("-Infinity", CLAMP), Ok(-128));
    assert_eq!(int::<i8>("NaN", CLAMP), Ok(0));
  }

  #[test]
  fn integer_symbol_is_an_error() {
    let err = int::<i32>("Symbol()", IntOptions::default()).unwrap_err();
    assert!(err.starts_with(
      "Failed to execute 'test': Argument 1 threw an exception: TypeError:"
    ));
  }

  #[test]
  fn double_and_float() {
    assert_eq!(convert::<f64, _>("'1.5'", &()), Ok(1.5));
    assert_eq!(
      convert::<f64, _>("undefined", &()).unwrap_err(),
      "Failed to execute 'test': Argument 1 is not a finite number"
    );
    assert_eq!(
      convert::<f64, _>("'abc'", &()).unwrap_err(),
      "Failed to execute 'test': Argument 1 is not a finite number"
    );
    assert_eq!(
      convert::<f32, _>("3.4028234663852886e38", &()),
      Ok(f32::MAX)
    );
    assert_eq!(
      convert::<f32, _>("1e39", &()).unwrap_err(),
      "Failed to execute 'test': Argument 1 is not a finite number"
    );
    let nan = convert::<UnrestrictedDouble, _>("NaN", &()).unwrap();
    assert!(nan.is_nan());
    let inf = convert::<UnrestrictedDouble, _>("-Infinity", &()).unwrap();
    assert_eq!(*inf, f64::NEG_INFINITY);
    let big = convert::<UnrestrictedFloat, _>("1e39", &()).unwrap();
    assert_eq!(*big, f32::INFINITY);
    let nan = convert::<UnrestrictedFloat, _>("NaN", &()).unwrap();
    assert!(nan.is_nan());
  }

  #[test]
  fn domstring() {
    let d = StringOptions::default();
    assert_eq!(string("5", d), Ok("5".to_owned()));
    assert_eq!(string("null", d), Ok("null".to_owned()));
    assert_eq!(string("undefined", d), Ok("undefined".to_owned()));
    assert_eq!(string("({toString: () => 'hi'})", d), Ok("hi".to_owned()));
    let null_empty = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(string("null", null_empty), Ok(String::new()));
    assert_eq!(string("undefined", null_empty), Ok("undefined".to_owned()));
    assert_eq!(string("'héllo €😀'", d), Ok("héllo €😀".to_owned()));
    assert_eq!(string("'\\uD800'", d), Ok("\u{FFFD}".to_owned()));
    assert_eq!(
      string("Symbol()", d).unwrap_err(),
      "Failed to execute 'test': Argument 1 threw an exception: TypeError: Cannot convert a Symbol value to a string"
    );
  }

  #[test]
  fn bytestring() {
    let bs = |js: &str| convert::<ByteString, _>(js, &StringOptions::default());
    assert_eq!(bs("'é'").unwrap().0, "é");
    assert_eq!(bs("'\\u0000'").unwrap().0, "\u{0}");
    assert_eq!(
      bs("'€'").unwrap_err(),
      "Failed to execute 'test': Argument 1 is not a valid ByteString"
    );
    assert_eq!(
      bs("'\\uD800'").unwrap_err(),
      "Failed to execute 'test': Argument 1 is not a valid ByteString"
    );
    assert_eq!(
      convert::<ByteString, _>(
        "null",
        &StringOptions {
          treat_null_as_empty_string: true
        }
      )
      .unwrap()
      .0,
      ""
    );
    assert!(
      convert::<ByteString, _>("Symbol()", &StringOptions::default()).is_err()
    );
  }

  #[test]
  fn bigint() {
    let big = |js: &str| convert::<BigInt, _>(js, &()).unwrap();
    assert_eq!(
      big("0n"),
      BigInt {
        sign: false,
        words: vec![]
      }
    );
    assert_eq!(
      big("true"),
      BigInt {
        sign: false,
        words: vec![1]
      }
    );
    assert_eq!(
      big("false"),
      BigInt {
        sign: false,
        words: vec![]
      }
    );
    assert_eq!(
      big("'123'"),
      BigInt {
        sign: false,
        words: vec![123]
      }
    );
    assert_eq!(
      big("-(2n ** 64n + 3n)"),
      BigInt {
        sign: true,
        words: vec![3, 1]
      }
    );
    for js in ["5", "undefined", "null", "Symbol()", "'abc'"] {
      assert!(convert::<BigInt, _>(js, &()).is_err(), "{js}");
    }
  }

  #[test]
  fn nullable() {
    assert_eq!(convert::<Option<bool>, _>("null", &()), Ok(None));
    assert_eq!(convert::<Option<bool>, _>("undefined", &()), Ok(None));
    assert_eq!(convert::<Option<bool>, _>("1", &()), Ok(Some(true)));
    assert_eq!(
      convert::<Option<u8>, _>("'x'", &IntOptions::default()),
      Ok(Some(0))
    );
    assert_eq!(
      convert::<Option<u8>, _>("Symbol()", &IntOptions::default())
        .unwrap_err(),
      "Failed to execute 'test': Argument 1 threw an exception: TypeError: Cannot convert a Symbol value to a number"
    );
  }

  #[test]
  fn sequence() {
    let d = IntOptions::default();
    assert_eq!(convert::<Vec<u8>, _>("[1, 2, 3]", &d), Ok(vec![1, 2, 3]));
    assert_eq!(convert::<Vec<u8>, _>("new Set([4])", &d), Ok(vec![4]));
    assert_eq!(
      convert::<Vec<u8>, _>("(function*() { yield 7 })()", &d),
      Ok(vec![7])
    );
    assert_eq!(convert::<Vec<u8>, _>("[,1]", &d), Ok(vec![0, 1]));
    for js in ["5", "'abc'", "null", "undefined"] {
      assert_eq!(
        convert::<Vec<u8>, _>(js, &d).unwrap_err(),
        "Failed to execute 'test': Argument 1 is not an object",
        "{js}"
      );
    }
    for js in ["({})", "({length: 2})"] {
      assert_eq!(
        convert::<Vec<u8>, _>(js, &d).unwrap_err(),
        "Failed to execute 'test': Argument 1 is not an iterable object",
        "{js}"
      );
    }
    assert_eq!(
      convert::<Vec<f64>, _>("[1, 'x']", &()).unwrap_err(),
      "Failed to execute 'test': Argument 1, index 1 is not a finite number"
    );
    assert_eq!(
      convert::<Vec<u8>, _>(
        "({[Symbol.iterator]() { return { next() { throw new Error('boom') } } } })",
        &d
      )
      .unwrap_err(),
      "Failed to execute 'test': Argument 1 threw an exception: Error: boom"
    );
  }

  #[test]
  fn record() {
    let d = IntOptions::default();
    let map = convert::<HashMap<String, u8>, _>("({a: 1, 0: 2})", &d).unwrap();
    assert_eq!(map.len(), 2);
    assert_eq!(map["a"], 1);
    assert_eq!(map["0"], 2);
    let inherited = convert::<HashMap<String, u8>, _>(
      "(function() { const p = {x: 1}; const o = Object.create(p); \
       Object.defineProperty(o, 'h', {value: 2, enumerable: false}); \
       o.y = 3; return o })()",
      &d,
    )
    .unwrap();
    assert_eq!(inherited.len(), 1);
    assert_eq!(inherited["y"], 3);
    assert_eq!(
      convert::<HashMap<String, u8>, _>("null", &d).unwrap_err(),
      "Failed to execute 'test': Argument 1 is not an object"
    );
    assert_eq!(
      convert::<HashMap<String, f64>, _>("({a: 'x'})", &()).unwrap_err(),
      "Failed to execute 'test': Argument 1, key 'a' is not a finite number"
    );
    assert_eq!(
      convert::<HashMap<String, u8>, _>(
        "({[Symbol('s')]: 1})",
        &d
      )
      .unwrap_err(),
      "Failed to execute 'test': Argument 1 threw an exception: TypeError: Cannot convert a Symbol value to a string"
    );
    let skipped = convert::<HashMap<String, u8>, _>(
      "(function() { const o = {a: 1}; Object.defineProperty(o, Symbol('hidden'), {value: 9, enumerable: false}); return o })()",
      &d,
    )
    .unwrap();
    assert_eq!(skipped.len(), 1);
  }

  #[test]
  fn getter_throws_in_record() {
    let err = convert::<HashMap<String, u8>, _>(
      "({get a() { throw new Error('boom') }})",
      &IntOptions::default(),
    )
    .unwrap_err();
    assert_eq!(
      err,
      "Failed to execute 'test': Argument 1 threw an exception: Error: boom"
    );
  }

  #[test]
  fn record_lone_surrogate_key_reads_its_own_value() {
    let map = convert::<HashMap<String, bool>, _>(
      r#"(function() { const o = {}; Object.defineProperty(o, '\uD800', {value: true, enumerable: true}); return o })()"#,
      &(),
    )
    .unwrap();
    assert_eq!(map.len(), 1);
    assert!(map["\u{FFFD}"]);
  }

  #[test]
  fn no_pending_exception_after_error() {
    with_value(
      "({valueOf() { throw new Error('boom') }})",
      |scope, value| {
        let result = <i32 as WebIdlConverter<'_>>::convert(
          scope,
          value,
          Cow::Borrowed(PREFIX),
          || Cow::Borrowed(CONTEXT),
          &IntOptions::default(),
        );
        assert!(result.is_err());
      },
    );
  }
}
