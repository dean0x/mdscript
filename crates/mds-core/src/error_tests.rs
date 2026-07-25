use super::*;

// ── compute_line_column ───────────────────────────────────────────────────

#[test]
fn line_col_first_byte() {
    // Offset 0 in any source is (1, 1).
    assert_eq!(compute_line_column("hello", 0), Some((1, 1)));
}

#[test]
fn line_col_mid_line() {
    // "hello world", offset 6 → 'w' → line 1, column 7.
    assert_eq!(compute_line_column("hello world", 6), Some((1, 7)));
}

#[test]
fn line_col_second_line() {
    // "line1\nline2" — '\n' at offset 5, 'l' of "line2" at offset 6.
    assert_eq!(compute_line_column("line1\nline2", 6), Some((2, 1)));
}

#[test]
fn line_col_third_line() {
    // "a\nb\nc" — 'a'=0, '\n'=1, 'b'=2, '\n'=3, 'c'=4 → offset 4 = (3,1).
    assert_eq!(compute_line_column("a\nb\nc", 4), Some((3, 1)));
}

#[test]
fn line_col_multibyte_utf8() {
    // "café\nworld": 'c'=0,'a'=1,'f'=2,'é'=3+4(2 bytes),'\n'=5,'w'=6
    // offset 6 → 'w' on line 2, col 1.
    let src = "café\nworld";
    assert_eq!(src.as_bytes()[6], b'w');
    assert_eq!(compute_line_column(src, 6), Some((2, 1)));
}

#[test]
fn line_col_at_newline() {
    // "ab\ncd" — '\n' is at offset 2, on line 1. col = 3 (after 'a','b').
    assert_eq!(compute_line_column("ab\ncd", 2), Some((1, 3)));
}

#[test]
fn line_col_out_of_bounds() {
    // Offset beyond source length must return None.
    assert_eq!(compute_line_column("short", 100), None);
}

#[test]
fn line_col_empty_source() {
    // Empty source, offset 0 is valid (at the very start).
    assert_eq!(compute_line_column("", 0), Some((1, 1)));
}

// ── MdsError::serialize — per-variant tests ───────────────────────────────

#[test]
fn serialize_syntax_with_span() {
    let e = MdsError::syntax_at("unexpected token", "file.mds", "hello world", 0, 5);
    let s = e.serialize();
    assert_eq!(s.code, "mds::syntax");
    assert_eq!(s.help, None);
    let span = s.span.expect("span should be Some");
    assert_eq!(span.offset, 0);
    assert_eq!(span.length, 5);
    assert_eq!(span.line, Some(1));
    assert_eq!(span.column, Some(1));
}

#[test]
fn serialize_syntax_without_span() {
    let e = MdsError::syntax("unexpected token");
    let s = e.serialize();
    assert_eq!(s.code, "mds::syntax");
    assert_eq!(s.help, None);
    assert_eq!(s.span, None);
}

#[test]
fn serialize_undefined_var_with_span() {
    // "{{ x }}" — 'x' is at offset 3, length 1.
    let e = MdsError::undefined_var_at("x", "f.mds", "{{ x }}", 3, 1);
    let s = e.serialize();
    assert_eq!(s.code, "mds::undefined_var");
    let help = s.help.expect("UndefinedVariable should have help text");
    assert!(
        help.contains("define"),
        "help should mention 'define', got: {help}"
    );
    let span = s.span.expect("span should be Some");
    assert_eq!(span.offset, 3);
    assert_eq!(span.length, 1);
    assert_eq!(span.line, Some(1));
    assert_eq!(span.column, Some(4)); // bytes 0,1,2 → col 4
}

#[test]
fn serialize_arity_code() {
    let e = MdsError::arity_at("greet", 1, 1, 3, "f.mds", "source text", 0, 6, None);
    let s = e.serialize();
    assert_eq!(s.code, "mds::arity");
    assert!(
        s.span.is_some(),
        "ArityMismatch with span should produce Some(span)"
    );
}

#[test]
fn serialize_type_error_with_help() {
    let e = MdsError::type_error_at("string", "f.mds", "source", 0, 6);
    let s = e.serialize();
    assert_eq!(s.code, "mds::type_error");
    assert!(s.help.is_some(), "TypeError should have help text");
    assert!(s.span.is_some());
}

#[test]
fn serialize_circular_import() {
    let e = MdsError::circular_import_at("a->b->a", "f.mds", "source", 0, 1);
    let s = e.serialize();
    assert_eq!(s.code, "mds::circular_import");
    assert!(s.help.is_some(), "CircularImport should have help text");
    assert!(s.span.is_some());
}

#[test]
fn serialize_file_not_found() {
    let e = MdsError::file_not_found_at("missing.mds", "f.mds", "source", 0, 1);
    let s = e.serialize();
    assert_eq!(s.code, "mds::file_not_found");
    assert!(s.help.is_some(), "FileNotFound should have help text");
    assert!(s.span.is_some());
}

#[test]
fn serialize_recursion() {
    let e = MdsError::recursion_at("fib", "f.mds", "source text", 0, 3);
    let s = e.serialize();
    assert_eq!(s.code, "mds::recursion");
    assert!(s.help.is_some(), "Recursion should have help text");
    assert!(s.span.is_some());
}

#[test]
fn serialize_not_mds_no_span() {
    let e = MdsError::not_mds_file("readme.txt");
    let s = e.serialize();
    assert_eq!(s.code, "mds::not_mds");
    assert!(s.help.is_some(), "NotMdsFile should have help text");
    assert_eq!(s.span, None);
}

#[test]
fn serialize_io_no_span() {
    let e = MdsError::io("permission denied");
    let s = e.serialize();
    assert_eq!(s.code, "mds::io");
    assert_eq!(s.help, None);
    assert_eq!(s.span, None);
}

#[test]
fn serialize_resource_limit_no_span() {
    let e = MdsError::resource_limit("too many iterations");
    let s = e.serialize();
    assert_eq!(s.code, "mds::resource_limit");
    assert_eq!(s.help, None);
    assert_eq!(s.span, None);
}

#[test]
fn serialize_yaml_error_no_span() {
    let e = MdsError::yaml_error("unexpected indent");
    let s = e.serialize();
    assert_eq!(s.code, "mds::yaml");
    assert_eq!(s.help, None);
    assert_eq!(s.span, None);
}

#[test]
fn serialize_json_error_no_span() {
    let e = MdsError::json_error("trailing comma");
    let s = e.serialize();
    assert_eq!(s.code, "mds::json");
    assert_eq!(s.help, None);
    assert_eq!(s.span, None);
}

// ── JSON serialization tests ──────────────────────────────────────────────

#[test]
fn serialized_error_to_json_with_span() {
    let e = MdsError::syntax_at("bad token", "file.mds", "hello world", 6, 5);
    let s = e.serialize();
    let json = serde_json::to_string(&s).expect("serialization should succeed");
    // Verify JSON structure contains expected keys.
    assert!(json.contains("\"code\""), "JSON should contain 'code' key");
    assert!(
        json.contains("\"message\""),
        "JSON should contain 'message' key"
    );
    assert!(json.contains("\"span\""), "JSON should contain 'span' key");
    assert!(
        json.contains("\"offset\""),
        "JSON should contain 'offset' key"
    );
    assert!(
        json.contains("\"length\""),
        "JSON should contain 'length' key"
    );
    assert!(json.contains("\"line\""), "JSON should contain 'line' key");
    assert!(
        json.contains("\"column\""),
        "JSON should contain 'column' key"
    );
    // Verify values are correct.
    let v: serde_json::Value = serde_json::from_str(&json).expect("JSON should parse back");
    assert_eq!(v["code"], "mds::syntax");
    assert_eq!(v["span"]["offset"], 6);
    assert_eq!(v["span"]["length"], 5);
    assert_eq!(v["span"]["line"], 1);
    assert_eq!(v["span"]["column"], 7); // offset 6 in "hello world" → col 7
}

#[test]
fn serialized_error_to_json_null_span() {
    let e = MdsError::io("disk full");
    let s = e.serialize();
    let json = serde_json::to_string(&s).expect("serialization should succeed");
    let v: serde_json::Value = serde_json::from_str(&json).expect("JSON should parse back");
    assert!(v["span"].is_null(), "span should be null in JSON when None");
}

// ── T-1..T-3: serialize() sanitizes control chars (issue #176 / ESC-INJECTION) ──

/// T-1 [AC-F3]: serialize() sanitizes raw ESC (U+001B) in the message field.
/// The message string must not contain the raw ESC byte; the sanitized 6-char
/// literal `` must appear instead.
#[test]
fn serialize_sanitizes_esc_in_message() {
    // Build a Syntax error whose message embeds a raw ESC byte mid-string.
    let e = MdsError::syntax("bad\x1Btoken");
    let s = e.serialize();
    assert!(
        !s.message.contains('\x1B'),
        "raw ESC byte must not appear in serialized message; got: {:?}",
        s.message
    );
    assert!(
        s.message.contains("\\u001B"),
        "sanitized literal \\u001B must appear in message; got: {:?}",
        s.message
    );
}

/// T-2 [AC-F3]: serialize() sanitizes raw ESC in both message and help fields.
/// UndefinedVariable embeds `name` in both the message ("undefined variable 'name'")
/// and the help ("define 'name' in frontmatter or imports").
#[test]
fn serialize_sanitizes_esc_in_help() {
    // The name `a\x1Bb` embeds an ESC byte — miette uses it in both fields.
    let e = MdsError::undefined_var("a\x1Bb");
    let s = e.serialize();
    // Message must be sanitized.
    assert!(
        !s.message.contains('\x1B'),
        "raw ESC byte must not appear in serialized message; got: {:?}",
        s.message
    );
    assert!(
        s.message.contains("\\u001B"),
        "sanitized literal \\u001B must appear in message; got: {:?}",
        s.message
    );
    // Help must also be sanitized.
    let help = s.help.expect("UndefinedVariable should carry help text");
    assert!(
        !help.contains('\x1B'),
        "raw ESC byte must not appear in serialized help; got: {:?}",
        help
    );
    assert!(
        help.contains("\\u001B"),
        "sanitized literal \\u001B must appear in help; got: {:?}",
        help
    );
}

/// T-3 [AC-F3]: serialize() sanitizes DEL (U+007F) and C1 NEL (U+0085) in addition
/// to ESC, producing the corresponding `\uXXXX` literals.
#[test]
fn serialize_sanitizes_del_and_c1() {
    let e = MdsError::syntax("del\u{007F}and\u{0085}nel");
    let s = e.serialize();
    // Raw DEL byte must be sanitized.
    assert!(
        !s.message.contains('\u{007F}'),
        "raw DEL must not appear in serialized message; got: {:?}",
        s.message
    );
    assert!(
        s.message.contains("\\u007F"),
        "sanitized \\u007F must appear in message; got: {:?}",
        s.message
    );
    // Raw C1 NEL (U+0085) must be sanitized.
    assert!(
        !s.message.contains('\u{0085}'),
        "raw C1 NEL must not appear in serialized message; got: {:?}",
        s.message
    );
    assert!(
        s.message.contains("\\u0085"),
        "sanitized \\u0085 must appear in message; got: {:?}",
        s.message
    );
}

/// T-3b [AC-F3]: `serialize()` escapes the widened class — bidi overrides
/// (Trojan Source, CVE-2021-42574), U+2028/U+2029, and U+FEFF — none of which
/// are C0/DEL/C1 and all of which previously passed straight through.
#[test]
fn serialize_sanitizes_bidi_separators_and_bom() {
    let e = MdsError::syntax("rlo\u{202E}iso\u{2066}ls\u{2028}ps\u{2029}bom\u{FEFF}end");
    let s = e.serialize();
    for (ch, escaped) in [
        ('\u{202E}', "\\u202E"),
        ('\u{2066}', "\\u2066"),
        ('\u{2028}', "\\u2028"),
        ('\u{2029}', "\\u2029"),
        ('\u{FEFF}', "\\uFEFF"),
    ] {
        assert!(
            !s.message.contains(ch),
            "raw U+{:04X} must not appear in serialized message; got: {:?}",
            ch as u32,
            s.message
        );
        assert!(
            s.message.contains(escaped),
            "sanitized {escaped} must appear in message; got: {:?}",
            s.message
        );
    }
    // Non-vacuity: the surrounding prose survives.
    assert!(
        s.message.contains("rlo") && s.message.contains("end"),
        "clean text must be preserved; got: {:?}",
        s.message
    );
}

/// T-3c [AC-F3]: `serialize()` is a WIRE boundary — an embedded newline (U+000A)
/// becomes its 6-char escape literal so a hostile message cannot forge an extra
/// line in a line-oriented consumer of `SerializedError.message`.
#[test]
fn serialize_escapes_newline_on_the_wire() {
    let e = MdsError::syntax("a\nerror[mds::forged]: FAKE\nb");
    let s = e.serialize();
    assert!(
        !s.message.contains('\n'),
        "raw newline must not appear in serialized message; got: {:?}",
        s.message
    );
    assert!(
        s.message.contains("\\u000A"),
        "sanitized \\u000A must appear in serialized message; got: {:?}",
        s.message
    );
    // Non-vacuity: the message body itself is untouched.
    assert!(
        s.message.contains("error[mds::forged]"),
        "message body must be preserved verbatim; got: {:?}",
        s.message
    );
}

// ── display_sanitized() ───────────────────────────────────────────────────

/// T-DS: `display_sanitized()` escapes raw ESC (U+001B) bytes in the terminal-
/// safe output while `to_string()` / `Display` leaves them raw.
///
/// This test is the regression anchor for rust-5/architecture-2 (PF-004 on
/// the published API, CWE-150 / issue #176).  It FAILS if `display_sanitized()`
/// is removed or reverted to a bare `self.to_string()` call (avoids PF-013).
#[test]
fn display_sanitized_escapes_esc_byte() {
    let e = MdsError::syntax("bad\x1Btoken");
    let displayed = e.display_sanitized();
    assert!(
        !displayed.contains('\x1B'),
        "raw ESC byte must not appear in display_sanitized(); got: {:?}",
        displayed
    );
    // Positive assertion — FAILS if display_sanitized() reverts to to_string().
    assert!(
        displayed.contains("\\u001B"),
        "sanitized literal \\u001B must appear in display_sanitized(); got: {:?}",
        displayed
    );
}

/// T-DS-BIDI: `display_sanitized()` also covers the widened class — a bidi
/// override reaching a TTY reverses the visible order of the rest of the line.
#[test]
fn display_sanitized_escapes_bidi_override() {
    let e = MdsError::syntax("bad\u{202E}token");
    let displayed = e.display_sanitized();
    assert!(
        !displayed.contains('\u{202E}'),
        "raw U+202E must not appear in display_sanitized(); got: {displayed:?}"
    );
    assert!(
        displayed.contains("\\u202E"),
        "sanitized literal \\u202E must appear in display_sanitized(); got: {displayed:?}"
    );
}

/// T-DS-NL: `display_sanitized()` is the HUMAN boundary — newlines stay raw so
/// multi-line miette frames remain readable. This is the deliberate asymmetry
/// with `serialize()` (see T-3c); pinning it here prevents an accidental
/// "sanitize everything the same way" regression.
#[test]
fn display_sanitized_preserves_newline() {
    let e = MdsError::syntax("line one\nline two");
    let displayed = e.display_sanitized();
    assert!(
        displayed.contains('\n'),
        "display_sanitized() must preserve raw newlines; got: {displayed:?}"
    );
    assert!(
        !displayed.contains("\\u000A"),
        "display_sanitized() must not escape newlines; got: {displayed:?}"
    );
}

/// `display_sanitized()` and `to_string()` differ on ESC-bearing input, proving
/// that `display_sanitized()` is not a trivial alias for the raw Display impl.
#[test]
fn display_sanitized_differs_from_to_string_on_esc() {
    let e = MdsError::syntax("msg\x1Bend");
    // Raw Display keeps the ESC byte.
    assert!(
        e.to_string().contains('\x1B'),
        "to_string() must keep raw ESC (display contract); got: {:?}",
        e.to_string()
    );
    // Sanitized form must not.
    assert!(
        !e.display_sanitized().contains('\x1B'),
        "display_sanitized() must not keep raw ESC; got: {:?}",
        e.display_sanitized()
    );
}

// ── Display output ────────────────────────────────────────────────────────

#[test]
fn syntax_display_contains_message() {
    let e = MdsError::syntax("unexpected token '}'");
    assert!(e.to_string().contains("unexpected token '}'"));
}

#[test]
fn undefined_var_display_contains_name() {
    let e = MdsError::undefined_var("my_var");
    assert!(e.to_string().contains("my_var"));
}

#[test]
fn undefined_fn_display_contains_name() {
    let e = MdsError::undefined_fn("my_fn");
    assert!(e.to_string().contains("my_fn"));
}

#[test]
fn arity_display_contains_name_and_counts() {
    let e = MdsError::arity("greet", 1, 1, 3, None);
    let msg = e.to_string();
    assert!(msg.contains("greet"));
    assert!(msg.contains('1'));
    assert!(msg.contains('3'));
}

#[test]
fn arity_display_singular_argument() {
    let e = MdsError::arity("f", 1, 1, 0, None);
    assert!(
        e.to_string().contains("argument"),
        "should say 'argument' not 'arguments' for 1"
    );
}

#[test]
fn arity_display_plural_arguments() {
    let e = MdsError::arity("f", 2, 2, 0, None);
    assert!(
        e.to_string().contains("arguments"),
        "should say 'arguments' for 2"
    );
}

#[test]
fn arity_display_range() {
    let e = MdsError::arity("f", 1, 3, 0, None);
    let msg = e.to_string();
    assert!(
        msg.contains("1-3"),
        "should display range '1-3' for min=1 max=3, got: {msg}"
    );
}

#[test]
fn builtin_error_display_and_serialize() {
    let e = MdsError::builtin_error("upper() requires a string argument, got number");
    let msg = e.to_string();
    assert!(
        msg.contains("upper()"),
        "builtin error should contain the message, got: {msg}"
    );
    let s = e.serialize();
    assert_eq!(s.code, "mds::builtin");
    assert!(
        s.span.is_none(),
        "builtin error without span should have None span"
    );
}

#[test]
fn type_error_display_contains_got() {
    let e = MdsError::type_error("string");
    assert!(e.to_string().contains("string"));
}

#[test]
fn circular_import_display_contains_cycle() {
    let e = MdsError::circular_import("a -> b -> a");
    assert!(e.to_string().contains("a -> b -> a"));
}

#[test]
fn file_not_found_display_contains_path() {
    let e = MdsError::file_not_found("foo/bar.mds");
    assert!(e.to_string().contains("foo/bar.mds"));
}

#[test]
fn recursion_display_contains_name() {
    let e = MdsError::recursion("fib");
    assert!(e.to_string().contains("fib"));
}

#[test]
fn io_display_contains_message() {
    let e = MdsError::io("permission denied");
    assert!(e.to_string().contains("permission denied"));
}

#[test]
fn yaml_error_display_contains_message() {
    let e = MdsError::yaml_error("unexpected indent");
    assert!(e.to_string().contains("unexpected indent"));
}

#[test]
fn json_error_display_contains_message() {
    let e = MdsError::json_error("trailing comma");
    assert!(e.to_string().contains("trailing comma"));
}

#[test]
fn not_mds_file_display_contains_path() {
    let e = MdsError::not_mds_file("readme.txt");
    assert!(e.to_string().contains("readme.txt"));
}

#[test]
fn resource_limit_display_contains_message() {
    let e = MdsError::resource_limit("too many iterations");
    assert!(e.to_string().contains("too many iterations"));
}

// ── Span propagation via _at constructors ─────────────────────────────────

#[test]
fn syntax_at_populates_span_and_src() {
    let e = MdsError::syntax_at("bad token", "file.mds", "hello world", 0, 5);
    match e {
        MdsError::Syntax { span, src, .. } => {
            assert!(span.is_some(), "span should be populated");
            assert!(src.is_some(), "src should be populated");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn undefined_var_at_populates_span() {
    let e = MdsError::undefined_var_at("x", "f.mds", "{{ x }}", 3, 1);
    match e {
        MdsError::UndefinedVariable { span, .. } => {
            assert!(span.is_some());
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn type_error_at_populates_span() {
    let e = MdsError::type_error_at("string", "f.mds", "source", 0, 6);
    match e {
        MdsError::TypeError { span, .. } => {
            assert!(span.is_some());
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn recursion_at_populates_span() {
    let e = MdsError::recursion_at("fib", "f.mds", "source", 0, 3);
    match e {
        MdsError::Recursion { span, .. } => {
            assert!(span.is_some());
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn circular_import_at_populates_span() {
    let e = MdsError::circular_import_at("a->b->a", "f.mds", "source", 0, 1);
    match e {
        MdsError::CircularImport { span, .. } => {
            assert!(span.is_some());
        }
        _ => panic!("wrong variant"),
    }
}

// ── No-span constructors leave span as None ───────────────────────────────

#[test]
fn syntax_without_at_has_no_span() {
    let e = MdsError::syntax("msg");
    match e {
        MdsError::Syntax { span, src, .. } => {
            assert!(span.is_none());
            assert!(src.is_none());
        }
        _ => panic!("wrong variant"),
    }
}

// ── serialize() — remaining span-bearing variants ─────────────────────────

#[test]
fn serialize_undefined_fn_with_span() {
    // "{{ greet() }}" — 'g' of "greet" is at offset 3, length 5.
    let e = MdsError::undefined_fn_at("greet", "f.mds", "{{ greet() }}", 3, 5);
    let s = e.serialize();
    assert_eq!(s.code, "mds::undefined_fn");
    let help = s.help.expect("UndefinedFunction should have help text");
    assert!(
        help.contains("define"),
        "help should mention 'define', got: {help}"
    );
    let span = s.span.expect("span should be Some");
    assert_eq!(span.offset, 3);
    assert_eq!(span.length, 5);
    assert_eq!(span.line, Some(1));
    assert_eq!(span.column, Some(4)); // bytes 0,1,2 → col 4
}

#[test]
fn serialize_import_error_with_span() {
    // "import x" — 'x' at offset 7, length 1.
    let e = MdsError::import_error_at("could not resolve", "f.mds", "import x", 7, 1);
    let s = e.serialize();
    assert_eq!(s.code, "mds::import");
    assert_eq!(s.help, None, "ImportError has no help text");
    let span = s.span.expect("span should be Some");
    assert_eq!(span.offset, 7);
    assert_eq!(span.length, 1);
    assert_eq!(span.line, Some(1));
    assert_eq!(span.column, Some(8)); // bytes 0..6 → col 8
}

#[test]
fn serialize_name_collision_with_span() {
    // "foo" redefined at offset 0, length 3.
    let e = MdsError::name_collision_at("foo", "f.mds", "foo = 1", 0, 3);
    let s = e.serialize();
    assert_eq!(s.code, "mds::name_collision");
    assert_eq!(s.help, None, "NameCollision has no help text");
    let span = s.span.expect("span should be Some");
    assert_eq!(span.offset, 0);
    assert_eq!(span.length, 3);
    assert_eq!(span.line, Some(1));
    assert_eq!(span.column, Some(1));
}

// ── serialize() — ExportError ─────────────────────────────────────────────

#[test]
fn serialize_export_error_with_span() {
    // Export statement at offset 0, length 6.
    let e = MdsError::export_error_at("invalid export target", "f.mds", "export foo", 0, 6);
    let s = e.serialize();
    assert_eq!(s.code, "mds::export");
    assert_eq!(s.help, None, "ExportError has no help text");
    let span = s.span.expect("span should be Some");
    assert_eq!(span.offset, 0);
    assert_eq!(span.length, 6);
    assert_eq!(span.line, Some(1));
    assert_eq!(span.column, Some(1));
}

// ── serialize() — span=Some but src=None ──────────────────────────────────

#[test]
fn serialize_span_some_src_none_omits_line_column() {
    // Construct Syntax directly with span set but src intentionally None,
    // matching the documented behavior in serialize()'s doc comment.
    let e = MdsError::Syntax {
        message: "bad token".to_string(),
        span: Some(miette::SourceSpan::new(10.into(), 3)),
        src: None,
    };
    let s = e.serialize();
    assert_eq!(s.code, "mds::syntax");
    let span = s.span.expect("span should be Some when SourceSpan is set");
    assert_eq!(span.offset, 10);
    assert_eq!(span.length, 3);
    // Without src there is no source text to compute line/column from.
    assert_eq!(span.line, None, "line should be None when src is None");
    assert_eq!(span.column, None, "column should be None when src is None");
}

// ── compute_line_column — boundary: offset == source.len() ───────────────

#[test]
fn line_col_at_end_of_source() {
    // "abc" has len 3. Offset 3 is one past the last byte — still valid
    // (offset == len is the exclusive-end sentinel, not out-of-bounds).
    assert_eq!(compute_line_column("abc", 3), Some((1, 4)));
}

// ── compute_line_column — char-based column (C3, C5) ─────────────────────

#[test]
fn compute_line_column_is_char_based() {
    // "日本語\n!" — each CJK char is 3 bytes; offset 6 points to '語' (the 3rd char).
    // Byte-based col would be 7 (6 bytes consumed + 1); char-based col is 3 (2 chars before).
    let src = "日本語\n!";
    let (line, col) = compute_line_column(src, 6).expect("offset 6 should be valid");
    assert_eq!(line, 1, "char-based: line should be 1");
    assert_eq!(
        col, 3,
        "char-based: col should be 3 (two chars before: 日, 本), not 7 (byte-based)"
    );

    // Start-of-line after multibyte: col is 1 regardless of byte/char mode.
    assert_eq!(compute_line_column("café\nworld", 6), Some((2, 1)));

    // Out-of-bounds still returns None.
    assert_eq!(compute_line_column("short", 100), None);
}
