// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversion of JavaScript values to Rust values following the WebIDL
//! specification's JavaScript-to-IDL algorithms
//! (<https://webidl.spec.whatwg.org/#js-type-mapping>).
//!
//! Types implementing [`WebIdlConverter`] can be used as `#[op2]` arguments
//! with the `#[webidl]` attribute, and dictionaries and enums can implement it
//! with `#[derive(WebIDL)]`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;

/// An error produced when a JavaScript value cannot be converted to a WebIDL
/// type. The message starts with the prefix, then the context, then what was
/// wrong, eg: `Failed to execute 'fetch': Argument 1 is not a finite number`.
#[derive(Debug)]
pub struct WebIdlError {
  pub prefix: Cow<'static, str>,
  pub context: Cow<'static, str>,
  pub kind: WebIdlErrorKind,
}

impl WebIdlError {
  pub fn new<C>(
    prefix: Cow<'static, str>,
    context: C,
    kind: WebIdlErrorKind,
  ) -> Self
  where
    C: Fn() -> Cow<'static, str>,
  {
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

impl std::error::Error for WebIdlError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match &self.kind {
      WebIdlErrorKind::Other(err) => Some(err.as_ref()),
      _ => None,
    }
  }
}

/// What went wrong in a WebIDL conversion.
#[derive(Debug)]
pub enum WebIdlErrorKind {
  /// The value is not of a kind that can be converted to the named type.
  ConvertToConverterType(&'static str),
  /// A required dictionary member is missing.
  DictionaryCannotConvertKey {
    converter: &'static str,
    key: &'static str,
  },
  /// The number is NaN or infinite where a finite number is required.
  NotFinite,
  /// The integer is outside of the range allowed by `[EnforceRange]`.
  IntRange {
    lower_bound: f64,
    upper_bound: f64,
  },
  /// The value does not fit in a single-precision float.
  Precision,
  /// The string contains a code unit above 255.
  InvalidByteString,
  /// The string is not one of the values of the enumeration.
  InvalidEnumVariant {
    converter: &'static str,
    variant: String,
  },
  /// JavaScript code run during the conversion threw an exception.
  Exception(String),
  Other(Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl fmt::Display for WebIdlErrorKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::ConvertToConverterType(ty) => {
        write!(f, "can not be converted to '{ty}'")
      }
      Self::DictionaryCannotConvertKey { converter, key } => write!(
        f,
        "can not be converted to '{converter}' because '{key}' is required in '{converter}'"
      ),
      Self::NotFinite => write!(f, "is not a finite number"),
      Self::IntRange {
        lower_bound,
        upper_bound,
      } => write!(
        f,
        "is outside the accepted range of {lower_bound} to {upper_bound}, inclusive"
      ),
      Self::Precision => write!(
        f,
        "is outside the range of a single-precision floating-point value"
      ),
      Self::InvalidByteString => write!(f, "is not a valid ByteString"),
      Self::InvalidEnumVariant { converter, variant } => write!(
        f,
        "can not be converted to '{converter}' because '{variant}' is not a valid enum value"
      ),
      Self::Exception(message) => {
        write!(f, "could not be converted: {message}")
      }
      Self::Other(err) => fmt::Display::fmt(err, f),
    }
  }
}

/// Converts a JavaScript value to a Rust value of a WebIDL type.
pub trait WebIdlConverter<'a>: Sized {
  /// Extended attributes that modify the conversion (eg: `[Clamp]`).
  type Options: Default;

  /// Converts `value`. `prefix` and `context` describe where the value came
  /// from (eg: "Failed to execute 'fetch'" and "Argument 1") and are used in
  /// error messages.
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

/// Runs `f` in a `TryCatch`, turning a thrown exception into an error.
fn try_js<'a, T>(
  scope: &mut v8::HandleScope<'a>,
  f: impl FnOnce(&mut v8::HandleScope<'a>) -> Option<T>,
) -> Result<T, WebIdlErrorKind> {
  let tc = &mut v8::TryCatch::new(scope);
  if let Some(value) = f(tc) {
    return Ok(value);
  }
  let message = match tc.exception() {
    Some(exception) => {
      tc.reset();
      exception.to_rust_string_lossy(tc)
    }
    None => String::from("execution terminated"),
  };
  Err(WebIdlErrorKind::Exception(message))
}

/// Gets dictionary member `key`, returning `None` if it is missing or
/// undefined. Used by `#[derive(WebIDL)]`.
#[doc(hidden)]
// Takes `&Cow` so the prefix is only cloned (keeping it borrowed if it is) on error.
#[allow(clippy::ptr_arg)]
pub fn dictionary_member<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  object: Option<v8::Local<'a, v8::Object>>,
  key: &'static str,
  prefix: &Cow<'static, str>,
  context: C,
) -> Result<Option<v8::Local<'a, v8::Value>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let Some(object) = object else {
    return Ok(None);
  };
  let value = try_js(scope, |scope| {
    let key = v8::String::new(scope, key)?;
    object.get(scope, key.into())
  })
  .map_err(|kind| WebIdlError::new(prefix.clone(), context, kind))?;
  Ok((!value.is_undefined()).then_some(value))
}

/// Checks that `value` can be converted to a dictionary, returning the object
/// to read members from (`None` for undefined or null). Used by
/// `#[derive(WebIDL)]`.
#[doc(hidden)]
// Takes `&Cow` so the prefix is only cloned (keeping it borrowed if it is) on error.
#[allow(clippy::ptr_arg)]
pub fn dictionary_object<'a, C>(
  value: v8::Local<'a, v8::Value>,
  converter: &'static str,
  prefix: &Cow<'static, str>,
  context: C,
) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if value.is_null_or_undefined() {
    return Ok(None);
  }
  v8::Local::<v8::Object>::try_from(value)
    .map(Some)
    .map_err(|_| {
      WebIdlError::new(
        prefix.clone(),
        context,
        WebIdlErrorKind::ConvertToConverterType(converter),
      )
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

/// ToNumber(V), with a fast path for values that are already numbers.
// Takes `&Cow` so the prefix is only cloned (keeping it borrowed if it is) on error.
#[allow(clippy::ptr_arg)]
fn to_number<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &Cow<'static, str>,
  context: C,
) -> Result<f64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if let Ok(number) = v8::Local::<v8::Number>::try_from(value) {
    return Ok(number.value());
  }
  try_js(scope, |scope| value.to_number(scope))
    .map(|number| number.value())
    .map_err(|kind| WebIdlError::new(prefix.clone(), context, kind))
}

/// `[Clamp]` and `[EnforceRange]` for integer types.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IntOptions {
  pub clamp: bool,
  pub enforce_range: bool,
}

/// 2^64 as a double.
const TWO_POW_64: f64 = 18446744073709551616.0;

/// ConvertToInt(V, bitLength, signedness) for an integer type with the given
/// bounds. The result is returned as the two's complement bits of the value,
/// which the caller truncates to the bit length of its type.
fn convert_to_int<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: Cow<'static, str>,
  context: C,
  options: &IntOptions,
  lower_bound: f64,
  upper_bound: f64,
) -> Result<u64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let x = to_number(scope, value, &prefix, &context)?;
  // Values in this range are exactly representable in an i64, and this also
  // turns -0 into +0.
  let in_range = |x: f64| x as i64 as u64;

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
    return Ok(in_range(x));
  }

  if options.clamp && !x.is_nan() {
    let x = x.clamp(lower_bound, upper_bound).round_ties_even();
    return Ok(in_range(x));
  }

  if !x.is_finite() {
    return Ok(0);
  }
  // x modulo 2^64: `%` is exact and keeps the sign of x, so the remainder is
  // an integer of magnitude below 2^64, which fits in an i128. Truncating
  // that to 64 bits gives the result modulo 2^64.
  let x = x.trunc() % TWO_POW_64;
  Ok(x as i128 as u64)
}

macro_rules! impl_int {
  ($($t:ty => $lower:expr, $upper:expr;)*) => {
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
          convert_to_int(scope, value, prefix, context, options, $lower, $upper)
            .map(|bits| bits as $t)
        }
      }
    )*
  };
}

/// 2^53 - 1, the bound for 64-bit integer types.
const MAX_SAFE_INTEGER: f64 = 9007199254740991.0;

impl_int! {
  i8 => i8::MIN as f64, i8::MAX as f64;
  u8 => 0.0, u8::MAX as f64;
  i16 => i16::MIN as f64, i16::MAX as f64;
  u16 => 0.0, u16::MAX as f64;
  i32 => i32::MIN as f64, i32::MAX as f64;
  u32 => 0.0, u32::MAX as f64;
  i64 => -MAX_SAFE_INTEGER, MAX_SAFE_INTEGER;
  u64 => 0.0, MAX_SAFE_INTEGER;
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
    if !x.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::NotFinite,
      ));
    }
    // Rounds to nearest, ties to even, and to infinity when the value is
    // closer to 2^128 than to f32::MAX.
    let y = x as f32;
    if y.is_infinite() {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::Precision,
      ));
    }
    Ok(y)
  }
}

/// The WebIDL `unrestricted float` type, which allows NaN and infinities.
#[derive(Debug, Default, Clone, Copy, PartialEq, PartialOrd)]
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
    if x.is_nan() {
      return Ok(UnrestrictedFloat(f32::NAN));
    }
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

/// The WebIDL `unrestricted double` type, which allows NaN and infinities.
#[derive(Debug, Default, Clone, Copy, PartialEq, PartialOrd)]
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
    let x = to_number(scope, value, &prefix, &context)?;
    if x.is_nan() {
      return Ok(UnrestrictedDouble(f64::NAN));
    }
    Ok(UnrestrictedDouble(x))
  }
}

/// The WebIDL `bigint` type.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct BigInt {
  /// True if the value is negative.
  pub sign: bool,
  /// The magnitude, as little-endian 64-bit words.
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
    let bigint = match v8::Local::<v8::BigInt>::try_from(value) {
      Ok(bigint) => bigint,
      Err(_) => try_js(scope, |scope| value.to_big_int(scope))
        .map_err(|kind| WebIdlError::new(prefix, context, kind))?,
    };
    let mut words = vec![0; bigint.word_count()];
    let (sign, _) = bigint.to_words_array(&mut words);
    Ok(BigInt { sign, words })
  }
}

/// `[LegacyNullToEmptyString]` for string types.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  pub treat_null_as_empty_string: bool,
}

/// The shared part of DOMString and ByteString conversion: ToString(V), or
/// `None` for null with `[LegacyNullToEmptyString]`.
fn to_v8_string<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: Cow<'static, str>,
  context: C,
  options: &StringOptions,
  converter: &'static str,
) -> Result<Option<v8::Local<'a, v8::String>>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if let Ok(string) = v8::Local::<v8::String>::try_from(value) {
    return Ok(Some(string));
  }
  if value.is_null() && options.treat_null_as_empty_string {
    return Ok(None);
  }
  if value.is_symbol() {
    return Err(WebIdlError::new(
      prefix,
      context,
      WebIdlErrorKind::ConvertToConverterType(converter),
    ));
  }
  try_js(scope, |scope| value.to_string(scope))
    .map(Some)
    .map_err(|kind| WebIdlError::new(prefix, context, kind))
}

// DOMString
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
    let string =
      to_v8_string(scope, value, prefix, context, options, "DOMString")?;
    Ok(
      string
        .map(|string| string.to_rust_string_lossy(scope))
        .unwrap_or_default(),
    )
  }
}

/// The WebIDL `ByteString` type. Each character of the string is one byte
/// (U+0000 to U+00FF).
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
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
    let Some(string) = to_v8_string(
      scope,
      value,
      prefix.clone(),
      &context,
      options,
      "ByteString",
    )?
    else {
      return Ok(ByteString::default());
    };
    let mut buffer = vec![0u16; string.length()];
    string.write(scope, &mut buffer, 0, v8::WriteOptions::NO_NULL_TERMINATION);
    buffer
      .into_iter()
      .map(|unit| u8::try_from(unit).map(char::from))
      .collect::<Result<String, _>>()
      .map(ByteString)
      .map_err(|_| {
        WebIdlError::new(prefix, context, WebIdlErrorKind::InvalidByteString)
      })
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
      return Ok(None);
    }
    T::convert(scope, value, prefix, context, options).map(Some)
  }
}

// sequence<T>
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
    let not_a_sequence = |prefix: &Cow<'static, str>| {
      WebIdlError::new(
        prefix.clone(),
        &context,
        WebIdlErrorKind::ConvertToConverterType("sequence"),
      )
    };
    let js_error = |prefix: &Cow<'static, str>, kind| {
      WebIdlError::new(prefix.clone(), &context, kind)
    };

    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(not_a_sequence(&prefix));
    };

    // GetMethod(V, @@iterator)
    let method = try_js(scope, |scope| {
      let key = v8::Symbol::get_iterator(scope);
      object.get(scope, key.into())
    })
    .map_err(|kind| js_error(&prefix, kind))?;
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
      return Err(not_a_sequence(&prefix));
    };

    // GetIteratorFromMethod(V, method)
    let iterator = try_js(scope, |scope| method.call(scope, value, &[]))
      .map_err(|kind| js_error(&prefix, kind))?;
    let Ok(iterator) = v8::Local::<v8::Object>::try_from(iterator) else {
      return Err(not_a_sequence(&prefix));
    };
    let next = try_js(scope, |scope| {
      let key = v8::String::new(scope, "next")?;
      iterator.get(scope, key.into())
    })
    .map_err(|kind| js_error(&prefix, kind))?;
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
      return Err(not_a_sequence(&prefix));
    };

    let mut out = vec![];
    loop {
      // IteratorStepValue(iteratorRecord)
      let result =
        try_js(scope, |scope| next.call(scope, iterator.into(), &[]))
          .map_err(|kind| js_error(&prefix, kind))?;
      let Ok(result) = v8::Local::<v8::Object>::try_from(result) else {
        return Err(not_a_sequence(&prefix));
      };
      let (done, item) = try_js(scope, |scope| {
        let done_key = v8::String::new(scope, "done")?;
        let done = result.get(scope, done_key.into())?.boolean_value(scope);
        if done {
          return Some((true, None));
        }
        let value_key = v8::String::new(scope, "value")?;
        Some((false, Some(result.get(scope, value_key.into())?)))
      })
      .map_err(|kind| js_error(&prefix, kind))?;
      if done {
        break;
      }
      let index = out.len();
      out.push(T::convert(
        scope,
        item.unwrap(),
        prefix.clone(),
        || format!("{}, index {index}", context()).into(),
        options,
      )?);
    }
    Ok(out)
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
    let js_error = |prefix: &Cow<'static, str>, kind| {
      WebIdlError::new(prefix.clone(), &context, kind)
    };

    // O.[[OwnPropertyKeys]]()
    let keys = try_js(scope, |scope| {
      object.get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
          mode: v8::KeyCollectionMode::OwnOnly,
          property_filter: v8::PropertyFilter::ALL_PROPERTIES,
          index_filter: v8::IndexFilter::IncludeIndices,
          key_conversion: v8::KeyConversionMode::ConvertToString,
        },
      )
    })
    .map_err(|kind| js_error(&prefix, kind))?;

    let mut out = HashMap::with_capacity(keys.length() as usize);
    for i in 0..keys.length() {
      let key = try_js(scope, |scope| keys.get_index(scope, i))
        .map_err(|kind| js_error(&prefix, kind))?;
      let Ok(name) = v8::Local::<v8::Name>::try_from(key) else {
        continue;
      };

      // O.[[GetOwnProperty]](key), skipping missing and non-enumerable keys
      let enumerable = try_js(scope, |scope| {
        let desc = object.get_own_property_descriptor(scope, name)?;
        let Ok(desc) = v8::Local::<v8::Object>::try_from(desc) else {
          return Some(false);
        };
        let enumerable_key = v8::String::new(scope, "enumerable")?;
        Some(desc.get(scope, enumerable_key.into())?.boolean_value(scope))
      })
      .map_err(|kind| js_error(&prefix, kind))?;
      if !enumerable {
        continue;
      }

      let typed_key = String::convert(
        scope,
        key,
        prefix.clone(),
        &context,
        &StringOptions::default(),
      )?;
      let value = try_js(scope, |scope| object.get(scope, key))
        .map_err(|kind| js_error(&prefix, kind))?;
      let typed_value = T::convert(
        scope,
        value,
        prefix.clone(),
        || format!("{}, key '{typed_key}'", context()).into(),
        options,
      )?;
      out.insert(typed_key, typed_value);
    }
    Ok(out)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;

  fn eval<'a>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
  ) -> v8::Local<'a, v8::Value> {
    let code = v8::String::new(scope, code).unwrap();
    let script = v8::Script::compile(scope, code, None).unwrap();
    script.run(scope).unwrap()
  }

  fn convert<'a, T: WebIdlConverter<'a>>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
    options: &T::Options,
  ) -> Result<T, WebIdlError> {
    let value = eval(scope, code);
    T::convert(scope, value, "prefix".into(), || "context".into(), options)
  }

  /// Converts with default options and unwraps the result.
  fn ok<'a, T: WebIdlConverter<'a>>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
  ) -> T {
    match convert(scope, code, &Default::default()) {
      Ok(value) => value,
      Err(err) => panic!("conversion of {code} failed: {err}"),
    }
  }

  /// Converts with default options and returns the error message.
  fn err<'a, T: WebIdlConverter<'a>>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
  ) -> String {
    match convert::<T>(scope, code, &Default::default()) {
      Ok(_) => panic!("conversion of {code} succeeded"),
      Err(err) => err.to_string(),
    }
  }

  #[test]
  fn error_message() {
    let err = WebIdlError::new(
      "Failed to execute 'fetch'".into(),
      || "Argument 1".into(),
      WebIdlErrorKind::NotFinite,
    );
    assert_eq!(
      err.to_string(),
      "Failed to execute 'fetch': Argument 1 is not a finite number"
    );
  }

  #[test]
  fn any() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let value = ok::<v8::Local<v8::Value>>(scope, "({ a: 1 })");
    assert!(value.is_object());
    let value = ok::<v8::Local<v8::Value>>(scope, "undefined");
    assert!(value.is_undefined());
  }

  #[test]
  fn boolean() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    assert!(ok::<bool>(scope, "true"));
    assert!(!ok::<bool>(scope, "false"));
    assert!(!ok::<bool>(scope, "undefined"));
    assert!(!ok::<bool>(scope, "null"));
    assert!(!ok::<bool>(scope, "0"));
    assert!(!ok::<bool>(scope, "''"));
    assert!(ok::<bool>(scope, "1"));
    assert!(ok::<bool>(scope, "'false'"));
    assert!(ok::<bool>(scope, "({})"));
  }

  #[test]
  fn integers() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<u8>(scope, "1"), 1);
    assert_eq!(ok::<u8>(scope, "255"), 255);
    assert_eq!(ok::<u8>(scope, "256"), 0);
    assert_eq!(ok::<u8>(scope, "-1"), 255);
    assert_eq!(ok::<u8>(scope, "1.9"), 1);
    assert_eq!(ok::<u8>(scope, "-1.9"), 255);
    assert_eq!(ok::<i8>(scope, "128"), -128);
    assert_eq!(ok::<i8>(scope, "-129"), 127);
    assert_eq!(ok::<i8>(scope, "-1"), -1);
    assert_eq!(ok::<u16>(scope, "65537"), 1);
    assert_eq!(ok::<i16>(scope, "32768"), -32768);
    assert_eq!(ok::<u32>(scope, "-1"), u32::MAX);
    assert_eq!(ok::<u32>(scope, "4294967296"), 0);
    assert_eq!(ok::<i32>(scope, "2147483648"), i32::MIN);
    assert_eq!(ok::<i32>(scope, "-2147483649"), i32::MAX);
    assert_eq!(ok::<i64>(scope, "-1"), -1);
    assert_eq!(ok::<i64>(scope, "2 ** 63"), i64::MIN);
    assert_eq!(ok::<i64>(scope, "2 ** 64 + 2 ** 12"), 4096);
    assert_eq!(ok::<u64>(scope, "-1"), u64::MAX);
    assert_eq!(ok::<u64>(scope, "2 ** 64"), 0);
    assert_eq!(ok::<u64>(scope, "2 ** 70 + 2 ** 20"), 1 << 20);
    assert_eq!(ok::<u64>(scope, "-(2 ** 60)"), u64::MAX - (1 << 60) + 1);
    assert_eq!(ok::<u64>(scope, "1e300"), 0);
    assert_eq!(ok::<u64>(scope, "9007199254740993"), 9007199254740992);

    // ToNumber
    assert_eq!(ok::<u32>(scope, "'42'"), 42);
    assert_eq!(ok::<u32>(scope, "true"), 1);
    assert_eq!(ok::<u32>(scope, "null"), 0);
    assert_eq!(ok::<u32>(scope, "({ valueOf() { return 7 } })"), 7);
    // NaN, infinities and -0 become 0
    for code in ["NaN", "Infinity", "-Infinity", "-0", "undefined", "'x'"] {
      assert_eq!(ok::<i32>(scope, code), 0, "{code}");
      assert_eq!(ok::<u64>(scope, code), 0, "{code}");
    }

    assert_eq!(
      err::<u32>(scope, "Symbol()"),
      "prefix: context could not be converted: TypeError: Cannot convert a Symbol value to a number"
    );
    assert_eq!(
      err::<u32>(scope, "({ valueOf() { throw new Error('boom') } })"),
      "prefix: context could not be converted: Error: boom"
    );
    assert_eq!(
      err::<u32>(scope, "1n"),
      "prefix: context could not be converted: TypeError: Cannot convert a BigInt value to a number"
    );
  }

  #[test]
  fn integers_enforce_range() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions {
      enforce_range: true,
      clamp: false,
    };

    assert_eq!(convert::<u8>(scope, "255.9", &options).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "-0.9", &options).unwrap(), 0);
    assert_eq!(convert::<i8>(scope, "-128", &options).unwrap(), -128);
    assert_eq!(
      convert::<u64>(scope, "2 ** 53 - 1", &options).unwrap(),
      9007199254740991
    );
    assert_eq!(
      convert::<i64>(scope, "-(2 ** 53) + 1", &options).unwrap(),
      -9007199254740991
    );

    assert_eq!(
      convert::<u8>(scope, "256", &options)
        .unwrap_err()
        .to_string(),
      "prefix: context is outside the accepted range of 0 to 255, inclusive"
    );
    assert_eq!(
      convert::<i8>(scope, "-129", &options)
        .unwrap_err()
        .to_string(),
      "prefix: context is outside the accepted range of -128 to 127, inclusive"
    );
    assert_eq!(
      convert::<u64>(scope, "2 ** 53", &options)
        .unwrap_err()
        .to_string(),
      "prefix: context is outside the accepted range of 0 to 9007199254740991, inclusive"
    );
    assert_eq!(
      convert::<i64>(scope, "-(2 ** 53)", &options)
        .unwrap_err()
        .to_string(),
      "prefix: context is outside the accepted range of -9007199254740991 to 9007199254740991, inclusive"
    );
    for code in ["NaN", "Infinity", "-Infinity", "undefined"] {
      assert_eq!(
        convert::<u32>(scope, code, &options)
          .unwrap_err()
          .to_string(),
        "prefix: context is not a finite number"
      );
    }
  }

  #[test]
  fn integers_clamp() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions {
      clamp: true,
      enforce_range: false,
    };

    assert_eq!(convert::<u8>(scope, "300", &options).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "-5", &options).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "Infinity", &options).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "-Infinity", &options).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "NaN", &options).unwrap(), 0);
    // Rounds to nearest, ties to even
    assert_eq!(convert::<u8>(scope, "1.5", &options).unwrap(), 2);
    assert_eq!(convert::<u8>(scope, "2.5", &options).unwrap(), 2);
    assert_eq!(convert::<u8>(scope, "2.6", &options).unwrap(), 3);
    assert_eq!(convert::<i8>(scope, "-2.5", &options).unwrap(), -2);
    assert_eq!(convert::<i8>(scope, "-1000", &options).unwrap(), -128);
    assert_eq!(convert::<i32>(scope, "-0.4", &options).unwrap(), 0);
    assert_eq!(
      convert::<u64>(scope, "2 ** 60", &options).unwrap(),
      9007199254740991
    );
    assert_eq!(
      convert::<i64>(scope, "-(2 ** 60)", &options).unwrap(),
      -9007199254740991
    );
  }

  #[test]
  fn floats() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<f64>(scope, "1.5"), 1.5);
    assert_eq!(ok::<f64>(scope, "'2.25'"), 2.25);
    assert_eq!(ok::<f64>(scope, "-0").to_bits(), (-0.0f64).to_bits());
    assert_eq!(ok::<f32>(scope, "0.1"), 0.1f32);
    assert_eq!(ok::<f32>(scope, "-0").to_bits(), (-0.0f32).to_bits());
    assert_eq!(ok::<f32>(scope, "3.4028235e38"), f32::MAX);
    assert_eq!(ok::<f32>(scope, "1e-50"), 0.0);
    for code in ["NaN", "Infinity", "-Infinity", "undefined"] {
      assert_eq!(
        err::<f64>(scope, code),
        "prefix: context is not a finite number"
      );
      assert_eq!(
        err::<f32>(scope, code),
        "prefix: context is not a finite number"
      );
    }
    assert_eq!(
      err::<f32>(scope, "1e39"),
      "prefix: context is outside the range of a single-precision floating-point value"
    );
    assert_eq!(
      err::<f32>(scope, "-(2 ** 128)"),
      "prefix: context is outside the range of a single-precision floating-point value"
    );

    assert!(ok::<UnrestrictedDouble>(scope, "NaN").is_nan());
    assert_eq!(*ok::<UnrestrictedDouble>(scope, "Infinity"), f64::INFINITY);
    assert_eq!(*ok::<UnrestrictedDouble>(scope, "-1.25"), -1.25);
    assert!(ok::<UnrestrictedFloat>(scope, "NaN").is_nan());
    assert_eq!(ok::<UnrestrictedFloat>(scope, "NaN").to_bits(), 0x7fc00000);
    assert_eq!(
      *ok::<UnrestrictedFloat>(scope, "-Infinity"),
      f32::NEG_INFINITY
    );
    assert_eq!(*ok::<UnrestrictedFloat>(scope, "1e39"), f32::INFINITY);
    assert_eq!(*ok::<UnrestrictedFloat>(scope, "0.5"), 0.5);
  }

  #[test]
  fn bigint() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(
      ok::<BigInt>(scope, "0n"),
      BigInt {
        sign: false,
        words: vec![]
      }
    );
    assert_eq!(
      ok::<BigInt>(scope, "-5n"),
      BigInt {
        sign: true,
        words: vec![5]
      }
    );
    assert_eq!(
      ok::<BigInt>(scope, "2n ** 64n + 3n"),
      BigInt {
        sign: false,
        words: vec![3, 1]
      }
    );
    // ToBigInt
    assert_eq!(
      ok::<BigInt>(scope, "true"),
      BigInt {
        sign: false,
        words: vec![1]
      }
    );
    assert_eq!(
      ok::<BigInt>(scope, "'12'"),
      BigInt {
        sign: false,
        words: vec![12]
      }
    );
    assert_eq!(
      err::<BigInt>(scope, "1"),
      "prefix: context could not be converted: TypeError: Cannot convert 1 to a BigInt"
    );
    assert!(err::<BigInt>(scope, "undefined").starts_with(
      "prefix: context could not be converted: TypeError: Cannot convert undefined to a BigInt"
    ));
    assert!(err::<BigInt>(scope, "'x'").starts_with(
      "prefix: context could not be converted: SyntaxError: Cannot convert x to a BigInt"
    ));
  }

  #[test]
  fn dom_string() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<String>(scope, "'foo'"), "foo");
    assert_eq!(ok::<String>(scope, "'\u{1F600}é'"), "\u{1F600}é");
    assert_eq!(ok::<String>(scope, "1.5"), "1.5");
    assert_eq!(ok::<String>(scope, "null"), "null");
    assert_eq!(ok::<String>(scope, "undefined"), "undefined");
    assert_eq!(ok::<String>(scope, "({ toString() { return 'x' } })"), "x");
    assert_eq!(
      err::<String>(scope, "Symbol()"),
      "prefix: context can not be converted to 'DOMString'"
    );
    assert_eq!(
      err::<String>(scope, "({ toString() { throw new TypeError('no') } })"),
      "prefix: context could not be converted: TypeError: no"
    );

    let options = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(convert::<String>(scope, "null", &options).unwrap(), "");
    assert_eq!(
      convert::<String>(scope, "undefined", &options).unwrap(),
      "undefined"
    );
  }

  #[test]
  fn byte_string() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(*ok::<ByteString>(scope, "'foo'"), "foo");
    assert_eq!(*ok::<ByteString>(scope, "'\\xff\\x00'"), "\u{ff}\u{0}");
    assert_eq!(*ok::<ByteString>(scope, "12"), "12");
    assert_eq!(
      err::<ByteString>(scope, "'\\u0100'"),
      "prefix: context is not a valid ByteString"
    );
    assert_eq!(
      err::<ByteString>(scope, "'\u{1F600}'"),
      "prefix: context is not a valid ByteString"
    );
    assert_eq!(
      err::<ByteString>(scope, "Symbol()"),
      "prefix: context can not be converted to 'ByteString'"
    );

    let options = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(*convert::<ByteString>(scope, "null", &options).unwrap(), "");
    assert_eq!(*ok::<ByteString>(scope, "null"), "null");
  }

  #[test]
  fn nullable() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<Option<u8>>(scope, "null"), None);
    assert_eq!(ok::<Option<u8>>(scope, "undefined"), None);
    assert_eq!(ok::<Option<u8>>(scope, "3"), Some(3));
    assert_eq!(ok::<Option<String>>(scope, "'x'"), Some("x".to_string()));
    assert_eq!(
      err::<Option<f64>>(scope, "NaN"),
      "prefix: context is not a finite number"
    );
    // Options are passed to the inner type
    let options = IntOptions {
      clamp: true,
      enforce_range: false,
    };
    assert_eq!(
      convert::<Option<u8>>(scope, "1000", &options).unwrap(),
      Some(255)
    );
  }

  #[test]
  fn sequence() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<Vec<u8>>(scope, "[]"), Vec::<u8>::new());
    assert_eq!(ok::<Vec<u8>>(scope, "[1, 2, 3]"), vec![1, 2, 3]);
    assert_eq!(
      ok::<Vec<String>>(scope, "new Set(['a', 'b'])"),
      vec!["a".to_string(), "b".to_string()]
    );
    assert_eq!(
      ok::<Vec<Vec<u8>>>(scope, "[[1], [2, 3]]"),
      vec![vec![1], vec![2, 3]]
    );
    assert_eq!(
      ok::<Vec<u8>>(scope, "(function*() { yield 4; yield 5; })()"),
      vec![4, 5]
    );
    // Arrays are iterated with their (observable) iterator
    assert_eq!(
      ok::<Vec<u8>>(
        scope,
        "const a = [1, 2]; a[Symbol.iterator] = function*() { yield 9; }; a"
      ),
      vec![9]
    );
    assert_eq!(
      ok::<Vec<u8>>(
        scope,
        "({ [Symbol.iterator]() { let i = 0; return { next() { return { done: i >= 2, value: i++ } } } } })"
      ),
      vec![0, 1]
    );

    for code in [
      "1",
      "'abc'",
      "null",
      "undefined",
      "({})",
      "({ [Symbol.iterator]: 1 })",
    ] {
      assert_eq!(
        err::<Vec<u8>>(scope, code),
        "prefix: context can not be converted to 'sequence'",
        "{code}"
      );
    }
    assert_eq!(
      err::<Vec<f64>>(scope, "[1, NaN]"),
      "prefix: context, index 1 is not a finite number"
    );
    assert_eq!(
      err::<Vec<Vec<f64>>>(scope, "[[1], [2, 3, Infinity]]"),
      "prefix: context, index 1, index 2 is not a finite number"
    );
    assert_eq!(
      err::<Vec<u8>>(
        scope,
        "({ [Symbol.iterator]() { throw new Error('nope') } })"
      ),
      "prefix: context could not be converted: Error: nope"
    );

    let options = IntOptions {
      clamp: false,
      enforce_range: true,
    };
    assert_eq!(
      convert::<Vec<u8>>(scope, "[1, 256]", &options)
        .unwrap_err()
        .to_string(),
      "prefix: context, index 1 is outside the accepted range of 0 to 255, inclusive"
    );
  }

  #[test]
  fn record() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<HashMap<String, u8>>(scope, "({})"), HashMap::new());
    assert_eq!(
      ok::<HashMap<String, u8>>(scope, "({ a: 1, b: '2', 3: 3 })"),
      HashMap::from([
        ("a".to_string(), 1),
        ("b".to_string(), 2),
        ("3".to_string(), 3)
      ])
    );
    // Only own enumerable properties
    assert_eq!(
      ok::<HashMap<String, u8>>(
        scope,
        "const o = Object.create({ inherited: 1 }); Object.defineProperty(o, 'hidden', { value: 2 }); o.own = 3; o"
      ),
      HashMap::from([("own".to_string(), 3)])
    );
    // Non-enumerable symbols are skipped, enumerable ones can't be converted
    assert_eq!(
      ok::<HashMap<String, u8>>(
        scope,
        "const p = { a: 1 }; Object.defineProperty(p, Symbol(), { value: 2 }); p"
      ),
      HashMap::from([("a".to_string(), 1)])
    );
    assert_eq!(
      err::<HashMap<String, u8>>(scope, "({ [Symbol()]: 1 })"),
      "prefix: context can not be converted to 'DOMString'"
    );
    assert_eq!(
      ok::<HashMap<String, Vec<String>>>(scope, "({ a: ['x', 'y'] })"),
      HashMap::from([(
        "a".to_string(),
        vec!["x".to_string(), "y".to_string()]
      )])
    );

    for code in ["1", "'abc'", "null", "undefined"] {
      assert_eq!(
        err::<HashMap<String, u8>>(scope, code),
        "prefix: context can not be converted to 'record'",
        "{code}"
      );
    }
    assert_eq!(
      err::<HashMap<String, f64>>(scope, "({ a: 1, b: NaN })"),
      "prefix: context, key 'b' is not a finite number"
    );
    assert_eq!(
      err::<HashMap<String, u8>>(
        scope,
        "({ get a() { throw new Error('getter') } })"
      ),
      "prefix: context could not be converted: Error: getter"
    );
  }

  #[derive(Debug, PartialEq, crate::WebIDL)]
  #[webidl(dictionary)]
  struct Dict {
    zebra: u8,
    apple_pie: Option<u8>,
    #[webidl(default = 42)]
    with_default: u32,
    #[webidl(default = Some("x".to_string()))]
    optional_default: Option<String>,
    #[webidl(rename = "renamedKey")]
    renamed: Option<bool>,
    #[options(clamp = true)]
    #[webidl(default = 0)]
    clamped: u8,
    #[options(enforce_range = true)]
    #[webidl(default = vec![])]
    ranged: Vec<u8>,
    #[webidl(required)]
    required_option: Option<u8>,
  }

  #[test]
  fn dictionary() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(
      ok::<Dict>(scope, "({ zebra: 1, requiredOption: null })"),
      Dict {
        zebra: 1,
        apple_pie: None,
        with_default: 42,
        optional_default: Some("x".to_string()),
        renamed: None,
        clamped: 0,
        ranged: vec![],
        required_option: None,
      }
    );
    assert_eq!(
      ok::<Dict>(
        scope,
        "({ zebra: '2', applePie: 3, withDefault: 4, optionalDefault: null, renamedKey: 1, renamed: 'ignored', clamped: 300, ranged: [5], requiredOption: 6 })"
      ),
      Dict {
        zebra: 2,
        apple_pie: Some(3),
        with_default: 4,
        optional_default: None,
        renamed: Some(true),
        clamped: 255,
        ranged: vec![5],
        required_option: Some(6),
      }
    );
    // Undefined members take their default
    assert_eq!(
      ok::<Dict>(
        scope,
        "({ zebra: 1, withDefault: undefined, applePie: undefined, requiredOption: 0 })"
      ),
      Dict {
        zebra: 1,
        apple_pie: None,
        with_default: 42,
        optional_default: Some("x".to_string()),
        renamed: None,
        clamped: 0,
        ranged: vec![],
        required_option: Some(0),
      }
    );
    // Inherited members are read
    assert_eq!(
      ok::<Dict>(scope, "Object.create({ zebra: 7, requiredOption: 8 })").zebra,
      7
    );

    // Members are read in lexicographical order of their keys
    eval(scope, "globalThis.order = []");
    ok::<Dict>(
      scope,
      "new Proxy({ zebra: 1, requiredOption: 1 }, { get(t, k) { order.push(k); return t[k] } })",
    );
    let order = ok::<Vec<String>>(scope, "order");
    assert_eq!(
      order,
      [
        "applePie",
        "clamped",
        "optionalDefault",
        "ranged",
        "renamedKey",
        "requiredOption",
        "withDefault",
        "zebra"
      ]
    );

    assert_eq!(
      err::<Dict>(scope, "({ requiredOption: 1 })"),
      "prefix: context can not be converted to 'Dict' because 'zebra' is required in 'Dict'"
    );
    assert_eq!(
      err::<Dict>(scope, "({ zebra: 1 })"),
      "prefix: context can not be converted to 'Dict' because 'requiredOption' is required in 'Dict'"
    );
    assert_eq!(
      err::<Dict>(scope, "({ zebra: 1, requiredOption: undefined })"),
      "prefix: context can not be converted to 'Dict' because 'requiredOption' is required in 'Dict'"
    );
    assert_eq!(
      err::<Dict>(scope, "undefined"),
      "prefix: context can not be converted to 'Dict' because 'requiredOption' is required in 'Dict'"
    );
    for code in ["1", "'abc'", "true", "Symbol()", "1n"] {
      assert_eq!(
        err::<Dict>(scope, code),
        "prefix: context can not be converted to 'Dict'",
        "{code}"
      );
    }
    assert_eq!(
      err::<Dict>(scope, "({ zebra: 1, requiredOption: 1, ranged: [1, 256] })"),
      "prefix: 'ranged' of 'Dict' (context), index 1 is outside the accepted range of 0 to 255, inclusive"
    );
    assert_eq!(
      err::<Dict>(
        scope,
        "({ zebra: 1, requiredOption: 1, get withDefault() { throw new Error('getter') } })"
      ),
      "prefix: context could not be converted: Error: getter"
    );
  }

  #[derive(Debug, Default, PartialEq, crate::WebIDL)]
  #[webidl(dictionary)]
  struct Empty {
    a: Option<u8>,
  }

  #[derive(crate::WebIDL)]
  #[webidl(dictionary)]
  struct Nested<'a> {
    #[webidl(default = Empty::default())]
    inner: Empty,
    any: Option<v8::Local<'a, v8::Value>>,
    list: Option<Vec<Empty>>,
  }

  #[test]
  fn dictionary_nested() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    // undefined and null convert to an empty dictionary
    assert_eq!(ok::<Empty>(scope, "undefined"), Empty { a: None });
    assert_eq!(ok::<Empty>(scope, "null"), Empty { a: None });
    assert_eq!(ok::<Empty>(scope, "[]"), Empty { a: None });

    let nested = ok::<Nested>(
      scope,
      "({ inner: { a: 1 }, any: 'foo', list: [{}, { a: 2 }] })",
    );
    assert_eq!(nested.inner, Empty { a: Some(1) });
    assert!(nested.any.unwrap().is_string());
    assert_eq!(
      nested.list.unwrap(),
      vec![Empty { a: None }, Empty { a: Some(2) }]
    );

    let nested = ok::<Nested>(scope, "({})");
    assert_eq!(nested.inner, Empty { a: None });
    assert!(nested.any.is_none());
    assert!(nested.list.is_none());

    assert_eq!(
      err::<Nested>(scope, "({ inner: 5 })"),
      "prefix: 'inner' of 'Nested' (context) can not be converted to 'Empty'"
    );
    assert_eq!(
      err::<Nested>(scope, "({ list: [{}, 1] })"),
      "prefix: 'list' of 'Nested' (context), index 1 can not be converted to 'Empty'"
    );
  }

  #[derive(Debug, PartialEq, crate::WebIDL)]
  #[webidl(enum)]
  enum Enumeration {
    FooBar,
    Baz,
    #[webidl(rename = "hello world")]
    Renamed,
  }

  #[test]
  fn enumeration() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<Enumeration>(scope, "'foo-bar'"), Enumeration::FooBar);
    assert_eq!(ok::<Enumeration>(scope, "'baz'"), Enumeration::Baz);
    assert_eq!(
      ok::<Enumeration>(scope, "'hello world'"),
      Enumeration::Renamed
    );
    assert_eq!(
      ok::<Enumeration>(scope, "({ toString() { return 'baz' } })"),
      Enumeration::Baz
    );
    for (code, variant) in [
      ("'FooBar'", "FooBar"),
      ("'renamed'", "renamed"),
      ("''", ""),
      ("undefined", "undefined"),
      ("1", "1"),
    ] {
      assert_eq!(
        err::<Enumeration>(scope, code),
        format!("prefix: context can not be converted to 'Enumeration' because '{variant}' is not a valid enum value")
      );
    }
    assert_eq!(
      err::<Enumeration>(scope, "Symbol()"),
      "prefix: context can not be converted to 'DOMString'"
    );
    assert_eq!(
      ok::<Vec<Enumeration>>(scope, "['baz', 'foo-bar']"),
      vec![Enumeration::Baz, Enumeration::FooBar]
    );
  }
}
