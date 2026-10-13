# `#[derive(WebIDL)]`

Derives `deno_core::webidl::WebIdlConverter` so a Rust type can be converted
from a JavaScript value following the WebIDL JS-to-IDL type mapping. It is
re-exported as `deno_core::WebIDL`.

## Dictionaries

```rust,ignore
#[derive(deno_core::WebIDL)]
#[webidl(dictionary)]
struct CreateOptions {
  // Key is the camelCase field name: "fooBar". Use `rename` for another key.
  foo_bar: u8,
  #[webidl(required)]
  name: String,
  #[webidl(rename = "mode", default = "r".to_string())]
  open_mode: String,
  // A missing or undefined `Option` member is `None`.
  timeout: Option<u32>,
  // `#[options]` sets fields of the member's `WebIdlConverter::Options`.
  #[options(clamp = true)]
  retries: u8,
  #[options(enforce_range = true)]
  port: Option<u16>,
}
```

- `undefined` and `null` convert to an empty dictionary. Any other non-object
  is an error.
- Members are read with one `[[Get]]` each, in lexicographic order of their
  keys, not declaration order.
- A missing member is an error unless it has a `default`, or it is an
  `Option` (which becomes `None`). `required` makes a missing member an error
  even for an `Option`.

## Enums

```rust,ignore
#[derive(deno_core::WebIDL)]
#[webidl(enum)]
enum Method {
  // Matches "foo-bar" (kebab-case of the variant name).
  FooBar,
  #[webidl(rename = "PATCH")]
  Patch,
}
```

The value is converted with ToString, then must match a variant exactly
(case-sensitive). A string that matches no variant, such as `"FooBar"` or
`""`, is an error that names the enum and the received value. A Symbol is
rejected with the Symbol message instead, and a `toString` that throws is
reported as that exception.
