//! File-description syntax detection and Rust and Python summary extraction
//! tests.

use super::FileDescriptionSyntax;

#[test]
fn from_path_maps_supported_extensions() {
    // Arrange
    let cases = [
        (
            "crates/agentty/src/main.rs",
            Some(FileDescriptionSyntax::Rust),
        ),
        ("scripts/tool.py", Some(FileDescriptionSyntax::Python)),
        ("stubs/tool.pyi", Some(FileDescriptionSyntax::Python)),
        ("README.md", None),
        ("Makefile", None),
    ];

    for (path, expected_syntax) in cases {
        // Act
        let syntax = FileDescriptionSyntax::from_path(path);

        // Assert
        assert_eq!(syntax, expected_syntax, "path: {path}");
    }
}

#[test]
fn describe_rust_returns_first_doc_paragraph() {
    // Arrange
    let cases = [
        (
            "//! Runtime event loop.\n\nuse std::io;\n",
            Some("Runtime event loop."),
        ),
        (
            "// License header.\n#![allow(dead_code)]\n\n//! # Agentty\n",
            Some("Agentty"),
        ),
        (
            "//!\n//! Wrapped summary\n//!   continues here.\n//!\n//! Details.\n",
            Some("Wrapped summary continues here."),
        ),
        (
            "/*\n * Copyright Example.\n * Licensed under MIT.\n */\n\n//! Licensed module.\n",
            Some("Licensed module."),
        ),
        (
            "/* SPDX-License-Identifier: MIT */\n//! Tagged module.\n",
            Some("Tagged module."),
        ),
        ("/*! Block docs. */\n//! Late.\n", None),
        ("//! Summary.\nfn main() {}\n", Some("Summary.")),
        ("/// Item docs.\nfn main() {}\n//! Late.\n", None),
        ("fn main() {}\n", None),
        ("", None),
    ];

    for (source, expected_description) in cases {
        // Act
        let description = FileDescriptionSyntax::Rust.describe(source);

        // Assert
        assert_eq!(
            description.as_deref(),
            expected_description,
            "source: {source:?}"
        );
    }
}

#[test]
fn describe_python_returns_docstring_summary_line() {
    // Arrange
    let cases = [
        (
            "#!/usr/bin/env python3\n# -*- coding: utf-8 -*-\n\"\"\"Build helpers.\"\"\"\n",
            Some("Build helpers."),
        ),
        (
            "\"\"\"\n\n    Command line   entry point.\n\nDetails.\n\"\"\"\n",
            Some("Command line entry point."),
        ),
        ("r'''Raw summary.\n'''\n", Some("Raw summary.")),
        ("'Single quoted.'\n", Some("Single quoted.")),
        ("\"\"\"\n\"\"\"\nimport os\n", None),
        ("\"\"\"Unterminated", Some("Unterminated")),
        ("\"\"\"\n", None),
        ("import os\n", None),
        ("# Only comments.\n", None),
    ];

    for (source, expected_description) in cases {
        // Act
        let description = FileDescriptionSyntax::Python.describe(source);

        // Assert
        assert_eq!(
            description.as_deref(),
            expected_description,
            "source: {source:?}"
        );
    }
}
