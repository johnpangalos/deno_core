// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Behavior of `#[derive(WebIDL)]` against a real V8 isolate. Error cases
//! compare the exact message.

use super::*;
use crate::JsRuntime;
use crate::RuntimeOptions;
use crate::WebIDL;

/// The prefix and context used by every test conversion.
const OP: &str = "Failed to execute 'op': Argument 1";

#[derive(WebIDL, Debug, PartialEq)]
#[webidl(dictionary)]
struct Dict {
  foo_bar: u8,
  #[webidl(default = 7)]
  with_default: u8,
  maybe: Option<String>,
  #[webidl(rename = "renamed")]
  other: String,
  #[webidl(required)]
  must: Option<u8>,
  #[options(clamp = true)]
  #[webidl(default = 1)]
  clamped: u8,
  #[options(treat_null_as_empty_string = true)]
  #[webidl(default = String::new())]
  empty: String,
}

#[derive(WebIDL, Debug, PartialEq)]
#[webidl(dictionary)]
struct Empty {}

#[derive(WebIDL, Debug, PartialEq)]
#[webidl(enum)]
enum Mode {
  FooBar,
  A,
  #[webidl(rename = "HTTP")]
  Http2,
}

/// Evaluates `code` as a script and returns its completion value.
fn eval<'a>(
  scope: &mut v8::HandleScope<'a>,
  code: &str,
) -> v8::Local<'a, v8::Value> {
  let source = v8::String::new(scope, code).unwrap();
  let script = v8::Script::compile(scope, source, None).unwrap();
  script.run(scope).unwrap()
}

/// Evaluates `code` and converts the result to `T`, returning the error text.
fn convert<'a, T: WebIdlConverter<'a>>(
  scope: &mut v8::HandleScope<'a>,
  code: &str,
) -> Result<T, String> {
  let value = eval(scope, code);
  T::convert(
    scope,
    value,
    Cow::Borrowed("Failed to execute 'op'"),
    || Cow::Borrowed("Argument 1"),
    &Default::default(),
  )
  .map_err(|e| e.to_string())
}

/// A dictionary value with every member present.
const FULL: &str = "({ fooBar: 1, withDefault: 2, maybe: 'x', renamed: 'y', \
  must: 3, clamped: 300, empty: 'z' })";

fn full_dict() -> Dict {
  Dict {
    foo_bar: 1,
    with_default: 2,
    maybe: Some("x".to_string()),
    other: "y".to_string(),
    must: Some(3),
    clamped: 255,
    empty: "z".to_string(),
  }
}

fn full_dict_with_no_maybe() -> Dict {
  Dict {
    maybe: None,
    clamped: 1,
    ..full_dict()
  }
}

#[test]
fn dictionary_reads_every_member() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert_eq!(convert::<Dict>(scope, FULL), Ok(full_dict()));
}

#[test]
fn dictionary_defaults_and_optional_members() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  // Missing members use their defaults, and a missing `Option` is `None`.
  // `null` is a present value: it converts, and for an `Option` it is `None`.
  assert_eq!(
    convert::<Dict>(scope, "({ fooBar: 1, renamed: 'y', must: null })"),
    Ok(Dict {
      foo_bar: 1,
      with_default: 7,
      maybe: None,
      other: "y".to_string(),
      must: None,
      clamped: 1,
      empty: String::new(),
    })
  );
}

#[test]
fn dictionary_missing_required_members_name_the_key() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let expected = |key: &str| {
    format!(
      "{OP} can not be converted to 'Dict' because '{key}' is required in 'Dict'"
    )
  };
  assert_eq!(
    convert::<Dict>(scope, "({ fooBar: undefined, renamed: 'y', must: 1 })"),
    Err(expected("fooBar"))
  );
  assert_eq!(
    convert::<Dict>(scope, "({ fooBar: 1, must: 1 })"),
    Err(expected("renamed"))
  );
  // `required` wins over `Option`: a missing optional member is still an error.
  assert_eq!(
    convert::<Dict>(scope, "({ fooBar: 1, renamed: 'y' })"),
    Err(expected("must"))
  );
}

#[test]
fn dictionary_null_or_undefined_makes_every_member_missing() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  for code in ["null", "undefined"] {
    assert_eq!(
      convert::<Dict>(scope, code),
      Err(format!(
        "{OP} can not be converted to 'Dict' because 'fooBar' is required in 'Dict'"
      )),
      "{code}"
    );
    assert_eq!(convert::<Empty>(scope, code), Ok(Empty {}), "{code}");
  }
}

#[test]
fn dictionary_rejects_non_objects() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let expected = format!("{OP} is not an object");
  for code in ["1", "'s'", "true", "10n", "Symbol('s')"] {
    assert_eq!(
      convert::<Dict>(scope, code),
      Err(expected.clone()),
      "{code}"
    );
    assert_eq!(
      convert::<Empty>(scope, code),
      Err(expected.clone()),
      "{code}"
    );
  }
}

#[test]
fn dictionary_reads_inherited_members() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let code = "Object.create({ fooBar: 1, renamed: 'a', must: 2 })";
  assert_eq!(
    convert::<Dict>(scope, code),
    Ok(Dict {
      foo_bar: 1,
      with_default: 7,
      maybe: None,
      other: "a".to_string(),
      must: Some(2),
      clamped: 1,
      empty: String::new(),
    })
  );
}

#[test]
fn dictionary_null_members_are_converted_not_missing() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let code = "({ fooBar: null, withDefault: null, renamed: null, must: null, \
    clamped: null, empty: null })";
  assert_eq!(
    convert::<Dict>(scope, code),
    Ok(Dict {
      foo_bar: 0,
      with_default: 0,
      maybe: None,
      other: "null".to_string(),
      must: None,
      clamped: 0,
      empty: String::new(),
    })
  );
}

#[test]
fn dictionary_clamps_members_with_options() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let code = "({ fooBar: 1, renamed: 'y', must: 1, clamped: -5 })";
  assert_eq!(convert::<Dict>(scope, code).map(|d| d.clamped), Ok(0));
  let code = "({ fooBar: 1, renamed: 'y', must: 1, clamped: 2.5 })";
  assert_eq!(convert::<Dict>(scope, code).map(|d| d.clamped), Ok(2));
}

#[test]
fn dictionary_reads_members_in_code_unit_order() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  // The fields are declared as foo_bar, with_default, maybe, renamed, must,
  // clamped, empty. Each getter records the key it was read with.
  let code = r"
    globalThis.log = [];
    const values = {
      fooBar: 1, withDefault: 2, maybe: undefined, renamed: 'y', must: 3,
      clamped: 1, empty: 'z',
    };
    const object = {};
    for (const key of Object.keys(values)) {
      Object.defineProperty(object, key, {
        enumerable: true,
        get() { log.push(key); return values[key]; },
      });
    }
    object;
  ";
  let value = eval(scope, code);
  let object = v8::Local::<v8::Object>::try_from(value).unwrap();
  let result = Dict::convert(
    scope,
    object.into(),
    Cow::Borrowed("Failed to execute 'op'"),
    || Cow::Borrowed("Argument 1"),
    &(),
  );
  assert_eq!(result, Ok(full_dict_with_no_maybe()));
  let log = eval(scope, "log.join(',')");
  assert_eq!(
    log.to_rust_string_lossy(scope),
    "clamped,empty,fooBar,maybe,must,renamed,withDefault"
  );
}

#[test]
fn dictionary_getter_exceptions_name_the_member() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let code =
    "({ get fooBar() { throw new Error('boom') }, renamed: 'y', must: 1 })";
  let error = convert::<Dict>(scope, code).unwrap_err();
  assert_eq!(
    error,
    "Failed to execute 'op': 'fooBar' of 'Dict' (Argument 1) threw an exception: Error: boom"
  );
}

#[test]
fn dictionary_member_conversion_failures_name_the_member() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let code =
    "({ fooBar: { valueOf() { throw new Error('boom') } }, renamed: 'y', must: 1 })";
  assert_eq!(
    convert::<Dict>(scope, code),
    Err(
      "Failed to execute 'op': 'fooBar' of 'Dict' (Argument 1) threw an exception: Error: boom"
        .to_string()
    )
  );
  let code = "({ fooBar: Symbol('s'), renamed: 'y', must: 1 })";
  assert_eq!(
    convert::<Dict>(scope, code),
    Err(
      "Failed to execute 'op': 'fooBar' of 'Dict' (Argument 1) is a Symbol and can not be converted to a number"
        .to_string()
    )
  );
}

#[test]
fn dictionary_engine_stays_usable_after_getter_throws() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let _ = convert::<Dict>(scope, "({ get fooBar() { throw 1 } })");
  let value = eval(scope, "2 + 2");
  assert_eq!(value.number_value(scope), Some(4.0));
}

#[test]
fn enum_matches_kebab_case_and_rename() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert_eq!(convert::<Mode>(scope, "'foo-bar'"), Ok(Mode::FooBar));
  assert_eq!(convert::<Mode>(scope, "'a'"), Ok(Mode::A));
  assert_eq!(convert::<Mode>(scope, "'HTTP'"), Ok(Mode::Http2));
}

#[test]
fn enum_matching_is_exact() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  let expected =
    |value: &str| format!("{OP} '{value}' is not a valid enum value of 'Mode'");
  for value in ["Foo-Bar", " foo-bar", "foo-bar ", "foo_bar", "http2"] {
    assert_eq!(
      convert::<Mode>(scope, &format!("'{value}'")),
      Err(expected(value)),
      "{value}"
    );
  }
  assert_eq!(
    convert::<Mode>(scope, "undefined"),
    Err(expected("undefined"))
  );
}

#[test]
fn enum_uses_to_string() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert_eq!(
    convert::<Mode>(scope, "({ toString() { return 'foo-bar' } })"),
    Ok(Mode::FooBar)
  );
}

#[test]
fn enum_rejects_symbols_and_reports_throws() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let scope = &mut runtime.handle_scope();
  assert_eq!(
    convert::<Mode>(scope, "Symbol('s')"),
    Err(format!(
      "{OP} is a Symbol and can not be converted to a string"
    ))
  );
  assert_eq!(
    convert::<Mode>(scope, "({ toString() { throw 1 } })"),
    Err(format!("{OP} threw an exception: 1"))
  );
}
