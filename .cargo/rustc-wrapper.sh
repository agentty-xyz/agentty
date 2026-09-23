#!/usr/bin/env bash
# Bash preserves Cargo environment names containing hyphens; dash drops them.
# Preserve Cargo's compiler arguments and failures, with an optional shared cache.
if command -v sccache >/dev/null 2>&1 && sccache --dist-status >/dev/null 2>&1; then
    exec sccache "$@"
fi
exec "$@"
